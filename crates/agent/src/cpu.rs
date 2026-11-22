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

/// Observed CPU clocks while a test runs (Linux cpufreq; other OSes don't expose
/// reliable live clocks to unprivileged programs).
#[derive(Serialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "camelCase")]
pub struct Clocks {
    /// Highest clock any core reached during the single-thread tests.
    pub single_max_mhz: u32,
    /// Median clock across all cores during the all-thread tests.
    pub all_core_mhz: u32,
}

#[cfg(target_os = "linux")]
fn core_freqs_mhz() -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") else { return vec![] };
    dir.flatten()
        .filter(|e| e.file_name().to_string_lossy().strip_prefix("cpu").is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())))
        .filter_map(|e| std::fs::read_to_string(e.path().join("cpufreq/scaling_cur_freq")).ok())
        .filter_map(|s| s.trim().parse::<u32>().ok().map(|khz| khz / 1000))
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn core_freqs_mhz() -> Vec<u32> {
    vec![]
}

/// Sample core clocks every 100 ms while `f` runs with `busy` threads. Returns the highest
/// clock seen and the median clock of the `busy` fastest cores (idle cores sit at their
/// minimum and would drag a plain median down).
pub fn sample_clocks<T>(busy: usize, f: impl FnOnce() -> T) -> (T, Option<(u32, u32)>) {
    if core_freqs_mhz().is_empty() {
        return (f(), None);
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = stop.clone();
    let sampler = std::thread::spawn(move || {
        let (mut max, mut medians) = (0u32, Vec::new());
        while !s2.load(Ordering::Relaxed) {
            let mut v = core_freqs_mhz();
            if !v.is_empty() {
                v.sort_unstable();
                max = max.max(*v.last().unwrap());
                let top = &v[v.len().saturating_sub(busy.max(1))..];
                medians.push(top[top.len() / 2]);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        medians.sort_unstable();
        (max, medians.get(medians.len() / 2).copied().unwrap_or(0))
    });
    let out = f();
    stop.store(true, Ordering::Relaxed);
    let r = sampler.join().ok().filter(|(m, _)| *m > 0);
    (out, r)
}
