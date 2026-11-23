//! Prints single-thread chunk rates per kernel: `cargo run --release -p hwbench-kernels --example chunk_times`.
use hwbench_kernels::{run_chunk, ALL_KERNELS};
use std::time::Instant;

fn main() {
    let seed = [3u8; 32];
    for k in ALL_KERNELS {
        let _ = run_chunk(&seed, k, 0);
        let t = Instant::now();
        let mut n = 0u32;
        while t.elapsed().as_secs_f64() < 1.5 {
            let _ = run_chunk(&seed, k, n);
            n += 1;
        }
        let rate = n as f64 / t.elapsed().as_secs_f64();
        println!("{:<9} {:>7.1} chunks/s  {:>6.2} ms/chunk", k.name(), rate, 1000.0 / rate);
    }
}
