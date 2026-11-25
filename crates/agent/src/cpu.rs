use hwbench_kernels::{Kernel, ShortDigest, run_chained};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CpuTest {
    pub kernel: &'static str,
    pub threads: usize,
    /// Chunks completed across all lanes.
    pub chunks: u32,
    pub elapsed_ns: u64,
    /// One chain per worker thread, in lane order: the first 8 bytes of each chunk's digest, hex.
    pub lanes: Vec<String>,
}

impl CpuTest {
    pub fn chunks_per_sec(&self) -> f64 {
        self.chunks as f64 / (self.elapsed_ns as f64 / 1e9)
    }
}

/// Run `kernel` on `threads` workers for roughly `window`. Each worker runs its own chain of
/// chunks (lane `n` for worker `n`); each chunk depends on the one before it in the lane.
pub fn run_test(test_seed: &[u8; 32], kernel: Kernel, threads: usize, window: Duration) -> CpuTest {
    let start = Instant::now();
    let deadline = start + window;
    let lanes: Vec<Vec<ShortDigest>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|lane| {
                s.spawn(move || {
                    crate::qos::full_speed_thread();
                    let mut out = Vec::with_capacity(512);
                    let mut prev = [0u8; 8];
                    let mut index = 0u32;
                    while Instant::now() < deadline {
                        prev = run_chained(test_seed, kernel, lane as u32, index, &prev);
                        out.push(prev);
                        index += 1;
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let elapsed = start.elapsed();
    let chunks = lanes.iter().map(|l| l.len() as u32).sum();
    let lanes = lanes.iter().map(|l| l.iter().map(|d| crate::util::hex(d)).collect::<String>()).collect();
    CpuTest { kernel: kernel.name(), threads, chunks, elapsed_ns: elapsed.as_nanos() as u64, lanes }
}

/// SHA-256 over every test's kernel, threads, chunks, elapsed time and lanes, in order:
/// `"{kernel}:{threads}:{chunks}:{elapsedNs}:{lane0},{lane1},…\n"` per test. Hex.
pub fn commitment(tests: &[CpuTest]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    for t in tests {
        h.update(format!("{}:{}:{}:{}:", t.kernel, t.threads, t.chunks, t.elapsed_ns).as_bytes());
        h.update(t.lanes.join(",").as_bytes());
        h.update(b"\n");
    }
    crate::util::hex(&h.finalize())
}

/// Spin all cores briefly so frequency scaling has ramped before measuring.
pub fn warm_up(seed: &[u8; 32], threads: usize) {
    let _ = run_test(seed, Kernel::Hash, threads, Duration::from_millis(400));
}

/// Observed CPU clocks while a test runs (Linux cpufreq). Windows' performance counters
/// report impossible clocks on current CPUs and macOS doesn't expose live clocks to
/// unprivileged programs, so other systems report none.
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
        .filter(|e| e.file_name().to_string_lossy().strip_prefix("cpu").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The server computes the same commitment (hwbench: src/lib/cpu-commit.ts); keep them in step.
    #[test]
    fn commitment_golden() {
        let tests = [
            CpuTest { kernel: "hash", threads: 1, chunks: 2, elapsed_ns: 800_000_000, lanes: vec!["00112233445566778899aabbccddeeff".into()] },
            CpuTest { kernel: "sort", threads: 2, chunks: 1, elapsed_ns: 801_000_000, lanes: vec!["0123456789abcdef".into(), String::new()] },
        ];
        assert_eq!(commitment(&tests), "f7983b53501f88aaef4efe09502647e4352c307d6a814fb981abb375a7b22a4b");
    }
}
