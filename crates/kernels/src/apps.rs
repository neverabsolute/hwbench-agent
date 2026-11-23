//! Application-style kernels (added in KERNELS_VERSION 2): workloads that behave like
//! real programs rather than tight loops. Same rules as the rest of the crate: outputs
//! must be bit-identical on every platform, so no transcendental floating-point
//! functions, no FMA, no hash-map iteration and no usize-width-dependent arithmetic.

use crate::Rng;

// ---------------------------------------------------------------------------------------
// search: alpha-beta game-tree search on Othello positions (game AI, branch prediction).

const NOT_A: u64 = 0xfefe_fefe_fefe_fefe; // clears file A after a shift towards it
const NOT_H: u64 = 0x7f7f_7f7f_7f7f_7f7f;

#[inline]
fn shift(b: u64, dir: usize) -> u64 {
    match dir {
        0 => (b << 1) & NOT_A,
        1 => (b >> 1) & NOT_H,
        2 => b << 8,
        3 => b >> 8,
        4 => (b << 9) & NOT_A,
        5 => (b << 7) & NOT_H,
        6 => (b >> 7) & NOT_A,
        _ => (b >> 9) & NOT_H,
    }
}

fn legal_moves(me: u64, opp: u64) -> u64 {
    let empty = !(me | opp);
    let mut moves = 0;
    for d in 0..8 {
        let mut x = shift(me, d) & opp;
        for _ in 0..5 {
            x |= shift(x, d) & opp;
        }
        moves |= shift(x, d) & empty;
    }
    moves
}

fn flips(me: u64, opp: u64, sq: u64) -> u64 {
    let mut all = 0;
    for d in 0..8 {
        let mut f = 0;
        let mut x = shift(sq, d);
        while x & opp != 0 {
            f |= x;
            x = shift(x, d);
        }
        if x & me != 0 {
            all |= f;
        }
    }
    all
}

const CORNERS: u64 = 0x8100_0000_0000_0081;

fn evaluate(me: u64, opp: u64) -> i32 {
    let material = me.count_ones() as i32 - opp.count_ones() as i32;
    let corners = (me & CORNERS).count_ones() as i32 - (opp & CORNERS).count_ones() as i32;
    let mobility = legal_moves(me, opp).count_ones() as i32 - legal_moves(opp, me).count_ones() as i32;
    material + 25 * corners + 3 * mobility
}

fn negamax(me: u64, opp: u64, depth: u32, mut alpha: i32, beta: i32, nodes: &mut u64) -> i32 {
    *nodes += 1;
    if depth == 0 {
        return evaluate(me, opp);
    }
    let mut moves = legal_moves(me, opp);
    if moves == 0 {
        if legal_moves(opp, me) == 0 {
            return 1000 * (me.count_ones() as i32 - opp.count_ones() as i32);
        }
        return -negamax(opp, me, depth - 1, -beta, -alpha, nodes);
    }
    let mut best = i32::MIN + 1;
    while moves != 0 {
        let sq = moves.isolate_lowest_one();
        moves &= moves - 1;
        let f = flips(me, opp, sq);
        let score = -negamax(opp & !f, me | f | sq, depth - 1, -beta, -alpha, nodes);
        if score > best {
            best = score;
        }
        if best > alpha {
            alpha = best;
        }
        if alpha >= beta {
            break;
        }
    }
    best
}

pub(crate) fn search(rng: &mut Rng, out: &mut blake3::Hasher) {
    let mut nodes = 0u64;
    for _ in 0..3 {
        // Start position, then 10–17 random legal plies to reach a varied mid-game.
        let (mut me, mut opp): (u64, u64) = (0x0000_0008_1000_0000, 0x0000_0010_0800_0000);
        for _ in 0..(10 + rng.next_u64() % 8) {
            let moves = legal_moves(me, opp);
            if moves == 0 {
                std::mem::swap(&mut me, &mut opp);
                continue;
            }
            let pick = (rng.next_u64() % moves.count_ones() as u64) as u32;
            let mut m = moves;
            for _ in 0..pick {
                m &= m - 1;
            }
            let sq = m.isolate_lowest_one();
            let f = flips(me, opp, sq);
            let (nme, nopp) = (opp & !f, me | f | sq);
            me = nme;
            opp = nopp;
        }
        let score = negamax(me, opp, 6, -1_000_000, 1_000_000, &mut nodes);
        out.update(&me.to_le_bytes());
        out.update(&opp.to_le_bytes());
        out.update(&score.to_le_bytes());
    }
    out.update(&nodes.to_le_bytes());
}

