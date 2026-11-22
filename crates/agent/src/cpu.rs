use hwbench_kernels::{Kernel, run_chunk};
use serde::Serialize;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CpuTest {
    pub kernel: &'static str,
    pub threads: usize,
    pub chunks: u32,
    pub elapsed_ns: u64,
    /// First 8 bytes of each chunk digest, hex, in index order.
    pub digests: String,
}

impl CpuTest {
    pub fn chunks_per_sec(&self) -> f64 {
        self.chunks as f64 / (self.elapsed_ns as f64 / 1e9)
    }
}

/// Run `kernel` on `threads` workers for roughly `window`. Chunk indices are
/// handed out in order, so completed chunks are always exactly `0..chunks`.
pub fn run_test(seed: &[u8; 32], kernel: Kernel, threads: usize, window: Duration) -> CpuTest {
    let next = AtomicU32::new(0);
    let start = Instant::now();
    let deadline = start + window;
    let mut all: Vec<(u32, [u8; 8])> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut local = Vec::with_capacity(512);
                    while Instant::now() < deadline {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let d = run_chunk(seed, kernel, i);
                        local.push((i, d[..8].try_into().unwrap()));
                    }
                    local
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let elapsed = start.elapsed();
    all.sort_unstable_by_key(|(i, _)| *i);
    debug_assert!(all.iter().enumerate().all(|(n, (i, _))| n as u32 == *i));
    let mut digests = String::with_capacity(all.len() * 16);
    for (_, d) in &all {
        digests.push_str(&crate::util::hex(d));
    }
    CpuTest { kernel: kernel.name(), threads, chunks: all.len() as u32, elapsed_ns: elapsed.as_nanos() as u64, digests }
}

/// Spin all cores briefly so frequency scaling has ramped before measuring.
pub fn warm_up(seed: &[u8; 32], threads: usize) {
    let _ = run_test(seed, Kernel::Hash, threads, Duration::from_millis(400));
}
