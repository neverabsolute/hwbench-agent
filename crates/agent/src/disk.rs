//! Storage benchmark: sequential and 4K random I/O against a temporary file on
//! the chosen drive, bypassing the OS page cache.

use hwbench_kernels::{Kernel, Rng};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const ALIGN: usize = 4096;
const SEQ_BLOCK: usize = 1 << 20;
const RND_BLOCK: usize = 4096;
const QD_HIGH: usize = 32;

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DriveInfo {
    pub model: String,
    /// "nvme" | "sata" | "usb" | "sd" | "virtual" | "unknown"
    pub bus: String,
    pub rotational: Option<bool>,
    pub size_bytes: Option<u64>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct IoTest {
    pub bytes: u64,
    pub ops: u64,
    pub elapsed_ns: u64,
    pub queue_depth: u32,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskResult {
    pub drive: DriveInfo,
    /// false when the OS page cache couldn't be bypassed (results not comparable)
    pub direct: bool,
    pub file_bytes: u64,
    pub filesystem: String,
    pub seq_read: IoTest,
    pub seq_write: IoTest,
    pub rnd_read_q1: IoTest,
    pub rnd_write_q1: IoTest,
    pub rnd_read_q32: IoTest,
    pub rnd_write_q32: IoTest,
}

/// Page-aligned buffer (required for unbuffered I/O on every OS).
struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}
unsafe impl Send for AlignedBuf {}
impl AlignedBuf {
    fn new(len: usize) -> AlignedBuf {
        let layout = std::alloc::Layout::from_size_align(len, ALIGN).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory");
        AlignedBuf { ptr, len }
    }
    fn as_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.ptr, std::alloc::Layout::from_size_align(self.len, ALIGN).unwrap()) }
    }
}

/// Deletes the test file however the benchmark ends.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn open_direct(path: &Path, write: bool, create: bool) -> std::io::Result<(File, bool)> {
    let mut o = OpenOptions::new();
    o.read(true).write(write).create(create);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut d = o.clone();
        d.custom_flags(libc::O_DIRECT);
        if let Ok(f) = d.open(path) {
            return Ok((f, true));
        }
        return o.open(path).map(|f| (f, false));
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        let f = o.open(path)?;
        let ok = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) } != -1;
        return Ok((f, ok));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // No FILE_FLAG_WRITE_THROUGH: like Linux O_DIRECT, bypass the OS cache but not the drive's own.
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        let mut d = o.clone();
        d.custom_flags(FILE_FLAG_NO_BUFFERING);
        if let Ok(f) = d.open(path) {
            return Ok((f, true));
        }
        return o.open(path).map(|f| (f, false));
    }
    #[allow(unreachable_code)]
    o.open(path).map(|f| (f, false))
}

fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_at(buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        f.seek_read(buf, off)
    }
}

fn write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_at(buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        f.seek_write(buf, off)
    }
}

/// Canonical path as a comparable string. Windows canonicalize() adds a `\\?\`
/// prefix that mount points don't have, and drive letters compare case-insensitively.
fn comparable(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

/// The mounted filesystem holding `dir` (longest matching mount point).
fn disk_for<T>(dir: &Path, f: impl Fn(&sysinfo::Disk) -> T) -> Option<T> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    let canon = comparable(&std::fs::canonicalize(dir).ok()?);
    disks
        .iter()
        .filter(|d| canon.starts_with(&comparable(d.mount_point())))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(f)
}

pub fn free_space(dir: &Path) -> Option<u64> {
    disk_for(dir, |d| d.available_space())
}

fn filesystem_of(dir: &Path) -> String {
    #[cfg(target_os = "linux")]
    if let Some(fs) = linux::fs_type(dir) {
        return fs.to_string();
    }
    disk_for(dir, |d| d.file_system().to_string_lossy().into_owned()).unwrap_or_default()
}

/// Filesystems whose data lives in RAM: benchmarking them measures memory, not a drive.
pub fn is_ram_backed(dir: &Path) -> bool {
    #[cfg(target_os = "linux")]
    return matches!(linux::fs_type(dir), Some("tmpfs") | Some("ramfs"));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        false
    }
}

