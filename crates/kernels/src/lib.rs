//! Deterministic CPU workloads ("kernels") used by hwbench.
//!
//! Every chunk of work is derived from `(seed, kernel, index)` and produces a
//! 32-byte digest. The agent runs chunks natively and reports the digests; the
//! server re-runs a random sample of chunks from this same crate compiled to
//! WebAssembly and rejects the run if any digest differs. This proves the work
//! was actually performed for the server-issued seed. It does **not** prove how
//! long the work took — timing plausibility is checked server-side.
//!
//! Each CPU test runs under its own seed, [`test_seed`]`(seed, threads)`, so the
//! chunks computed for one test can't be reported again as another test's.
//!
//! Outputs must be bit-identical on every platform, so kernels avoid anything
//! platform-dependent: no `f32`/`f64` transcendental functions, no FMA, no
//! hash-map iteration order, no `usize`-width-dependent arithmetic.

use sha2::{Digest as _, Sha256};

mod apps;

/// Bump whenever any kernel's output for a given input changes, or the way seeds are derived.
/// v3: every CPU test runs under [`test_seed`] instead of the session seed.
pub const KERNELS_VERSION: u32 = 3;

/// The seed a CPU test with `threads` workers runs under, derived from the session seed.
pub fn test_seed(seed: &[u8; 32], threads: u32) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(seed);
    h.update(b"hwbench-cpu-test-v1");
    h.update(&threads.to_le_bytes());
    *h.finalize().as_bytes()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kernel {
    /// 11 chained SHA-256 passes over 1 MiB of pseudo-random data (integer/crypto throughput).
    Hash = 0,
    /// DEFLATE level 6 of 192 KiB of semi-compressible text (branchy integer code).
    Compress = 1,
    /// Unstable sort of 512 Ki u64 values (memory access + branch prediction).
    Sort = 2,
    /// 128×128 f64 matrix multiply, 36 rounds (floating point throughput).
    Float = 3,
    /// N-body style particle step using only +, -, *, / and sqrt (mixed FP).
    Physics = 4,
    /// Alpha-beta search of Othello positions (game AI; unpredictable branches).
    Search = 5,
    /// Bytecode interpreter running a small program (scripting engines; dispatch).
    Script = 6,
    /// Path tracer rendering a small scene (rendering; irregular floating point).
    Render = 7,
    /// AES-256-GCM encryption (TLS, disk encryption; crypto instructions where present).
    Crypto = 8,
    /// Generating and parsing JSON-like text (browsers, APIs, config files).
    Parse = 9,
}

pub const ALL_KERNELS: [Kernel; 10] = [
    Kernel::Hash, Kernel::Compress, Kernel::Sort, Kernel::Float, Kernel::Physics,
    Kernel::Search, Kernel::Script, Kernel::Render, Kernel::Crypto, Kernel::Parse,
];

impl Kernel {
    pub fn from_u32(v: u32) -> Option<Kernel> {
        ALL_KERNELS.get(v as usize).copied()
    }
    pub fn name(self) -> &'static str {
        match self {
            Kernel::Hash => "hash",
            Kernel::Compress => "compress",
            Kernel::Sort => "sort",
            Kernel::Float => "float",
            Kernel::Physics => "physics",
            Kernel::Search => "search",
            Kernel::Script => "script",
            Kernel::Render => "render",
            Kernel::Crypto => "crypto",
            Kernel::Parse => "parse",
        }
    }
    pub fn from_name(s: &str) -> Option<Kernel> {
        ALL_KERNELS.iter().copied().find(|k| k.name() == s)
    }
}

/// xoshiro256** seeded from a BLAKE3 derivation of (seed, kernel, index).
pub struct Rng([u64; 4]);

