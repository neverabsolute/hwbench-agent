mod api;
mod cpu;
mod ram;
mod system;
mod util;

use hwbench_kernels::Kernel;
use std::io::Write;
use std::time::Duration;

const DEFAULT_API: &str = "https://hwbench.io";
const COMPONENTS: [&str; 2] = ["cpu", "ram"];

struct Args {
    api: String,
    session: Option<String>,
    selection: Vec<String>,
    open: bool,
    dry_run: bool,
}

fn parse_args() -> Result<Args, lexopt::Error> {
    use lexopt::prelude::*;
    let mut args = Args {
        api: std::env::var("HWBENCH_API").unwrap_or_else(|_| DEFAULT_API.into()),
        session: None,
        selection: vec![],
        open: true,
        dry_run: false,
    };
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Long("api") => args.api = p.value()?.string()?,
            Long("session") | Short('s') => args.session = Some(p.value()?.string()?),
            Long("cpu") => args.selection.push("cpu".into()),
            Long("ram") => args.selection.push("ram".into()),
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
                     USAGE: hwbench [--session CODE] [--cpu] [--ram] [--all]\n\n\
                     --session CODE  use a session created on hwbench.io/run\n\
                     --cpu, --ram    pick components (default: all)\n\
                     --no-open       don't open the results page in a browser\n\
                     --dry-run       run locally and print results, submit nothing\n\
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
    println!("hwbench {} · https://hwbench.io\n", env!("CARGO_PKG_VERSION"));

    let sys = system::collect();
    println!("  CPU   {} ({} cores / {} threads)", sys.cpu.brand, sys.cpu.physical_cores, sys.cpu.logical_cores);
    println!("  RAM   {:.1} GiB", sys.memory.total_bytes as f64 / (1u64 << 30) as f64);
    println!("  OS    {} ({})\n", sys.os_version, sys.arch);

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
        for threads in counts {
            cpu::warm_up(&seed, threads);
            print!("  {:>3} thread{:<2}", threads, if threads == 1 { "" } else { "s" });
            for name in &plan.kernels {
                let k = Kernel::from_name(name).unwrap_or_else(|| fail(&format!("unknown kernel {name}; please update hwbench")));
                let test = cpu::run_test(&seed, k, threads, Duration::from_millis(plan.window_ms));
                print!(" {}={:.0}/s", name, test.chunks_per_sec());
                let _ = std::io::stdout().flush();
                tests.push(test);
            }
            println!();
        }
        body["cpu"] = serde_json::json!({ "tests": tests });
    }

    if wants("ram") {
        print!("Benchmarking memory… ");
        let _ = std::io::stdout().flush();
        let r = ram::run(&seed, sys.memory.total_bytes, logical);
        println!("read {:.1} GB/s · write {:.1} GB/s · copy {:.1} GB/s · latency {:.1} ns", r.read_gbs, r.write_gbs, r.copy_gbs, r.latency_ns);
        body["ram"] = serde_json::to_value(&r).unwrap();
    }

    if args.dry_run {
        println!("\n--dry-run: nothing was submitted.");
        return;
    }

    print!("\nSubmitting… ");
    let _ = std::io::stdout().flush();
    let done = api.submit(&code, &body).unwrap_or_else(|e| fail(&e));
    println!("done.\n\nYour results: {}\n", done.url);
    let _ = done.public_id;
    if args.open {
        let _ = webbrowser::open(&done.url);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("\nerror: {msg}");
    std::process::exit(1);
}
