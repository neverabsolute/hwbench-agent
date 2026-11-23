//! Pre-flight check for a busy system. A game or render running alongside the
//! benchmark lowers every score, and a game that fills the graphics card's memory
//! pushes the bandwidth test's buffer out to system RAM. Before benchmarking we
//! sample CPU and GPU load, tell the user what's busy, and wait a while for it to
//! settle. The final sample goes into the submission so the site can label the run.

use serde::Serialize;
use std::time::{Duration, Instant};
use sysinfo::{ProcessesToUpdate, System};

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Load {
    /// Whole-system CPU use (all cores, 0–100) just before benchmarking.
    pub cpu_pct: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_total_bytes: Option<u64>,
    /// How long we waited for the system to settle.
    pub waited_ms: u64,
    pub busy: bool,
}

const CPU_BUSY: f32 = 25.0;
const GPU_BUSY: f32 = 25.0;
const VRAM_BUSY: f64 = 0.5;
const MAX_WAIT: Duration = Duration::from_secs(30);

impl Load {
    fn reasons(&self) -> Vec<String> {
        let mut r = Vec::new();
        if self.cpu_pct >= CPU_BUSY {
            r.push(format!("CPU {:.0}% busy", self.cpu_pct));
        }
        if let Some(g) = self.gpu_pct.filter(|&g| g >= GPU_BUSY) {
            r.push(format!("GPU {g:.0}% busy"));
        }
        if let (Some(u), Some(t)) = (self.vram_used_bytes, self.vram_total_bytes)
            && t > 0
            && u as f64 / t as f64 >= VRAM_BUSY
        {
            r.push(format!("{:.1} of {:.0} GB graphics memory in use", u as f64 / 1e9, t as f64 / 1e9));
        }
        r
    }
}

struct Sampler {
    sys: System,
    gpu_device_id: Option<u32>,
}

impl Sampler {
    fn sample(&mut self) -> (Load, Vec<String>) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        std::thread::sleep(Duration::from_millis(1000).max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL));
        self.sys.refresh_cpu_usage();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        let cpu_pct = self.sys.global_cpu_usage();
        let cores = self.sys.cpus().len().max(1) as f32;
        let me = sysinfo::get_current_pid().ok();
        let mut procs: Vec<(String, f32)> = self
            .sys
            .processes()
            .iter()
            .filter(|(pid, _)| Some(**pid) != me)
            .map(|(_, p)| (p.name().to_string_lossy().into_owned(), p.cpu_usage() / cores))
            .filter(|(n, pct)| *pct >= 3.0 && !n.eq_ignore_ascii_case("System Idle Process") && !n.eq_ignore_ascii_case("idle"))
            .collect();
        procs.sort_by(|a, b| b.1.total_cmp(&a.1));
        procs.dedup_by(|a, b| a.0 == b.0);
        let top = procs.into_iter().take(3).map(|(n, _)| n).collect();
        let g = gpu_load(self.gpu_device_id);
        let mut load = Load { cpu_pct, gpu_pct: g.util_pct, vram_used_bytes: g.used, vram_total_bytes: g.total, ..Default::default() };
        load.busy = !load.reasons().is_empty();
        (load, top)
    }
}

/// Sample load and, if the system is busy, explain and wait up to 30 s for it to settle.
pub fn preflight(gpu_device_id: Option<u32>, wait: bool) -> Load {
    let Ok(sys) = System::new() else { return Load::default() };
    let mut s = Sampler { sys, gpu_device_id };
    let start = Instant::now();
    let (mut load, top) = s.sample();
    if !load.busy || !wait {
        load.waited_ms = start.elapsed().as_millis() as u64;
        return load;
    }
    println!("Your computer is busy: {}.", load.reasons().join(" · "));
    if !top.is_empty() {
        println!("  Busiest programs: {}", top.join(", "));
    }
    println!("  Close games and other heavy programs for a fair result.");
    println!("  Waiting up to {}s for things to settle (Ctrl+C to stop)…", MAX_WAIT.as_secs());
    while start.elapsed() < MAX_WAIT {
        std::thread::sleep(Duration::from_secs(2));
        (load, _) = s.sample();
        if !load.busy {
            println!("  Settled. Starting.\n");
            break;
        }
    }
    if load.busy {
        println!("  Still busy ({}). Running anyway; the results page will note it.\n", load.reasons().join(" · "));
    }
    load.waited_ms = start.elapsed().as_millis() as u64;
    load
}