// ---------------------------------------------------------------------------------------
// script: a bytecode interpreter running a small program (scripting engines, dispatch).

#[derive(Clone, Copy)]
enum Op {
    Push(i64),
    Load(u8),
    Store(u8),
    Add,
    Mul,
    Rem,
    Xor,
    Shr(u8),
    Lt,
    Jz(u16),
    Jmp(u16),
    LoadIdx,  // push table[pop & 255]
    StoreIdx, // table[pop & 255] = pop
    Halt,
}

pub(crate) fn script(rng: &mut Rng, out: &mut blake3::Hasher) {
    use Op::*;
    let (a, b, n) = ((rng.next_u64() % 97 + 3) as i64, (rng.next_u64() % 89 + 5) as i64, 120_000i64);
    // for i in 0..n { x = (x * a + i) % 1_000_003; if x % 3 == 0 { y += t[x & 255] } else { y ^= x >> 2 }; t[i & 255] = y + b }
    // Addresses are fixed: ELSE = 24, TAIL = 29, EXIT = 39.
    #[rustfmt::skip]
    let prog: [Op; 40] = [
        /* 0 */ Push(0), Store(0),                                        // i = 0
        /* 2 */ Load(0), Push(n), Lt, Jz(39),                             // while i < n
        /* 6 */ Load(1), Push(a), Mul, Load(0), Add, Push(1_000_003), Rem, Store(1), // x = (x*a + i) % p
        /* 14 */ Load(1), Push(3), Rem, Jz(24),                           // if x % 3 != 0:
        /* 18 */ Load(2), Load(1), Shr(2), Xor, Store(2), Jmp(29),        //   y ^= x >> 2
        /* 24 */ Load(2), Load(1), LoadIdx, Add, Store(2),                // else: y += t[x & 255]
        /* 29 */ Load(2), Push(b), Add, Load(0), StoreIdx,                // t[i & 255] = y + b
        /* 34 */ Load(0), Push(1), Add, Store(0), Jmp(2),                 // i += 1
        /* 39 */ Halt,
    ];

    let mut regs = [0i64; 4];
    regs[1] = (rng.next_u64() % 1_000_003) as i64;
    let mut table = [0i64; 256];
    for t in table.iter_mut() {
        *t = (rng.next_u64() % 65_536) as i64;
    }
    let mut stack: Vec<i64> = Vec::with_capacity(16);
    let mut pc = 0usize;
    let mut steps = 0u64;
    loop {
        steps += 1;
        match prog[pc] {
            Push(v) => stack.push(v),
            Load(r) => stack.push(regs[r as usize]),
            Store(r) => regs[r as usize] = stack.pop().unwrap(),
            Add => { let y = stack.pop().unwrap(); let x = stack.pop().unwrap(); stack.push(x.wrapping_add(y)); }
            Mul => { let y = stack.pop().unwrap(); let x = stack.pop().unwrap(); stack.push(x.wrapping_mul(y)); }
            Rem => { let y = stack.pop().unwrap(); let x = stack.pop().unwrap(); stack.push(x.rem_euclid(y)); }
            Xor => { let y = stack.pop().unwrap(); let x = stack.pop().unwrap(); stack.push(x ^ y); }
            Shr(s) => { let x = stack.pop().unwrap(); stack.push(x >> s); }
            Lt => { let y = stack.pop().unwrap(); let x = stack.pop().unwrap(); stack.push((x < y) as i64); }
            Jz(t) => { if stack.pop().unwrap() == 0 { pc = t as usize; continue; } }
            Jmp(t) => { pc = t as usize; continue; }
            LoadIdx => { let i = stack.pop().unwrap(); stack.push(table[(i & 255) as usize]); }
            StoreIdx => { let i = stack.pop().unwrap(); let v = stack.pop().unwrap(); table[(i & 255) as usize] = v; }
            Halt => break,
        }
        pc += 1;
    }
    out.update(&steps.to_le_bytes());
    for r in regs {
        out.update(&r.to_le_bytes());
    }
    for t in table.iter().step_by(8) {
        out.update(&t.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------------------
// render: a small path tracer (rendering; floating point with irregular branches).

#[derive(Clone, Copy)]
struct V3(f64, f64, f64);
impl V3 {
    fn add(self, o: V3) -> V3 { V3(self.0 + o.0, self.1 + o.1, self.2 + o.2) }
    fn sub(self, o: V3) -> V3 { V3(self.0 - o.0, self.1 - o.1, self.2 - o.2) }
    fn mul(self, s: f64) -> V3 { V3(self.0 * s, self.1 * s, self.2 * s) }
    fn had(self, o: V3) -> V3 { V3(self.0 * o.0, self.1 * o.1, self.2 * o.2) }
    fn dot(self, o: V3) -> f64 { self.0 * o.0 + self.1 * o.1 + self.2 * o.2 }
    fn norm(self) -> V3 { self.mul(1.0 / self.dot(self).sqrt()) }
}

fn random_in_sphere(rng: &mut Rng) -> V3 {
    loop {
        let v = V3(rng.next_f64() * 2.0 - 1.0, rng.next_f64() * 2.0 - 1.0, rng.next_f64() * 2.0 - 1.0);
        if v.dot(v) < 1.0 {
            return v;
        }
    }
}

pub(crate) fn render(rng: &mut Rng, out: &mut blake3::Hasher) {
    // Scene: a ground sphere plus 9 random spheres, each with a colour.
    let mut spheres: Vec<(V3, f64, V3)> = vec![(V3(0.0, -1000.0, 0.0), 1000.0, V3(0.5, 0.5, 0.5))];
    for _ in 0..9 {
        let c = V3(rng.next_f64() * 8.0 - 4.0, 0.3 + rng.next_f64() * 1.2, rng.next_f64() * 6.0 - 7.0);
        let col = V3(0.2 + rng.next_f64() * 0.8, 0.2 + rng.next_f64() * 0.8, 0.2 + rng.next_f64() * 0.8);
        spheres.push((c, c.1, col));
    }
    let (w, h, samples) = (120u32, 90u32, 10u32);
    let mut sum = [0u64; 3];
    for y in 0..h {
        for x in 0..w {
            let mut acc = V3(0.0, 0.0, 0.0);
            for _ in 0..samples {
                let u = (x as f64 + rng.next_f64()) / w as f64 * 2.0 - 1.0;
                let v = 1.0 - (y as f64 + rng.next_f64()) / h as f64 * 2.0;
                let mut origin = V3(0.0, 1.5, 4.0);
                let mut dir = V3(u * 1.33, v, -1.5).norm();
                let mut atten = V3(1.0, 1.0, 1.0);
                let mut light = V3(0.0, 0.0, 0.0);
                for _bounce in 0..4 {
                    let mut hit_t = 1e30;
                    let mut hit = None;
                    for (i, (c, r, _)) in spheres.iter().enumerate() {
                        let oc = origin.sub(*c);
                        let b = oc.dot(dir);
                        let disc = b * b - (oc.dot(oc) - r * r);
                        if disc > 0.0 {
                            let t = -b - disc.sqrt();
                            if t > 1e-4 && t < hit_t {
                                hit_t = t;
                                hit = Some(i);
                            }
                        }
                    }
                    match hit {
                        None => {
                            // Sky: brighter towards the top.
                            let s = 0.5 * (dir.1 + 1.0);
                            light = light.add(atten.had(V3(1.0 - 0.5 * s, 1.0 - 0.3 * s, 1.0)));
                            break;
                        }
                        Some(i) => {
                            let (c, _, col) = spheres[i];
                            let p = origin.add(dir.mul(hit_t));
                            let n = p.sub(c).norm();
                            origin = p;
                            dir = n.add(random_in_sphere(rng)).norm();
                            atten = atten.had(col);
                        }
                    }
                }
                acc = acc.add(light);
            }
            // Quantize per pixel so the digest is compact; values are deterministic anyway.
            sum[0] = sum[0].wrapping_add((acc.0 * 1000.0) as u64);
            sum[1] = sum[1].wrapping_add((acc.1 * 1000.0) as u64);
            sum[2] = sum[2].wrapping_add((acc.2 * 1000.0) as u64);
            out.update(&acc.0.to_bits().to_le_bytes());
        }
    }
    for s in sum {
        out.update(&s.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------------------
// crypto: AES-256-GCM, as used by TLS and disk encryption (uses AES/CLMUL instructions
// where the CPU has them; the WebAssembly verifier runs the portable code, same output).

pub(crate) fn crypto(rng: &mut Rng, out: &mut blake3::Hasher) {
    use aes_gcm::aead::{AeadInPlace, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};
    let mut key = [0u8; 32];
    rng.fill_bytes(&mut key);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut buf = vec![0u8; 512 * 1024];
    rng.fill_bytes(&mut buf);
    for round in 0u32..14 {
        let mut nonce = [0u8; 12];
        rng.fill_bytes(&mut nonce);
        let tag = cipher
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), &round.to_le_bytes(), &mut buf)
            .expect("buffer within AES-GCM limits");
        out.update(&tag);
    }
    for c in buf.chunks(4096) {
        out.update(&c[..16]);
    }
}

// ---------------------------------------------------------------------------------------
// parse: build and parse JSON-like text (browsers, config files, APIs).

fn gen_value(rng: &mut Rng, depth: u32, s: &mut Vec<u8>) {
    let r = rng.next_u64();
    match if depth > 4 { r % 3 } else { r % 6 } {
        0 => s.extend_from_slice((r >> 20).to_string().as_bytes()),
        1 => {
            s.push(b'"');
            s.extend_from_slice(crate::WORDS[(r >> 8) as usize % crate::WORDS.len()].as_bytes());
            if r & 1 == 0 {
                s.extend_from_slice(b"\\n");
            }
            s.push(b'"');
        }
        2 => s.extend_from_slice(if r & 1 == 0 { b"true" } else { b"null" }),
        3 | 4 => {
            s.push(b'{');
            for i in 0..(r >> 40) % 6 {
                if i > 0 {
                    s.push(b',');
                }
                s.push(b'"');
                s.extend_from_slice(crate::WORDS[(rng.next_u64() % crate::WORDS.len() as u64) as usize].as_bytes());
                s.extend_from_slice(b"\":");
                gen_value(rng, depth + 1, s);
            }
            s.push(b'}');
        }
        _ => {
            s.push(b'[');
            for i in 0..(r >> 40) % 7 {
                if i > 0 {
                    s.push(b',');
                }
                gen_value(rng, depth + 1, s);
            }
            s.push(b']');
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    stats: [u64; 6], // objects, arrays, strings, numbers, literals, key-hash
}

impl Parser<'_> {
    fn value(&mut self) -> u64 {
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                self.stats[0] += 1;
                let mut h = 17u64;
                while self.s[self.i] != b'}' {
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                    let k = self.string();
                    self.stats[5] = self.stats[5].rotate_left(5) ^ k;
                    self.i += 1; // ':'
                    h = h.wrapping_mul(31).wrapping_add(self.value() ^ k);
                }
                self.i += 1;
                h
            }
            b'[' => {
                self.i += 1;
                self.stats[1] += 1;
                let mut h = 7u64;
                while self.s[self.i] != b']' {
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                    h = h.wrapping_mul(37).wrapping_add(self.value());
                }
                self.i += 1;
                h
            }
            b'"' => {
                self.stats[2] += 1;
                self.string()
            }
            b't' | b'n' => {
                self.stats[4] += 1;
                let v = if self.s[self.i] == b't' { 1 } else { 2 };
                self.i += 4;
                v
            }
            _ => {
                self.stats[3] += 1;
                let mut v = 0u64;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    v = v.wrapping_mul(10).wrapping_add((self.s[self.i] - b'0') as u64);
                    self.i += 1;
                }
                v
            }
        }
    }

    fn string(&mut self) -> u64 {
        self.i += 1; // opening quote
        let mut h = 0xcbf2_9ce4_8422_2325u64; // FNV-1a
        while self.s[self.i] != b'"' {
            let c = if self.s[self.i] == b'\\' {
                self.i += 1;
                if self.s[self.i] == b'n' { b'\n' } else { self.s[self.i] }
            } else {
                self.s[self.i]
            };
            h = (h ^ c as u64).wrapping_mul(0x100_0000_01b3);
            self.i += 1;
        }
        self.i += 1;
        h
    }
}

pub(crate) fn parse(rng: &mut Rng, out: &mut blake3::Hasher) {
    let mut text = Vec::with_capacity(96 * 1024);
    text.push(b'[');
    let mut first = true;
    while text.len() < 64 * 1024 {
        if !first {
            text.push(b',');
        }
        first = false;
        gen_value(rng, 0, &mut text);
    }
    text.push(b']');
    // Parse it several times, as a server handling repeated requests would.
    let mut total = 0u64;
    let mut stats = [0u64; 6];
    for _ in 0..60 {
        let mut p = Parser { s: &text, i: 0, stats: [0; 6] };
        total = total.wrapping_add(p.value());
        stats = p.stats;
    }
    out.update(&(text.len() as u64).to_le_bytes());
    out.update(&total.to_le_bytes());
    for s in stats {
        out.update(&s.to_le_bytes());
    }
}
