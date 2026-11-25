//! Minimal C-ABI wrapper (no wasm-bindgen) so any WebAssembly host can verify
//! chunk digests: write the seed into `hwb_seed_ptr()`, call `hwb_run_chunk`,
//! read 32 bytes from `hwb_out_ptr()`. For a CPU test, call
//! `hwb_use_test_seed(threads)` after writing the session seed.

use core::cell::UnsafeCell;
use hwbench_kernels::{Kernel, KERNELS_VERSION, run_chained, run_chunk, test_seed};

struct Buf(UnsafeCell<[u8; 32]>);
// SAFETY: wasm32-unknown-unknown here is single-threaded.
unsafe impl Sync for Buf {}

static SEED: Buf = Buf(UnsafeCell::new([0; 32]));
static OUT: Buf = Buf(UnsafeCell::new([0; 32]));

struct Short(UnsafeCell<[u8; 8]>);
// SAFETY: as above.
unsafe impl Sync for Short {}
static PREV: Short = Short(UnsafeCell::new([0; 8]));

/// Where the host writes the previous chunk's 8-byte digest before `hwb_run_chained`.
#[unsafe(no_mangle)]
pub extern "C" fn hwb_prev_ptr() -> *mut u8 {
    PREV.0.get().cast()
}

/// Kernels v4: run chunk `index` of `lane` in a test on `threads` workers, chained on the digest
/// at `hwb_prev_ptr()`; the session seed stays at `hwb_seed_ptr()`. The 8-byte result goes to
/// `hwb_out_ptr()`. Returns 0, or -1 for an unknown kernel.
#[unsafe(no_mangle)]
pub extern "C" fn hwb_run_chained(kernel: u32, threads: u32, lane: u32, index: u32) -> i32 {
    let Some(k) = Kernel::from_u32(kernel) else { return -1 };
    // SAFETY: single-threaded; host writes the seed and previous digest before calling.
    let (seed, prev) = unsafe { (*SEED.0.get(), *PREV.0.get()) };
    let d = run_chained(&test_seed(&seed, threads), k, lane, index, &prev);
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&d);
    unsafe { *OUT.0.get() = out };
    0
}

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

/// Replace the seed with the seed of a CPU test run on `threads` workers.
#[unsafe(no_mangle)]
pub extern "C" fn hwb_use_test_seed(threads: u32) {
    // SAFETY: single-threaded; host writes the session seed before calling.
    unsafe { *SEED.0.get() = test_seed(&*SEED.0.get(), threads) };
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
