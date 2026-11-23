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

/// Observed CPU clocks while a test runs (Linux cpufreq, Windows performance counters;
/// macOS doesn't expose live clocks to unprivileged programs).
#[derive(Serialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "camelCase")]
pub struct Clocks {
    /// Highest clock any core reached during the single-thread tests.
    pub single_max_mhz: u32,
    /// Median clock across all cores during the all-thread tests.
    pub all_core_mhz: u32,
}

/// Reads the current clock of every core, in MHz.
struct ClockReader {
    #[cfg(windows)]
    pdh: Option<win::Pdh>,
}

impl ClockReader {
    fn new() -> Self {
        ClockReader {
            #[cfg(windows)]
            pdh: win::Pdh::open(),
        }
    }

    #[cfg(target_os = "linux")]
    fn read(&mut self) -> Vec<u32> {
        let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") else { return vec![] };
        dir.flatten()
            .filter(|e| e.file_name().to_string_lossy().strip_prefix("cpu").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
            .filter_map(|e| std::fs::read_to_string(e.path().join("cpufreq/scaling_cur_freq")).ok())
            .filter_map(|s| s.trim().parse::<u32>().ok().map(|khz| khz / 1000))
            .collect()
    }

    #[cfg(windows)]
    fn read(&mut self) -> Vec<u32> {
        self.pdh.as_mut().map(|p| p.read()).unwrap_or_default()
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn read(&mut self) -> Vec<u32> {
        vec![]
    }
}

/// Task Manager's method: effective clock = nominal frequency × "% Processor Performance"
/// (which goes above 100 under boost). Both counters are readable without admin rights.
#[cfg(windows)]
mod win {
    type Handle = isize;
    const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
    const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
    const PDH_MORE_DATA: u32 = 0x8000_07D2;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CounterValue {
        status: u32,
        value: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CounterItem {
        name: *const u16,
        value: CounterValue,
    }

    #[link(name = "pdh")]
    unsafe extern "system" {
        fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut Handle) -> u32;
        fn PdhAddEnglishCounterW(query: Handle, path: *const u16, user: usize, counter: *mut Handle) -> u32;
        fn PdhCollectQueryData(query: Handle) -> u32;
        fn PdhGetFormattedCounterArrayW(counter: Handle, format: u32, size: *mut u32, count: *mut u32, items: *mut u8) -> u32;
        fn PdhCloseQuery(query: Handle) -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub struct Pdh {
        query: Handle,
        perf: Handle,
        freq: Handle,
    }

    impl Drop for Pdh {
        fn drop(&mut self) {
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    impl Pdh {
        pub fn open() -> Option<Pdh> {
            let mut query: Handle = 0;
            if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
                return None;
            }
            let mut p = Pdh { query, perf: 0, freq: 0 };
            let add = |path: &str, out: &mut Handle| unsafe { PdhAddEnglishCounterW(query, wide(path).as_ptr(), 0, out) } == 0;
            if !add(r"\Processor Information(*)\% Processor Performance", &mut p.perf)
                || !add(r"\Processor Information(*)\Processor Frequency", &mut p.freq)
            {
                return None;
            }
            // Rate counters need a first sample to diff against.
            unsafe { PdhCollectQueryData(query) };
            Some(p)
        }

        fn values(&self, counter: Handle) -> Vec<(String, f64)> {
            let (mut size, mut count) = (0u32, 0u32);
            let fmt = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;
            if unsafe { PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, std::ptr::null_mut()) } != PDH_MORE_DATA {
                return vec![];
            }
            // u64 storage keeps the items 8-byte aligned.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            if unsafe { PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, buf.as_mut_ptr() as *mut u8) } != 0 {
                return vec![];
            }
            let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const CounterItem, count as usize) };
            items
                .iter()
                .filter(|i| i.value.status == 0 && !i.name.is_null())
                .map(|i| {
                    let len = (0..).take_while(|&k| unsafe { *i.name.add(k) } != 0).count();
                    (String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(i.name, len) }), i.value.value)
                })
                .collect()
        }

        pub fn read(&mut self) -> Vec<u32> {
            if unsafe { PdhCollectQueryData(self.query) } != 0 {
                return vec![];
            }
            let freq: std::collections::HashMap<String, f64> = self.values(self.freq).into_iter().collect();
            self.values(self.perf)
                .into_iter()
                .filter(|(name, _)| !name.contains("_Total"))
                .filter_map(|(name, perf)| Some((freq.get(&name)? * perf / 100.0) as u32))
                .filter(|mhz| *mhz > 0)
                .collect()
        }
    }
}

/// Sample core clocks every 100 ms while `f` runs with `busy` threads. Returns the highest
/// clock seen and the median clock of the `busy` fastest cores (idle cores sit at their
/// minimum and would drag a plain median down).
pub fn sample_clocks<T>(busy: usize, f: impl FnOnce() -> T) -> (T, Option<(u32, u32)>) {
    if cfg!(not(any(target_os = "linux", windows))) {
        return (f(), None);
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = stop.clone();
    let sampler = std::thread::spawn(move || {
        let mut reader = ClockReader::new();
        let (mut max, mut medians) = (0u32, Vec::new());
        while !s2.load(Ordering::Relaxed) {
            let mut v = reader.read();
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
