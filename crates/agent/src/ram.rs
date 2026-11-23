use hwbench_kernels::{Kernel, Rng};
use serde::Serialize;
use std::hint::black_box;
use std::time::Instant;

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RamResult {
    #[serde(rename = "readGBs")]
    pub read_gbs: f64,
    #[serde(rename = "writeGBs")]
    pub write_gbs: f64,
    #[serde(rename = "copyGBs")]
    pub copy_gbs: f64,
    pub latency_ns: f64,
    pub buffer_bytes: u64,
    pub threads: usize,
}

const MIB: usize = 1 << 20;

pub fn run(seed: &[u8; 32], total_mem: u64, threads: usize) -> RamResult {
    // Large enough to defeat any cache (incl. 3D V-Cache), small enough to be safe.
    let bytes = (512 * MIB).min((total_mem / 8) as usize).max(64 * MIB);
    let words = bytes / 8;
    let mut a = vec![1u64; words];
    let mut b = vec![0u64; words];

    let read = best_of(3, || {
        let t = Instant::now();
        par_chunks(&a, threads, |c| {
            black_box(c.iter().fold(0u64, |acc, x| acc.wrapping_add(*x)));
        });
        t.elapsed().as_secs_f64()
    });
    let write = best_of(3, || {
        let t = Instant::now();
        par_chunks_mut(&mut b, threads, |c| c.fill(black_box(7)));
        t.elapsed().as_secs_f64()
    });
    let copy = best_of(3, || {
        let t = Instant::now();
        let per = words.div_ceil(threads);
        std::thread::scope(|s| {
            for (dst, src) in b.chunks_mut(per).zip(a.chunks(per)) {
                s.spawn(move || {
                    crate::qos::full_speed_thread();
                    dst.copy_from_slice(src)
                });
            }
        });
        t.elapsed().as_secs_f64()
    });
    black_box(&mut a);

    RamResult {
        read_gbs: bytes as f64 / read / 1e9,
        write_gbs: bytes as f64 / write / 1e9,
        // copy moves the buffer twice (read + write)
        copy_gbs: 2.0 * bytes as f64 / copy / 1e9,
        latency_ns: latency(seed, bytes.min(256 * MIB)),
        buffer_bytes: bytes as u64,
        threads,
    }
}

/// Dependent loads around a random cycle of cache lines (Sattolo's algorithm),
/// so neither the prefetcher nor out-of-order execution can hide latency.
fn latency(seed: &[u8; 32], bytes: usize) -> f64 {
    const LINE_WORDS: usize = 8; // 64-byte lines
    let lines = bytes / 64;
    let mut order: Vec<u32> = (0..lines as u32).collect();
    let mut rng = Rng::derive(seed, Kernel::Sort, u32::MAX);
    for i in (1..lines).rev() {
        let j = (rng.next_u64() % i as u64) as usize;
        order.swap(i, j);
    }
    let mut buf = vec![0u32; lines * LINE_WORDS * 2]; // u32 slots, 16 per line
    for w in 0..lines {
        let from = order[w] as usize;
        let to = order[(w + 1) % lines] as usize;
        buf[from * 16] = to as u32;
    }
    drop(order);
    let hops = 8_000_000usize;
    let mut p = 0usize;
    for _ in 0..200_000 {
        p = buf[p * 16] as usize; // warm TLB / page in
    }
    let t = Instant::now();
    for _ in 0..hops {
        p = buf[p * 16] as usize;
    }
    let ns = t.elapsed().as_nanos() as f64 / hops as f64;
    black_box(p);
    ns
}

fn best_of(n: usize, mut f: impl FnMut() -> f64) -> f64 {
    (0..n).map(|_| f()).fold(f64::INFINITY, f64::min)
}

fn par_chunks(data: &[u64], threads: usize, f: impl Fn(&[u64]) + Sync) {
    let per = data.len().div_ceil(threads);
    std::thread::scope(|s| {
        for c in data.chunks(per) {
            let f = &f;
            s.spawn(move || {
                crate::qos::full_speed_thread();
                f(c)
            });
        }
    });
}

fn par_chunks_mut(data: &mut [u64], threads: usize, f: impl Fn(&mut [u64]) + Sync) {
    let per = data.len().div_ceil(threads);
    std::thread::scope(|s| {
        for c in data.chunks_mut(per) {
            let f = &f;
            s.spawn(move || {
                crate::qos::full_speed_thread();
                f(c)
            });
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn payload_keys_match_protocol() {
        let r = super::RamResult { read_gbs: 1.0, write_gbs: 1.0, copy_gbs: 1.0, latency_ns: 1.0, buffer_bytes: 1, threads: 1 };
        let v = serde_json::to_value(&r).unwrap();
        for k in ["readGBs", "writeGBs", "copyGBs", "latencyNs", "bufferBytes", "threads"] {
            assert!(v.get(k).is_some(), "missing key {k}: {v}");
        }
    }
}
