mod api;
mod cpu;
mod disk;
mod gpu;
mod hwinfo;
mod load;
mod power;
mod qos;
mod ram;
mod system;
mod util;

use hwbench_kernels::Kernel;
use std::io::Write;
use std::time::Duration;

const DEFAULT_API: &str = "https://hwbench.io";
const COMPONENTS: [&str; 4] = ["cpu", "disk", "gpu", "ram"];

struct Args {
    api: String,
    session: Option<String>,
    selection: Vec<String>,
    open: bool,
    dry_run: bool,
    gpu_index: Option<usize>,
    disk_path: Option<std::path::PathBuf>,
    list_gpus: bool,
    json: bool,
    /// Don't wait for a busy system to settle.
    no_wait: bool,
    /// Started by double-clicking a downloaded launcher: keep the window open at the end.
    pause_at_end: bool,
}

/// Downloaded launchers are named `hwbench_<code>[.exe]` (browsers may append
/// " (1)" etc.), which lets one signed binary serve every session.
fn code_from_exe_name() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let stem = exe.file_stem()?.to_str()?;
    let rest = stem.strip_prefix("hwbench_")?;
    let code: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (code.len() >= 8 && code.len() <= 16).then_some(code)
}

fn parse_args() -> Result<Args, lexopt::Error> {
    use lexopt::prelude::*;
    let mut args = Args {
        api: std::env::var("HWBENCH_API").unwrap_or_else(|_| DEFAULT_API.into()),
        session: None,
        selection: vec![],
        open: true,
        dry_run: false,
        gpu_index: None,
        disk_path: None,
        list_gpus: false,
        json: false,
        no_wait: false,
        pause_at_end: false,
    };
    if let Some(code) = code_from_exe_name() {
        args.session = Some(code);
        args.pause_at_end = true;
    }
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Long("api") => args.api = p.value()?.string()?,
            Long("session") | Short('s') => args.session = Some(p.value()?.string()?),
            Long("cpu") => args.selection.push("cpu".into()),
            Long("ram") => args.selection.push("ram".into()),
            Long("gpu") => args.selection.push("gpu".into()),
            Long("disk") => args.selection.push("disk".into()),
            Long("disk-path") => args.disk_path = Some(p.value()?.into()),
            Long("gpu-index") => args.gpu_index = Some(p.value()?.parse()?),
            Long("list-gpus") => args.list_gpus = true,
            Long("json") => args.json = true,
            Long("no-wait") => args.no_wait = true,
            Long("all") => args.selection.extend(COMPONENTS.iter().map(|s| s.to_string())),
            Long("no-open") => args.open = false,
            Long("dry-run") => args.dry_run = true,
            Long("version") | Short('V') => {
                println!("hwbench {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Long("help") | Short('h') => {
                println!(
                    "hwbench {} — https://hwbench.io\n\n\
                     USAGE: hwbench [--session CODE] [--cpu] [--gpu] [--disk] [--ram] [--all]\n\n\
                     --session CODE  use a session created on hwbench.io/run\n\
                     --cpu, --gpu, --ram, --disk  pick components (default: all)\n\
                     --disk-path DIR test the drive holding DIR (default: temp folder, or /var/tmp if it's in RAM)\n\
                     --list-gpus     show detected GPUs and exit\n\
                     --gpu-index N   benchmark GPU N from --list-gpus (default: best)\n\
                     --no-open       don't open the results page in a browser\n\
                     --no-wait       start even if other programs are keeping the computer busy\n\
                     --dry-run       run locally and print results, submit nothing\n\
                     --json          with --dry-run: print the payload that would be submitted\n\
                     --api URL       server (default {DEFAULT_API})",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            _ => return Err(arg.unexpected()),
        }
    }
    args.selection.sort();
    args.selection.dedup();
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => fail(&format!("{e} (try --help)")),
    };
    PAUSE_AT_END.store(args.pause_at_end, std::sync::atomic::Ordering::Relaxed);
    println!("hwbench {} · https://hwbench.io\n", env!("CARGO_PKG_VERSION"));

    qos::full_speed_process();
    let sys = system::collect();
    let gpus = gpu::list_adapters();
    if args.list_gpus {
        for (i, (_, g)) in gpus.iter().enumerate() {
            println!("  [{i}] {} ({}, {}, {:04x}:{:04x})", g.name, g.device_type, g.backend, g.vendor_id, g.device_id);
        }
        if gpus.is_empty() {
            println!("  no GPUs found");
        }
        return;
    }
    println!("  CPU   {} ({} cores / {} threads)", sys.cpu.brand, sys.cpu.physical_cores, sys.cpu.logical_cores);
    for (_, g) in &gpus {
        println!("  GPU   {} ({})", g.name, g.backend);
    }
    println!("  RAM   {:.1} GiB", sys.memory.total_bytes as f64 / (1u64 << 30) as f64);
    println!("  OS    {} ({})\n", sys.os_version, sys.arch);

    let tested_gpu = match args.gpu_index {
        Some(i) => gpus.get(i),
        None => gpus.first(),
    };
    let load = load::preflight(tested_gpu.map(|(_, g)| g.device_id), !args.no_wait);
    let power = power::detect();
    if power.limited() {
        println!("Note: this computer is {}. Plug it in and turn off power saving for a result you can compare", power.describe());
        println!("  with other machines; the results page will say how it ran.\n");
    }

    let api = api::Api::new(&args.api);
    let (code, challenge) = if args.dry_run {
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&(std::process::id() as u64).to_le_bytes());
        let sel = if args.selection.is_empty() { COMPONENTS.iter().map(|s| s.to_string()).collect() } else { args.selection.clone() };
        let challenge = api::Challenge {
            seed: util::hex(&seed),
            selection: sel,
            cpu: Some(api::CpuPlan {
                kernels: hwbench_kernels::ALL_KERNELS.iter().map(|k| k.name().to_string()).collect(),
                threads: vec![1, 4, 0],
                window_ms: 1500,
            }),
            gpu: Some(api::GpuPlan { window_ms: 2000 }),
            disk: Some(api::DiskPlan { window_ms: 2000 }),
        };
        (String::new(), challenge)
    } else {
        let code = match &args.session {
            Some(c) => c.clone(),
            None => {
                let sel: Vec<&str> = if args.selection.is_empty() { COMPONENTS.to_vec() } else { args.selection.iter().map(|s| s.as_str()).collect() };
                api.create_session(&sel).unwrap_or_else(|e| fail(&e)).code
            }
        };
        let challenge = api.claim(&code).unwrap_or_else(|e| fail(&e));
        (code, challenge)
    };
    let seed = util::unhex32(&challenge.seed).unwrap_or_else(|| fail("server sent an invalid seed"));
    let wants = |c: &str| challenge.selection.iter().any(|s| s == c);
    let logical = sys.cpu.available_threads.max(1);

    let mut body = serde_json::json!({
        "protocol": 1,
        "agent": {
            "version": env!("CARGO_PKG_VERSION"),
            "kernelsVersion": hwbench_kernels::KERNELS_VERSION,
            "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        },
        "deviceId": system::device_id(&sys.cpu.brand),
        "system": &sys,
        "gpus": gpus.iter().map(|(_, g)| g.clone()).collect::<Vec<_>>(),
        "load": &load,
        "power": &power,
    });

    if wants("cpu") {
        let plan = challenge.cpu.as_ref().unwrap_or_else(|| fail("server sent no CPU plan"));
        println!("Benchmarking CPU (about {}s)", plan.kernels.len() * plan.threads.len() * plan.window_ms as usize / 1000 + 2);
        let mut tests = Vec::new();
        let mut counts: Vec<usize> = plan.threads.iter().map(|&t| if t == 0 { logical } else { t.min(logical) }).collect();
        counts.dedup();
        if sys.cpu.available_threads < sys.cpu.logical_cores {
            println!("  note: only {} of {} threads are available to this process (VM/container limit?)", sys.cpu.available_threads, sys.cpu.logical_cores);
        }
        let mut clocks = cpu::Clocks::default();
        let mut have_clocks = false;
        let max_threads = *counts.iter().max().unwrap_or(&1);
        for threads in counts {
            cpu::warm_up(&seed, threads);
            print!("  {:>3} thread{:<2}", threads, if threads == 1 { "" } else { "s" });
            let (batch, sampled) = cpu::sample_clocks(threads, || {
                let mut batch = Vec::new();
                for name in &plan.kernels {
                    let k = Kernel::from_name(name).unwrap_or_else(|| fail(&format!("unknown kernel {name}; please update hwbench")));
                    let test = cpu::run_test(&seed, k, threads, Duration::from_millis(plan.window_ms));
                    print!(" {}={:.0}/s", name, test.chunks_per_sec());
                    let _ = std::io::stdout().flush();
                    batch.push(test);
                }
                batch
            });
            if let Some((max, median)) = sampled {
                have_clocks = true;
                if threads == 1 { clocks.single_max_mhz = max; }
                if threads == max_threads { clocks.all_core_mhz = median; }
            }
            tests.extend(batch);
            println!();
        }
        body["cpu"] = if have_clocks { serde_json::json!({ "tests": tests, "clocks": clocks }) } else { serde_json::json!({ "tests": tests }) };
    }

    if wants("gpu") {
        let window = Duration::from_millis(challenge.gpu.as_ref().map(|g| g.window_ms).unwrap_or(2000));
        let pick = match args.gpu_index {
            Some(i) => gpus.get(i),
            None => gpus.first(),
        };
        match pick {
            None => {
                println!("Skipping GPU: no usable GPU found (is a graphics driver installed?)");
                body["gpu"] = serde_json::json!({ "error": "no_adapter" });
            }
            // The handle from startup can go stale during the CPU tests; the GPU is looked up again by id.
            Some((_, report)) => {
                let apis = gpu::backends(report);
                if apis.len() > 1 {
                    print!("Benchmarking GPU: {} on {} (compute and 3D, about {}s)… ", report.name, apis.join(" and "), 20 * apis.len());
                } else {
                    print!("Benchmarking GPU: {} (compute and 3D, about 20s)… ", report.name);
                }
                let _ = std::io::stdout().flush();
                match gpu::run_best(report, window) {
                    Ok(mut r) => {
                        println!("done");
                        for alt in &r.alternates {
                            let pct = gpu::shortfall(&r, alt) * 100.0;
                            println!("      kept {}: {} was {:.0}% {}", r.adapter.backend, alt.backend, pct.abs(), if pct >= 0.0 { "slower" } else { "faster" });
                        }
                        println!("      compute: fp32 {:.0} GFLOPS · int32 {:.0} GIOPS · bandwidth {:.0} GB/s · blur {:.1} Gpix/s",
                            r.fp32_gflops, r.int32_giops, r.bandwidth_gbs, r.blur_gpix);
                        println!("      3D: geometry {:.2} Gtris/s · fill {:.1} Gpix/s · texture {:.0} Gtex/s · scene {:.0} fps",
                            r.geometry_gtris, r.fill_gpix, r.texture_gtex, r.scene_fps);
                        r.details = gpu::details::collect(r.adapter.vendor_id, r.adapter.device_id, &r.adapter.name);
                        if let Some(d) = &r.details {
                            let mut parts = Vec::new();
                            if let Some(v) = d.vram_bytes { parts.push(format!("{:.0} GB", v as f64 / (1u64 << 30) as f64)); }
                            if let Some(c) = d.cores { parts.push(format!("{c} cores")); }
                            if let (Some(g), Some(w)) = (d.pcie_gen.or(d.pcie_gen_max), d.pcie_width) { parts.push(format!("PCIe {g}.0 x{w}")); }
                            if let Some(v) = &d.driver_version { parts.push(format!("driver {v}")); }
                            if !parts.is_empty() { println!("      {}", parts.join(" · ")); }
                        }
                        body["gpu"] = serde_json::to_value(&r).unwrap();
                    }
                    Err(e) => {
                        println!("failed: {e}");
                        body["gpu"] = serde_json::json!({ "error": e.chars().take(300).collect::<String>() });
                    }
                }
            }
        }
    }

    if wants("disk") {
        let window = Duration::from_millis(challenge.disk.as_ref().map(|d| d.window_ms).unwrap_or(2000));
        let dir = args.disk_path.clone().or_else(disk::default_dir).unwrap_or_else(std::env::temp_dir);
        let info = disk::drive_info(&dir);
        println!("Benchmarking storage: {} ({})", if info.model.is_empty() { dir.display().to_string() } else { info.model.clone() }, info.bus);
        match disk::run(&dir, &seed, window, |step| {
            print!("\r  {step:<28}");
            let _ = std::io::stdout().flush();
        }) {
            Ok(r) => {
                let mbs = |t: &disk::IoTest| t.bytes as f64 / (t.elapsed_ns as f64 / 1e9) / 1e6;
                let iops = |t: &disk::IoTest| t.ops as f64 / (t.elapsed_ns as f64 / 1e9);
                println!(
                    "\r  seq read {:.0} MB/s · seq write {:.0} MB/s · 4K QD1 read {:.0} IOPS · 4K QD32 read {:.0} IOPS{}",
                    mbs(&r.seq_read), mbs(&r.seq_write), iops(&r.rnd_read_q1), iops(&r.rnd_read_q32),
                    if r.direct { "" } else { " (OS cache couldn't be bypassed: not counted)" }
                );
                body["disk"] = serde_json::to_value(&r).unwrap();
            }
            Err(e) => {
                println!("\r  skipped: {e:<60}");
                body["disk"] = serde_json::json!({ "error": e.chars().take(300).collect::<String>() });
            }
        }
    }

    if wants("ram") {
        print!("Benchmarking memory… ");
        let _ = std::io::stdout().flush();
        let r = ram::run(&seed, sys.memory.total_bytes, logical);
        println!("read {:.1} GB/s · write {:.1} GB/s · copy {:.1} GB/s · latency {:.1} ns", r.read_gbs, r.write_gbs, r.copy_gbs, r.latency_ns);
        body["ram"] = serde_json::to_value(&r).unwrap();
    }

    if args.dry_run {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
        }
        println!("\n--dry-run: nothing was submitted.");
        return;
    }

    print!("\nSubmitting… ");
    let _ = std::io::stdout().flush();
    let done = api.submit(&code, &body).unwrap_or_else(|e| fail(&e));
    println!("done.\n\nYour results: {}\n", done.url);
    if args.open {
        let _ = webbrowser::open(&done.url);
    }
    pause();
}

static PAUSE_AT_END: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pause() {
    if PAUSE_AT_END.load(std::sync::atomic::Ordering::Relaxed) {
        print!("Press Enter to close this window.");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("\nerror: {msg}");
    pause();
    std::process::exit(1);
}