impl Rng {
    pub fn derive(seed: &[u8; 32], kernel: Kernel, index: u32) -> Rng {
        let mut h = blake3::Hasher::new_keyed(seed);
        h.update(b"hwbench-chunk-v1");
        h.update(&(kernel as u32).to_le_bytes());
        h.update(&index.to_le_bytes());
        let out = h.finalize();
        let b = out.as_bytes();
        let mut s = [0u64; 4];
        for (i, w) in s.iter_mut().enumerate() {
            *w = u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        }
        if s == [0; 4] {
            s[0] = 1;
        }
        Rng(s)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }
    /// Uniform f64 in [0, 1) from the top 53 bits.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

/// Run one chunk and return its digest.
pub fn run_chunk(seed: &[u8; 32], kernel: Kernel, index: u32) -> [u8; 32] {
    let mut rng = Rng::derive(seed, kernel, index);
    let mut out = blake3::Hasher::new();
    out.update(&(kernel as u32).to_le_bytes());
    out.update(&index.to_le_bytes());
    match kernel {
        Kernel::Hash => hash(&mut rng, &mut out),
        Kernel::Compress => compress(&mut rng, &mut out),
        Kernel::Sort => sort(&mut rng, &mut out),
        Kernel::Float => float(&mut rng, &mut out),
        Kernel::Physics => physics(&mut rng, &mut out),
        Kernel::Search => apps::search(&mut rng, &mut out),
        Kernel::Script => apps::script(&mut rng, &mut out),
        Kernel::Render => apps::render(&mut rng, &mut out),
        Kernel::Crypto => apps::crypto(&mut rng, &mut out),
        Kernel::Parse => apps::parse(&mut rng, &mut out),
    }
    *out.finalize().as_bytes()
}

fn hash(rng: &mut Rng, out: &mut blake3::Hasher) {
    let mut buf = vec![0u8; 1 << 20];
    rng.fill_bytes(&mut buf);
    // Chain several passes so the result depends on every byte, every pass.
    let mut d = Sha256::digest(&buf);
    for i in 0..10 {
        buf[i * 32..i * 32 + 32].copy_from_slice(&d);
        d = Sha256::digest(&buf);
    }
    out.update(&d);
}

pub(crate) const WORDS: [&str; 32] = [
    "the", "benchmark", "processor", "memory", "of", "and", "graphics", "storage", "cache", "thread",
    "a", "to", "in", "latency", "bandwidth", "core", "clock", "frame", "is", "for", "with", "score",
    "drive", "speed", "data", "on", "random", "sequential", "write", "read", "fast", "test",
];

// Kernel code defines proof digests the server verifies: don't restyle it.
#[allow(clippy::manual_is_multiple_of)]
fn compress(rng: &mut Rng, out: &mut blake3::Hasher) {
    // Text-like input from a small vocabulary plus random noise bytes so the
    // compressor exercises both matching and literal paths.
    let target = 192 * 1024;
    let mut text = Vec::with_capacity(target + 32);
    while text.len() < target {
        let r = rng.next_u64();
        if r % 17 == 0 {
            text.push((r >> 8) as u8);
        } else {
            text.extend_from_slice(WORDS[(r >> 32) as usize % WORDS.len()].as_bytes());
            text.push(if r % 11 == 0 { b'\n' } else { b' ' });
        }
    }
    text.truncate(target);
    let compressed = miniz_oxide::deflate::compress_to_vec(&text, 6);
    out.update(&(compressed.len() as u64).to_le_bytes());
    out.update(&compressed);
}

fn sort(rng: &mut Rng, out: &mut blake3::Hasher) {
    let n = 512 * 1024;
    let mut v: Vec<u64> = (0..n).map(|_| rng.next_u64()).collect();
    v.sort_unstable();
    // Digest a strided sample plus a full xor-fold (sorted output is unique, so
    // any correct sort algorithm gives the same digest).
    let mut fold = 0u64;
    for (i, x) in v.iter().enumerate() {
        fold = fold.rotate_left(1) ^ x.wrapping_mul(i as u64 | 1);
    }
    out.update(&fold.to_le_bytes());
    for x in v.iter().step_by(1024) {
        out.update(&x.to_le_bytes());
    }
}

fn float(rng: &mut Rng, out: &mut blake3::Hasher) {
    const N: usize = 128;
    let mut a = vec![0f64; N * N];
    let mut b = vec![0f64; N * N];
    for x in a.iter_mut().chain(b.iter_mut()) {
        *x = rng.next_f64() * 2.0 - 1.0;
    }
    let mut c = vec![0f64; N * N];
    for _round in 0..36 {
        for i in 0..N {
            for k in 0..N {
                let aik = a[i * N + k];
                for j in 0..N {
                    c[i * N + j] += aik * b[k * N + j];
                }
            }
        }
        // Normalize to keep values bounded, then feed back.
        let mut m = 0f64;
        for x in &c {
            let ax = if *x < 0.0 { -*x } else { *x };
            if ax > m {
                m = ax;
            }
        }
        let inv = 1.0 / m;
        for (dst, src) in a.iter_mut().zip(c.iter()) {
            *dst = *src * inv;
        }
    }
    for x in c.iter().step_by(7) {
        out.update(&x.to_bits().to_le_bytes());
    }
}

fn physics(rng: &mut Rng, out: &mut blake3::Hasher) {
    const P: usize = 512;
    let mut pos = vec![[0f64; 3]; P];
    let mut vel = vec![[0f64; 3]; P];
    for p in pos.iter_mut() {
        for c in p.iter_mut() {
            *c = rng.next_f64() * 100.0;
        }
    }
    let dt = 0.01;
    for _step in 0..18 {
        for i in 0..P {
            let mut acc = [0f64; 3];
            let pi = pos[i];
            for (j, pj) in pos.iter().enumerate() {
                if i == j {
                    continue;
                }
                let dx = pj[0] - pi[0];
                let dy = pj[1] - pi[1];
                let dz = pj[2] - pi[2];
                let d2 = dx * dx + dy * dy + dz * dz + 0.01;
                let inv = 1.0 / (d2 * d2.sqrt());
                acc[0] += dx * inv;
                acc[1] += dy * inv;
                acc[2] += dz * inv;
            }
            for c in 0..3 {
                vel[i][c] += acc[c] * dt;
            }
        }
        for i in 0..P {
            for c in 0..3 {
                pos[i][c] += vel[i][c] * dt;
            }
        }
    }
    for p in pos.iter().step_by(4) {
        for c in p {
            out.update(&c.to_bits().to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let seed = [7u8; 32];
        for k in ALL_KERNELS {
            assert_eq!(run_chunk(&seed, k, 3), run_chunk(&seed, k, 3), "{k:?}");
            assert_ne!(run_chunk(&seed, k, 3), run_chunk(&seed, k, 4), "{k:?}");
        }
    }

    /// Golden digests: if this fails, outputs changed — bump KERNELS_VERSION.
    #[test]
    fn golden() {
        let seed = [1u8; 32];
        let got: Vec<String> = ALL_KERNELS.iter().map(|k| hex(&run_chunk(&seed, *k, 0))).collect();
        let want = [
            "fb3dc71deab7f0dd0b430eae9833c5ecbb8dae23457ccc06ade1c27138002065",
            "bc6635655f6dbe08a33991f143bc1251af4c8d5536bdb987c8bcbb7f3814c669",
            "d01440c11cd9a160569291deda702a6ed2cae750573f77a6a6fb1fec8d7f3454",
            "63d41430d4e43fa3e41f75b6751459e09cd85ce6292614ff42dd24a3462f5944",
            "ebcf7400fd23c6834b9fdbea00ffebdfc33c4625f674dff35be0be1343be16ff",
            "7e29ea669815ac7e1622a38bd0fadce45da40773f138455353140c7198ac7c79",
            "3cdaa2f430b535d5e6c2e9a5dd6cf34710f704b6741469293be7a07f41187696",
            "effd68b5a1906cd89ef4cb3a7163e4f0e18bd7666a00656374ebb70322717e0b",
            "1d0e4a42acf6838f6b1c0b7f5f6d4b1eee0084fcff5577791e5415ff972285b9",
            "f99ede5c859039a1d42231b229c341fe8b0dfb71af6f78e60296cd949091b460",
        ];
        assert_eq!(got, want);
    }

    /// Golden test seeds: the server's verifier derives these too.
    #[test]
    fn test_seeds() {
        let seed = [1u8; 32];
        assert_eq!(hex(&test_seed(&seed, 1)), "2ecc3fdab48979d635a8b6ae2db204521910c899428169fc7c45db1bc629102d");
        assert_eq!(hex(&test_seed(&seed, 4)), "9cc087b23214d0cffb7f011fc920a6f4084bf22aabe68f5b7863ee0ad7d5980e");
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