/// Where to put the test file by default: the system temp folder, unless it's in
/// RAM (Debian 13 and others mount /tmp as tmpfs), then /var/tmp, then home.
pub fn default_dir() -> Option<PathBuf> {
    let mut candidates = vec![std::env::temp_dir()];
    if cfg!(unix) {
        candidates.push(PathBuf::from("/var/tmp"));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        candidates.push(PathBuf::from(home));
    }
    candidates.into_iter().find(|d| d.is_dir() && !is_ram_backed(d) && writable(d))
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".hwbench-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Benchmark the drive holding `dir`. `window` bounds each timed test.
pub fn run(dir: &Path, seed: &[u8; 32], window: Duration, progress: impl Fn(&str)) -> Result<DiskResult, String> {
    if is_ram_backed(dir) {
        return Err(format!("{} is stored in RAM (tmpfs), so it can't measure a drive; pass --disk-path with a folder on a real disk", dir.display()));
    }
    let free = free_space(dir).unwrap_or(0);
    let file_bytes: u64 = if free >= 8 << 30 { 1 << 30 } else if free >= 2 << 30 { 256 << 20 } else {
        return Err(format!("not enough free space on {} (need 2 GiB)", dir.display()));
    };
    let path = dir.join(format!(".hwbench-{}.tmp", std::process::id()));
    let _guard = TempFile(path.clone());

    // Incompressible data so drives with transparent compression can't inflate results.
    let mut block = AlignedBuf::new(SEQ_BLOCK);
    Rng::derive(seed, Kernel::Hash, 0xD15C).fill_bytes(block.as_mut());

    // Sequential write (also creates the file the other tests read).
    progress("sequential write");
    let (f, direct) = open_direct(&path, true, true).map_err(|e| format!("can't create test file: {e}"))?;
    f.set_len(file_bytes).map_err(|e| e.to_string())?;
    let seq_write = timed_passes(window, file_bytes, |off| write_at(&f, block.as_ref(), off).map(|_| ()))?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);

    progress("sequential read");
    let (f, _) = open_direct(&path, false, false).map_err(|e| e.to_string())?;
    let mut rbuf = AlignedBuf::new(SEQ_BLOCK);
    let seq_read = timed_passes(window, file_bytes, |off| read_at(&f, rbuf.as_mut(), off).map(|_| ()))?;
    drop(f);

    progress("4K random read QD1");
    let rnd_read_q1 = random_io(&path, file_bytes, seed, false, 1, window)?;
    progress("4K random write QD1");
    let rnd_write_q1 = random_io(&path, file_bytes, seed, true, 1, window)?;
    progress("4K random read QD32");
    let rnd_read_q32 = random_io(&path, file_bytes, seed, false, QD_HIGH, window)?;
    progress("4K random write QD32");
    let rnd_write_q32 = random_io(&path, file_bytes, seed, true, QD_HIGH, window)?;

    Ok(DiskResult {
        drive: drive_info(dir),
        direct,
        file_bytes,
        filesystem: filesystem_of(dir),
        seq_read,
        seq_write,
        rnd_read_q1,
        rnd_write_q1,
        rnd_read_q32,
        rnd_write_q32,
    })
}

/// Repeat full sequential passes over the file until `window` has elapsed (at least one pass).
fn timed_passes(window: Duration, file_bytes: u64, mut op: impl FnMut(u64) -> std::io::Result<()>) -> Result<IoTest, String> {
    let start = Instant::now();
    let mut bytes = 0u64;
    let mut ops = 0u64;
    loop {
        let mut off = 0;
        while off < file_bytes {
            op(off).map_err(|e| format!("I/O error: {e}"))?;
            off += SEQ_BLOCK as u64;
            bytes += SEQ_BLOCK as u64;
            ops += 1;
            if start.elapsed() > window * 4 {
                break; // very slow drive: stop partway rather than run for minutes
            }
        }
        if start.elapsed() >= window || start.elapsed() > window * 4 {
            break;
        }
    }
    Ok(IoTest { bytes, ops, elapsed_ns: start.elapsed().as_nanos() as u64, queue_depth: 1 })
}

