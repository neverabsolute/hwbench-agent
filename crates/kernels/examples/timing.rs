use hwbench_kernels::*;
use std::time::Instant;
fn main() {
    let seed = [9u8; 32];
    for k in ALL_KERNELS {
        let t = Instant::now();
        for i in 0..20 { std::hint::black_box(run_chunk(&seed, k, i)); }
        println!("{:>8}: {:.2} ms/chunk", k.name(), t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    }
}
