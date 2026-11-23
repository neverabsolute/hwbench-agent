//! Hardware/OS inventory sent with each run. Only model-level information is
//! collected: no serial numbers, usernames, hostnames or MAC addresses.

use serde::Serialize;
use sysinfo::{Disks, System};

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub os: String,
    pub os_version: String,
    pub kernel_version: String,
    pub arch: String,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub disks: Vec<DiskInfo>,
    pub hardware: crate::hwinfo::Hardware,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CpuInfo {
    pub brand: String,
    pub vendor: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    /// Threads this process may actually use (lower under VM/container/affinity limits).
    pub available_threads: usize,
    pub frequency_mhz: u64,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInfo {
    pub total_bytes: u64,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub name: String,
    pub kind: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub removable: bool,
}

pub fn collect() -> SystemInfo {
    let mut sys = System::new_all().expect("unable to query system information");
    sys.refresh_cpu_all();
    let cpus = sys.cpus();
    let first = cpus.first();
    // Only real drives: container hosts report hundreds of overlay/bind mounts.
    const PSEUDO_FS: [&str; 16] = [
        "overlay", "tmpfs", "devtmpfs", "squashfs", "nsfs", "proc", "sysfs", "cgroup", "cgroup2",
        "autofs", "fuse.lxcfs", "ramfs", "efivarfs", "nfs", "cifs", "smb3",
    ];
    let mut seen = std::collections::HashSet::new();
    let disks = Disks::new_with_refreshed_list()
        .map(|d| {
            d
                .iter()
                .filter(|d| {
                    let fs = d.file_system().to_string_lossy().to_lowercase();
                    !PSEUDO_FS.iter().any(|p| fs == *p || fs.starts_with("fuse.")) && d.total_space() > 0
                })
                // Bind mounts repeat the same device under many paths.
                .filter(|d| seen.insert(d.name().to_string_lossy().into_owned()))
                .take(32)
                .map(|d| DiskInfo {
                    name: d.name().to_string_lossy().into_owned(),
                    kind: format!("{:?}", d.kind()),
                    file_system: d.file_system().to_string_lossy().into_owned(),
                    total_bytes: d.total_space(),
                    removable: d.is_removable(),
                })
                .collect()
        })
        .unwrap_or_default();
    SystemInfo {
        os: std::env::consts::OS.to_string(),
        os_version: System::long_os_version().unwrap_or_default(),
        kernel_version: System::kernel_version().unwrap_or_default(),
        arch: std::env::consts::ARCH.to_string(),
        cpu: CpuInfo {
            brand: first.map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            vendor: first.map(|c| c.vendor_id().to_string()).unwrap_or_default(),
            physical_cores: System::physical_core_count().unwrap_or(0),
            logical_cores: cpus.len(),
            available_threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(cpus.len()),
            frequency_mhz: first.map(|c| c.frequency()).unwrap_or(0),
        },
        memory: MemoryInfo { total_bytes: sys.total_memory() },
        disks,
        hardware: crate::hwinfo::collect(),
    }
}

/// Salted hash of the OS machine id. Lets the server link repeat runs from the
/// same machine (for anti-spam and "your runs") without learning the id itself.
pub fn device_id(cpu_brand: &str) -> String {
    let raw = machine_id().unwrap_or_default();
    let mut h = blake3::Hasher::new_derive_key("hwbench.io 2026 device id v1");
    h.update(raw.trim().as_bytes());
    h.update(b"\0");
    h.update(cpu_brand.as_bytes());
    crate::util::hex(&h.finalize().as_bytes()[..16])
}

fn machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/machine-id")
            .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
            .ok()
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find(|l| l.contains("IOPlatformUUID"))?;
        Some(line.split('"').nth(3)?.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("reg")
            .args(["query", r"HKLM\SOFTWARE\Microsoft\Cryptography", "/v", "MachineGuid"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        Some(text.split_whitespace().last()?.to_string())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}