/// 4K random I/O with `qd` threads, each with its own handle (synchronous
/// handles serialize I/O on Windows) and its own random offset stream.
fn random_io(path: &Path, file_bytes: u64, seed: &[u8; 32], write: bool, qd: usize, window: Duration) -> Result<IoTest, String> {
    let blocks = file_bytes / RND_BLOCK as u64;
    let stop = AtomicBool::new(false);
    let start = Instant::now();
    let results: Vec<Result<u64, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..qd)
            .map(|t| {
                let stop = &stop;
                s.spawn(move || -> Result<u64, String> {
                    let (f, _) = open_direct(path, write, false).map_err(|e| e.to_string())?;
                    let mut buf = AlignedBuf::new(RND_BLOCK);
                    let mut rng = Rng::derive(seed, Kernel::Sort, 0xD000 + t as u32 + if write { 0x100 } else { 0 });
                    if write {
                        rng.fill_bytes(buf.as_mut());
                    }
                    let mut ops = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let off = (rng.next_u64() % blocks) * RND_BLOCK as u64;
                        if write {
                            write_at(&f, buf.as_ref(), off).map_err(|e| format!("I/O error: {e}"))?;
                        } else {
                            read_at(&f, buf.as_mut(), off).map_err(|e| format!("I/O error: {e}"))?;
                        }
                        ops += 1;
                    }
                    Ok(ops)
                })
            })
            .collect();
        std::thread::sleep(window);
        stop.store(true, Ordering::Relaxed);
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let elapsed = start.elapsed();
    let mut ops = 0;
    for r in results {
        ops += r?;
    }
    Ok(IoTest { bytes: ops * RND_BLOCK as u64, ops, elapsed_ns: elapsed.as_nanos() as u64, queue_depth: qd as u32 })
}

/// Hypervisor disks report a bus (often "SATA" or "NVMe") that says nothing about real hardware.
fn is_virtual_model(model: &str) -> bool {
    let m = model.to_lowercase();
    ["virtual disk", "virtual_disk", "qemu", "vbox", "vmware", "virtio", "xen", "msft virtual", "amazon elastic", "google persistentdisk", "parallels"]
        .iter()
        .any(|v| m.contains(v))
}

/// Best-effort physical drive identification for the drive holding `dir`.
pub fn drive_info(dir: &Path) -> DriveInfo {
    #[cfg(target_os = "linux")]
    let mut info = linux::drive_info(dir).unwrap_or_default();
    #[cfg(target_os = "macos")]
    let mut info = macos::drive_info(dir).unwrap_or_default();
    #[cfg(windows)]
    let mut info = windows::drive_info(dir).unwrap_or_default();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let mut info = { let _ = dir; DriveInfo::default() };
    if is_virtual_model(&info.model) {
        info.bus = "virtual".into();
    }
    info
}