#[derive(Default, Debug, PartialEq)]
struct GpuLoad {
    util_pct: Option<f32>,
    used: Option<u64>,
    total: Option<u64>,
}

fn gpu_load(device_id: Option<u32>) -> GpuLoad {
    if let Some(out) = crate::util::command_output(
        std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=pci.device_id,utilization.gpu,memory.used,memory.total", "--format=csv,noheader,nounits"]),
        Duration::from_secs(5),
    ) && let Some(g) = parse_nvidia_smi_load(&out, device_id)
    {
        return g;
    }
    #[cfg(target_os = "linux")]
    if let Some(g) = amdgpu_sysfs() {
        return g;
    }
    #[cfg(target_os = "windows")]
    if let Some(g) = windows_counters() {
        return g;
    }
    GpuLoad::default()
}

fn parse_nvidia_smi_load(out: &str, device_id: Option<u32>) -> Option<GpuLoad> {
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split(',').map(str::trim).collect()).filter(|r: &Vec<&str>| r.len() >= 4).collect();
    let id = |r: &Vec<&str>| u32::from_str_radix(r[0].trim_start_matches("0x").trim_start_matches("0X"), 16).ok().map(|v| v >> 16);
    let row = rows.iter().find(|r| device_id.is_some() && id(r) == device_id).or(if rows.len() == 1 { rows.first() } else { None })?;
    let mib = |s: &str| s.parse::<u64>().ok().map(|m| m * 1024 * 1024);
    Some(GpuLoad { util_pct: row[1].parse().ok(), used: mib(row[2]), total: mib(row[3]) })
}

/// AMD on Linux: the busiest card's load and its VRAM use.
#[cfg(target_os = "linux")]
fn amdgpu_sysfs() -> Option<GpuLoad> {
    let read = |p: &std::path::Path, f: &str| std::fs::read_to_string(p.join(f)).ok().and_then(|s| s.trim().parse::<u64>().ok());
    let mut best: Option<GpuLoad> = None;
    for e in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = e.path().join("device");
        let Some(busy) = read(&dev, "gpu_busy_percent") else { continue };
        let g = GpuLoad { util_pct: Some(busy as f32), used: read(&dev, "mem_info_vram_used"), total: read(&dev, "mem_info_vram_total") };
        if best.as_ref().is_none_or(|b| g.total > b.total) {
            best = Some(g);
        }
    }
    best
}

/// Other GPUs on Windows: the 3D engine's utilization from Windows' GPU performance
/// counters (the numbers Task Manager shows). VRAM use isn't comparable across
/// adapters there, so only utilization is reported.
#[cfg(target_os = "windows")]
fn windows_counters() -> Option<GpuLoad> {
    let out = crate::util::command_output(
        std::process::Command::new("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[math]::Round(((Get-Counter '\\GPU Engine(*engtype_3D)\\Utilization Percentage' -ErrorAction Stop).CounterSamples | Measure-Object CookedValue -Sum).Sum)",
        ]),
        Duration::from_secs(8),
    )?;
    let pct: f32 = out.trim().parse().ok()?;
    Some(GpuLoad { util_pct: Some(pct.clamp(0.0, 100.0)), used: None, total: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_load_picks_device() {
        let out = "0x268410DE, 97, 7712, 8188\n0x2B8510DE, 3, 900, 32607\n";
        let g = parse_nvidia_smi_load(out, Some(0x2684)).unwrap();
        assert_eq!(g, GpuLoad { util_pct: Some(97.0), used: Some(7712 << 20), total: Some(8188 << 20) });
        assert_eq!(parse_nvidia_smi_load(out, None), None);
        assert!(parse_nvidia_smi_load("0x268410DE, [N/A], 100, 8188\n", None).unwrap().util_pct.is_none());
    }

    #[test]
    fn busy_reasons() {
        let quiet = Load { cpu_pct: 5.0, gpu_pct: Some(2.0), vram_used_bytes: Some(1 << 30), vram_total_bytes: Some(8 << 30), ..Default::default() };
        assert!(quiet.reasons().is_empty());
        let game = Load { cpu_pct: 40.0, gpu_pct: Some(98.0), vram_used_bytes: Some(7 << 30), vram_total_bytes: Some(8 << 30), ..Default::default() };
        assert_eq!(game.reasons().len(), 3);
    }
}
