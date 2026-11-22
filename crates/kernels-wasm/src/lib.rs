//! Minimal C-ABI wrapper (no wasm-bindgen) so any WebAssembly host can verify
//! chunk digests: write the seed into `hwb_seed_ptr()`, call `hwb_run_chunk`,
//! read 32 bytes from `hwb_out_ptr()`.

use core::cell::UnsafeCell;
use hwbench_kernels::{Kernel, KERNELS_VERSION, run_chunk};

struct Buf(UnsafeCell<[u8; 32]>);
// SAFETY: wasm32-unknown-unknown here is single-threaded.
unsafe impl Sync for Buf {}

static SEED: Buf = Buf(UnsafeCell::new([0; 32]));
static OUT: Buf = Buf(UnsafeCell::new([0; 32]));

#[unsafe(no_mangle)]
pub extern "C" fn hwb_kernels_version() -> u32 {
    KERNELS_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn hwb_seed_ptr() -> *mut u8 {
    SEED.0.get().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn hwb_out_ptr() -> *mut u8 {
    OUT.0.get().cast()
}

/// Returns 0 on success, -1 for an unknown kernel id.
#[unsafe(no_mangle)]
pub extern "C" fn hwb_run_chunk(kernel: u32, index: u32) -> i32 {
    let Some(k) = Kernel::from_u32(kernel) else { return -1 };
    // SAFETY: single-threaded; host writes the seed before calling.
    let seed = unsafe { *SEED.0.get() };
    let d = run_chunk(&seed, k, index);
    unsafe { *OUT.0.get() = d };
    0
}
