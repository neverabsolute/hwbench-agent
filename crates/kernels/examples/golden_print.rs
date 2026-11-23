//! Prints chunk 0 digests for seed [1; 32]: the values the `golden` test pins.
fn main() {
    for k in hwbench_kernels::ALL_KERNELS {
        let d = hwbench_kernels::run_chunk(&[1u8; 32], k, 0);
        println!("{} {}", k as u32, d.iter().map(|b| format!("{b:02x}")).collect::<String>());
    }
}