#[cfg(target_os = "linux")]
mod linux {
    use super::DriveInfo;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    /// Filesystem type from statfs(2): authoritative, unlike matching mount points.
    pub fn fs_type(dir: &Path) -> Option<&'static str> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some(match st.f_type as u64 {
            0xEF53 => "ext4",
            0x5846_5342 => "xfs",
            0x9123_683E => "btrfs",
            0x2FC1_2FC1 => "zfs",
            0x0102_1994 => "tmpfs",
            0x8584_58F6 => "ramfs",
            0x794C_7630 => "overlay",
            0xF2F5_2010 => "f2fs",
            0x6969 => "nfs",
            0xFF53_4D42 => "cifs",
            0x6573_5546 => "fuse",
            0x4D44 => "vfat",
            0x2011_BAB0 => "exfat",
            0x5346_544E => "ntfs",
            _ => "other",
        })
    }

    fn read(p: impl AsRef<Path>) -> Option<String> {
        std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Follow partitions and device-mapper (LVM, LUKS) down to a physical disk in /sys/block.
    fn physical(mut dev: PathBuf) -> Option<PathBuf> {
        for _ in 0..8 {
            let slaves: Vec<_> = std::fs::read_dir(dev.join("slaves")).ok().map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            if let Some(s) = slaves.first() {
                dev = std::fs::canonicalize(s).ok()?;
                continue;
            }
            if dev.join("partition").exists() {
                dev = dev.parent()?.to_path_buf();
                continue;
            }
            return Some(dev);
        }
        None
    }

    pub fn drive_info(dir: &Path) -> Option<DriveInfo> {
        let rdev = std::fs::metadata(dir).ok()?.dev();
        let (maj, min) = (libc::major(rdev), libc::minor(rdev));
        let dev = physical(std::fs::canonicalize(format!("/sys/dev/block/{maj}:{min}")).ok()?)?;
        let name = dev.file_name()?.to_string_lossy().into_owned();
        let real = std::fs::canonicalize(&dev).ok()?.to_string_lossy().into_owned();
        let bus = if name.starts_with("nvme") { "nvme" } else if real.contains("/usb") { "usb" } else if name.starts_with("mmcblk") { "sd" }
            else if name.starts_with("vd") || name.starts_with("xvd") || real.contains("virtio") { "virtual" } else if name.starts_with("sd") { "sata" } else { "unknown" };
        let model = read(dev.join("device/model")).or_else(|| read(dev.join("device/name"))).unwrap_or_default();
        let vendor = read(dev.join("device/vendor")).filter(|v| v != "ATA" && !model.starts_with(v.as_str()));
        Some(DriveInfo {
            model: match vendor { Some(v) => format!("{v} {model}"), None => model },
            bus: bus.into(),
            rotational: read(dev.join("queue/rotational")).map(|r| r == "1"),
            size_bytes: read(dev.join("size")).and_then(|s| s.parse::<u64>().ok()).map(|s| s * 512),
        })
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::DriveInfo;
    use std::path::Path;

    fn diskutil(target: &str) -> Option<String> {
        let out = std::process::Command::new("diskutil").args(["info", target]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn field(text: &str, k: &str) -> Option<String> {
        text.lines().find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
    }

    pub fn drive_info(dir: &Path) -> Option<DriveInfo> {
        // `diskutil info` wants a device or mount point, not an arbitrary folder.
        let df = std::process::Command::new("df").arg(dir).output().ok()?;
        let dev = String::from_utf8_lossy(&df.stdout).lines().nth(1)?.split_whitespace().next()?.to_string();
        let mut text = diskutil(&dev)?;
        // APFS volumes live in a container; the physical SSD is its "Physical Store".
        if let Some(store) = field(&text, "APFS Physical Store:") {
            if let Some(t) = diskutil(&store) {
                text = t;
            }
        }
        // Partition → whole disk for the model name.
        if field(&text, "Device / Media Name:").is_none() {
            if let Some(whole) = field(&text, "Part of Whole:").and_then(|w| diskutil(&w)) {
                text = whole;
            }
        }
        let protocol = field(&text, "Protocol:").unwrap_or_default().to_lowercase();
        Some(DriveInfo {
            model: field(&text, "Device / Media Name:").unwrap_or_default(),
            bus: if protocol.contains("usb") || protocol.contains("thunderbolt") { "usb" }
                else if protocol.contains("pci") || protocol.contains("apple fabric") || protocol.contains("nvme") { "nvme" }
                else if protocol.contains("sata") { "sata" }
                else if protocol.contains("virtual") { "virtual" }
                else { "unknown" }.into(),
            rotational: field(&text, "Solid State:").map(|v| v != "Yes"),
            size_bytes: field(&text, "Disk Size:").and_then(|v| v.split('(').nth(1)?.split_whitespace().next()?.parse().ok()),
        })
    }
}

#[cfg(windows)]
mod windows {
    //! Drive model, bus, media type and size straight from the storage driver, the way
    //! disk utilities read them: open the volume and its physical disk with no access rights
    //! (no administrator needed) and query them. PowerShell's Get-Disk is the fallback; it
    //! failed outright on some machines (the drive then showed as "Unknown drive").
    use super::DriveInfo;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetVolumePathNameW(file: *const u16, out: *mut u16, len: u32) -> i32;
        fn GetVolumeNameForVolumeMountPointW(mount: *const u16, out: *mut u16, len: u32) -> i32;
        fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *mut c_void, disposition: u32, flags: u32, template: *mut c_void) -> *mut c_void;
        fn DeviceIoControl(h: *mut c_void, code: u32, inb: *const c_void, insz: u32, outb: *mut c_void, outsz: u32, ret: *mut u32, ov: *mut c_void) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
    }

    const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
    const STORAGE_DEVICE_PROPERTY: u32 = 0;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }
    fn from_wide(buf: &[u16]) -> String {
        String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
    }

    struct Handle(*mut c_void);
    impl Handle {
        fn open(path: &str) -> Option<Handle> {
            // Zero access rights: enough for these queries, and works without administrator rights.
            let h = unsafe { CreateFileW(wide(path).as_ptr(), 0, 3, std::ptr::null_mut(), 3, 0, std::ptr::null_mut()) };
            (h as isize != -1 && !h.is_null()).then_some(Handle(h))
        }
        fn ioctl(&self, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
            let mut got = 0u32;
            let ok = unsafe {
                DeviceIoControl(self.0, code, input.as_ptr().cast(), input.len() as u32, out.as_mut_ptr().cast(), out.len() as u32, &mut got, std::ptr::null_mut())
            };
            (ok != 0).then_some(got as usize)
        }
        fn property(&self, id: u32, out: &mut [u8]) -> Option<usize> {
            let mut q = [0u8; 12]; // STORAGE_PROPERTY_QUERY { PropertyId, QueryType = standard, AdditionalParameters }
            q[..4].copy_from_slice(&id.to_le_bytes());
            self.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &q, out)
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        b.get(at..at + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).unwrap_or(0)
    }
    fn cstr_at(b: &[u8], offset: u32) -> String {
        let o = offset as usize;
        if o == 0 || o >= b.len() {
            return String::new();
        }
        let end = b[o..].iter().position(|&c| c == 0).map_or(b.len(), |n| o + n);
        String::from_utf8_lossy(&b[o..end]).trim().to_string()
    }

    /// STORAGE_BUS_TYPE → the agent's bus names.
    pub fn bus_name(bus_type: u32) -> &'static str {
        match bus_type {
            17 => "nvme",
            3 | 11 => "sata",
            7 => "usb",
            12 | 13 => "sd",
            14 | 15 => "virtual",
            _ => "unknown",
        }
    }

    /// Vendor and product from a STORAGE_DEVICE_DESCRIPTOR; "NVMe"/"ATA" vendor fields are bus names, not brands.
    pub fn model_from_descriptor(b: &[u8]) -> (String, u32) {
        let vendor = cstr_at(b, u32_at(b, 12));
        let product = cstr_at(b, u32_at(b, 16));
        let vendor = if vendor.eq_ignore_ascii_case("nvme") || vendor.eq_ignore_ascii_case("ata") { String::new() } else { vendor };
        let model = [vendor, product].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        (model, u32_at(b, 28))
    }

    fn native(dir: &Path) -> Option<DriveInfo> {
        let path = std::fs::canonicalize(dir).ok()?;
        let mut mount = [0u16; 512];
        if unsafe { GetVolumePathNameW(wide(&path.to_string_lossy()).as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0 {
            return None;
        }
        let mut volume = [0u16; 128];
        if unsafe { GetVolumeNameForVolumeMountPointW(mount.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0 {
            return None;
        }
        let volume = from_wide(&volume); // \\?\Volume{GUID}\ ; the device is the same name without the slash
        let vol = Handle::open(volume.trim_end_matches('\\'))?;
        let mut extents = [0u8; 8 + 24 * 8];
        vol.ioctl(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut extents)?;
        if u32_at(&extents, 0) == 0 {
            return None;
        }
        let disk = Handle::open(&format!(r"\\.\PhysicalDrive{}", u32_at(&extents, 8)))?;
        let mut desc = [0u8; 1024];
        let n = disk.property(STORAGE_DEVICE_PROPERTY, &mut desc)?;
        let (model, bus_type) = model_from_descriptor(&desc[..n]);
        let mut seek = [0u8; 12];
        let rotational = disk.property(STORAGE_DEVICE_SEEK_PENALTY_PROPERTY, &mut seek).filter(|&n| n >= 9).map(|_| seek[8] != 0);
        let mut geo = [0u8; 256];
        let size_bytes = disk.ioctl(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geo).filter(|&n| n >= 32)
            .map(|_| i64::from_le_bytes(geo[24..32].try_into().unwrap())).filter(|&v| v > 0).map(|v| v as u64);
        if model.is_empty() {
            return None;
        }
        Some(DriveInfo { model, bus: bus_name(bus_type).into(), rotational, size_bytes })
    }

    fn powershell(dir: &Path) -> Option<DriveInfo> {
        let letter = std::fs::canonicalize(dir).ok()?.to_string_lossy().trim_start_matches(r"\\?\").chars().next()?;
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        let script = format!(
            "$d = Get-Partition -DriveLetter {letter} | Get-Disk; $p = Get-PhysicalDisk | Where-Object DeviceId -eq $d.Number; \
             \"$($d.FriendlyName)|$($d.BusType)|$($p.MediaType)|$($d.Size)\""
        );
        let out = crate::util::command_output(
            std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]),
            std::time::Duration::from_secs(10),
        )?;
        let line = out.lines().next().unwrap_or("").trim().to_string();
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 4 || parts[0].trim().is_empty() {
            return None;
        }
        let bus = parts[1].to_lowercase();
        Some(DriveInfo {
            model: parts[0].trim().to_string(),
            bus: if bus.contains("nvme") { "nvme" } else if bus.contains("usb") { "usb" } else if bus.contains("sata") || bus.contains("ata") { "sata" } else if bus.contains("sd") || bus.contains("mmc") { "sd" } else { "unknown" }.into(),
            rotational: match parts[2].to_lowercase().as_str() { "hdd" => Some(true), "ssd" => Some(false), _ => None },
            size_bytes: parts[3].trim().parse().ok(),
        })
    }

    pub fn drive_info(dir: &Path) -> Option<DriveInfo> {
        native(dir).or_else(|| powershell(dir))
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn reads_the_temp_drive() {
            let info = super::drive_info(&std::env::temp_dir()).expect("drive info");
            assert!(!info.model.is_empty(), "{info:?}");
            assert!(info.size_bytes.unwrap_or(0) > 1 << 30, "{info:?}");
        }

        #[test]
        fn descriptor_parsing() {
            let mut b = vec![0u8; 96];
            b[12..16].copy_from_slice(&40u32.to_le_bytes());
            b[16..20].copy_from_slice(&48u32.to_le_bytes());
            b[28..32].copy_from_slice(&17u32.to_le_bytes());
            b[40..44].copy_from_slice(b"NVMe");
            b[48..71].copy_from_slice(b"Samsung SSD 990 PRO 2TB");
            assert_eq!(super::model_from_descriptor(&b), ("Samsung SSD 990 PRO 2TB".to_string(), 17));
            assert_eq!(super::bus_name(17), "nvme");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn virtual_models() {
        for m in ["Msft Virtual Disk", "Virtual_Disk NVME Premium", "QEMU HARDDISK", "VBOX HARDDISK", "Amazon Elastic Block Store"] {
            assert!(super::is_virtual_model(m), "{m}");
        }
        assert!(!super::is_virtual_model("Samsung SSD 980 PRO 2TB"));
    }

    #[test]
    fn comparable_strips_verbatim_prefix() {
        let p = std::path::Path::new(r"\\?\C:\Users\x");
        assert!(super::comparable(p).ends_with(r"c:\users\x") || super::comparable(p).ends_with(r"C:\Users\x"));
    }
}
