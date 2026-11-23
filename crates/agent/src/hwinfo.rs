//! Hardware details beyond model names: memory modules, motherboard and CPU caches.
//! Everything here is readable without administrator rights. Serial numbers and
//! asset tags are never read.
//!
//! Sources: Linux reads udev's copy of the SMBIOS tables (or the raw tables when
//! running as root) and sysfs; Windows reads the SMBIOS tables through
//! GetSystemFirmwareTable; macOS asks system_profiler and sysctl.

use serde::Serialize;

#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Hardware {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,
    /// Prebuilt or laptop model, e.g. "Dell Inc. XPS 15 9520" or "Mac14,2".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_model: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<MemoryModule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cache: Option<CpuCache>,
}

#[derive(Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bios_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bios_date: Option<String>,
}

#[derive(Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryModule {
    pub size_bytes: u64,
    /// "DDR4", "DDR5", "LPDDR5"...
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form_factor: Option<String>,
    /// Highest speed the module reports (MT/s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_mts: Option<u32>,
    /// Speed it is running at (MT/s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_mts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_number: Option<String>,
    /// Slot label, e.g. "DIMM_A2".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u8>,
}

#[derive(Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CpuCache {
    /// Total L2 across all cores, KiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l2_kb: Option<u64>,
    /// Total L3 across all dies, KiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l3_kb: Option<u64>,
}

pub fn collect() -> Hardware {
    let mut hw = platform::collect();
    hw.memory.truncate(32);
    // Self-built desktops repeat the board name as the system model.
    if let (Some(sys), Some(board)) = (&hw.system_model, hw.board.as_ref().and_then(|b| b.model.as_ref()))
        && sys.to_lowercase().contains(&board.to_lowercase())
    {
        hw.system_model = None;
    }
    hw
}

/// Firmware placeholders that mean "not set".
fn clean(s: &str) -> Option<String> {
    let t = s.trim().trim_matches('\0').trim();
    const JUNK: [&str; 14] = [
        "", "unknown", "not specified", "not available", "none", "n/a", "default string", "to be filled by o.e.m.",
        "system product name", "system manufacturer", "o.e.m.", "undefined", "0x0000", "00000000",
    ];
    if JUNK.contains(&t.to_lowercase().as_str()) || t.chars().all(|c| c == '0' || c == ' ') {
        None
    } else {
        Some(t.chars().filter(|c| !c.is_control()).take(64).collect())
    }
}

fn memory_type_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x0F => "SDRAM",
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x20 => "HBM",
        0x21 => "HBM2",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        0x24 => "HBM3",
        _ => return None,
    })
}

fn form_factor_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x09 => "DIMM",
        0x0D => "SODIMM",
        0x0F => "FB-DIMM",
        0x10 => "Die",
        0x0B => "Row of chips",
        0x11 => "CAMM",
        _ => return None,
    })
}

/// Parsed SMBIOS structures relevant to us (types 0, 1, 2 and 17).
#[derive(Default, Debug)]
pub struct Smbios {
    pub board: Option<Board>,
    pub system_model: Option<String>,
    pub memory: Vec<MemoryModule>,
}

/// Parse a raw SMBIOS structure table (no entry-point header).
pub fn parse_smbios(table: &[u8]) -> Smbios {
    let mut out = Smbios::default();
    let mut bios: (Option<String>, Option<String>) = (None, None);
    let mut board: (Option<String>, Option<String>) = (None, None);
    let mut i = 0usize;
    while i + 4 <= table.len() {
        let kind = table[i];
        let len = table[i + 1] as usize;
        if len < 4 || i + len > table.len() {
            break;
        }
        let f = &table[i..i + len];
        // Strings follow the formatted area, each NUL-terminated, ending with a double NUL.
        let mut j = i + len;
        let mut strings: Vec<&[u8]> = Vec::new();
        loop {
            let start = j;
            while j < table.len() && table[j] != 0 {
                j += 1;
            }
            if j >= table.len() {
                break;
            }
            if j == start {
                j += 1;
                if strings.is_empty() && j < table.len() && table[j] == 0 {
                    j += 1; // structure with no strings: two NULs
                }
                break;
            }
            strings.push(&table[start..j]);
            j += 1;
        }
        let s = |off: usize| -> Option<String> {
            let idx = *f.get(off)? as usize;
            if idx == 0 {
                return None;
            }
            clean(&String::from_utf8_lossy(strings.get(idx - 1)?))
        };
        let u16_at = |off: usize| f.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |off: usize| f.get(off..off + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        match kind {
            0 => bios = (s(0x05), s(0x08)),
            1 => {
                let (m, p) = (s(0x04), s(0x05));
                out.system_model = match (m, p) {
                    (Some(m), Some(p)) if p.to_lowercase().starts_with(&m.to_lowercase()) => Some(p),
                    (Some(m), Some(p)) => Some(format!("{m} {p}")),
                    (None, p) => p,
                    _ => None,
                };
            }
            2 => board = (s(0x04), s(0x05)),
            17 => {
                let size_bytes = match u16_at(0x0C) {
                    None | Some(0) | Some(0xFFFF) => 0,
                    Some(0x7FFF) => u32_at(0x1C).map(|mb| (mb & 0x7FFF_FFFF) as u64 * 1024 * 1024).unwrap_or(0),
                    Some(v) if v & 0x8000 != 0 => (v & 0x7FFF) as u64 * 1024,
                    Some(v) => v as u64 * 1024 * 1024,
                };
                if size_bytes == 0 {
                    i = j;
                    continue; // empty slot
                }
                let speed = |off: usize, ext: usize| match u16_at(off) {
                    None | Some(0) => None,
                    Some(0xFFFF) => u32_at(ext).filter(|v| *v > 0),
                    Some(v) => Some(v as u32),
                };
                out.memory.push(MemoryModule {
                    size_bytes,
                    kind: f.get(0x12).and_then(|c| memory_type_name(*c)).map(String::from),
                    form_factor: f.get(0x0E).and_then(|c| form_factor_name(*c)).map(String::from),
                    speed_mts: speed(0x15, 0x54),
                    configured_mts: speed(0x20, 0x58),
                    manufacturer: s(0x17),
                    part_number: s(0x1A),
                    locator: s(0x10),
                    rank: f.get(0x1B).map(|a| a & 0x0F).filter(|r| *r > 0),
                });
            }
            127 => break,
            _ => {}
        }
        i = j;
    }
    if board.0.is_some() || board.1.is_some() || bios.0.is_some() {
        out.board = Some(Board { vendor: board.0, model: board.1, bios_version: bios.0, bios_date: bios.1 });
    }
    out
}

/// systemd's udev rules copy SMBIOS memory records into `/run/udev/data/+dmi:id`
/// as `E:MEMORY_DEVICE_<n>_<FIELD>=value`, readable without root.
pub fn parse_udev_memory(text: &str) -> Vec<MemoryModule> {
    use std::collections::BTreeMap;
    let mut devs: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for line in text.lines() {
        let Some(kv) = line.strip_prefix("E:MEMORY_DEVICE_") else { continue };
        let Some((key, value)) = kv.split_once('=') else { continue };
        let Some((n, field)) = key.split_once('_') else { continue };
        let Ok(n) = n.parse::<u32>() else { continue };
        devs.entry(n).or_default().insert(field.to_string(), value.to_string());
    }
    devs.into_values()
        .filter(|d| d.get("PRESENT").map(|p| p != "0").unwrap_or(true))
        .filter_map(|d| {
            let size_bytes = d.get("SIZE")?.parse::<u64>().ok().filter(|s| *s > 0)?;
            let get = |k: &str| d.get(k).and_then(|v| clean(v));
            let num = |k: &str| d.get(k).and_then(|v| v.parse::<u32>().ok()).filter(|v| *v > 0);
            Some(MemoryModule {
                size_bytes,
                kind: get("TYPE"),
                form_factor: get("FORM_FACTOR"),
                speed_mts: num("SPEED_MTS"),
                configured_mts: num("CONFIGURED_SPEED_MTS"),
                manufacturer: get("MANUFACTURER"),
                part_number: get("PART_NUMBER"),
                locator: get("LOCATOR"),
                rank: num("RANK").map(|r| r as u8),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn read(p: &str) -> Option<String> {
        fs::read_to_string(p).ok().and_then(|s| clean(&s))
    }

    pub fn collect() -> Hardware {
        // Raw tables are root-only; udev's copy is world-readable on systemd distros.
        let smbios = fs::read("/sys/firmware/dmi/tables/DMI").ok().map(|t| parse_smbios(&t));
        let memory = match &smbios {
            Some(s) if !s.memory.is_empty() => s.memory.iter().map(|m| MemoryModule { ..clone_module(m) }).collect(),
            _ => fs::read_to_string("/run/udev/data/+dmi:id").map(|t| parse_udev_memory(&t)).unwrap_or_default(),
        };
        let d = "/sys/class/dmi/id";
        let board = Board {
            vendor: read(&format!("{d}/board_vendor")),
            model: read(&format!("{d}/board_name")),
            bios_version: read(&format!("{d}/bios_version")),
            bios_date: read(&format!("{d}/bios_date")),
        };
        let system_model = match (read(&format!("{d}/sys_vendor")), read(&format!("{d}/product_name"))) {
            (Some(m), Some(p)) if p.to_lowercase().starts_with(&m.to_lowercase()) => Some(p),
            (Some(m), Some(p)) => Some(format!("{m} {p}")),
            (None, p) => p,
            _ => None,
        };
        Hardware {
            board: (board != Board::default()).then_some(board),
            system_model,
            memory,
            cpu_cache: cpu_cache(),
        }
    }

    fn clone_module(m: &MemoryModule) -> MemoryModule {
        MemoryModule {
            size_bytes: m.size_bytes,
            kind: m.kind.clone(),
            form_factor: m.form_factor.clone(),
            speed_mts: m.speed_mts,
            configured_mts: m.configured_mts,
            manufacturer: m.manufacturer.clone(),
            part_number: m.part_number.clone(),
            locator: m.locator.clone(),
            rank: m.rank,
        }
    }

    fn size_kb(s: &str) -> Option<u64> {
        let s = s.trim();
        let (n, mult) = match s.chars().last()? {
            'K' => (&s[..s.len() - 1], 1),
            'M' => (&s[..s.len() - 1], 1024),
            _ => (s, 1),
        };
        n.parse::<u64>().ok().map(|v| v * mult)
    }

    /// Sum each distinct cache (identified by the CPUs that share it).
    fn cpu_cache() -> Option<CpuCache> {
        let mut seen: HashSet<(u8, String)> = HashSet::new();
        let (mut l2, mut l3) = (0u64, 0u64);
        for cpu in fs::read_dir("/sys/devices/system/cpu").ok()?.flatten() {
            let name = cpu.file_name().to_string_lossy().into_owned();
            if !name.starts_with("cpu") || !name[3..].chars().all(|c| c.is_ascii_digit()) || name.len() == 3 {
                continue;
            }
            let Ok(entries) = fs::read_dir(cpu.path().join("cache")) else { continue };
            for idx in entries.flatten() {
                let p = idx.path();
                let r = |f: &str| fs::read_to_string(p.join(f)).ok().map(|s| s.trim().to_string());
                let (Some(level), Some(kind), Some(size), Some(shared)) =
                    (r("level"), r("type"), r("size"), r("shared_cpu_list"))
                else {
                    continue;
                };
                let Ok(level) = level.parse::<u8>() else { continue };
                if kind == "Instruction" || !(level == 2 || level == 3) || !seen.insert((level, shared)) {
                    continue;
                }
                let kb = size_kb(&size).unwrap_or(0);
                if level == 2 { l2 += kb } else { l3 += kb }
            }
        }
        (l2 > 0 || l3 > 0).then_some(CpuCache { l2_kb: (l2 > 0).then_some(l2), l3_kb: (l3 > 0).then_some(l3) })
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemFirmwareTable(sig: u32, id: u32, buf: *mut u8, size: u32) -> u32;
        fn GetLogicalProcessorInformation(buf: *mut u8, len: *mut u32) -> i32;
    }

    pub fn collect() -> Hardware {
        let smbios = firmware_table().map(|t| parse_smbios(&t)).unwrap_or_default();
        Hardware { board: smbios.board, system_model: smbios.system_model, memory: smbios.memory, cpu_cache: cpu_cache() }
    }

    fn firmware_table() -> Option<Vec<u8>> {
        const RSMB: u32 = u32::from_be_bytes(*b"RSMB");
        let size = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
        if size < 8 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let got = unsafe { GetSystemFirmwareTable(RSMB, 0, buf.as_mut_ptr(), size) };
        if got < 8 || got > size {
            return None;
        }
        // RawSMBIOSData: 4 version bytes, u32 table length, then the table.
        let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        Some(buf.get(8..8 + len.min(got as usize - 8))?.to_vec())
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CacheDescriptor {
        level: u8,
        associativity: u8,
        line_size: u16,
        size: u32,
        kind: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union Info {
        cache: CacheDescriptor,
        reserved: [u64; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcessorInfo {
        mask: usize,
        relationship: u32,
        info: Info,
    }

    fn cpu_cache() -> Option<CpuCache> {
        let mut len = 0u32;
        unsafe { GetLogicalProcessorInformation(std::ptr::null_mut(), &mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if unsafe { GetLogicalProcessorInformation(buf.as_mut_ptr(), &mut len) } == 0 {
            return None;
        }
        let step = std::mem::size_of::<ProcessorInfo>();
        let (mut l2, mut l3) = (0u64, 0u64);
        for chunk in buf[..len as usize].chunks_exact(step) {
            let p: ProcessorInfo = unsafe { std::ptr::read_unaligned(chunk.as_ptr() as *const ProcessorInfo) };
            const RELATION_CACHE: u32 = 2;
            const CACHE_INSTRUCTION: u32 = 1;
            if p.relationship != RELATION_CACHE {
                continue;
            }
            let c = unsafe { p.info.cache };
            if c.kind == CACHE_INSTRUCTION {
                continue;
            }
            match c.level {
                2 => l2 += c.size as u64 / 1024,
                3 => l3 += c.size as u64 / 1024,
                _ => {}
            }
        }
        (l2 > 0 || l3 > 0).then_some(CpuCache { l2_kb: (l2 > 0).then_some(l2), l3_kb: (l3 > 0).then_some(l3) })
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn sysctl_u64(name: &str) -> Option<u64> {
        let c = std::ffi::CString::new(name).ok()?;
        let mut v: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let rc = unsafe { libc::sysctlbyname(c.as_ptr(), &mut v as *mut u64 as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        if rc != 0 {
            return None;
        }
        // Some keys are 32-bit.
        Some(if len == 4 { v & 0xFFFF_FFFF } else { v })
    }

    fn sysctl_string(name: &str) -> Option<String> {
        let c = std::ffi::CString::new(name).ok()?;
        let mut len = 0usize;
        unsafe { libc::sysctlbyname(c.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        let rc = unsafe { libc::sysctlbyname(c.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        (rc == 0).then(|| String::from_utf8_lossy(&buf[..len]).trim_end_matches('\0').to_string()).and_then(|s| clean(&s))
    }

    pub fn collect() -> Hardware {
        Hardware {
            board: None,
            system_model: sysctl_string("hw.model"),
            memory: memory(),
            cpu_cache: cpu_cache(),
        }
    }

    fn cpu_cache() -> Option<CpuCache> {
        // Apple Silicon: per performance level, L2 is shared by `cpusperl2` cores.
        let mut l2 = 0u64;
        let levels = sysctl_u64("hw.nperflevels").unwrap_or(0);
        for lvl in 0..levels {
            let k = |f: &str| sysctl_u64(&format!("hw.perflevel{lvl}.{f}"));
            if let (Some(size), Some(cores), Some(per)) = (k("l2cachesize"), k("physicalcpu"), k("cpusperl2")) {
                if per > 0 {
                    l2 += size / 1024 * cores.div_ceil(per);
                }
            }
        }
        if levels == 0 {
            // Intel Macs: L2 per core.
            if let (Some(size), Some(cores)) = (sysctl_u64("hw.l2cachesize"), sysctl_u64("hw.physicalcpu")) {
                l2 = size / 1024 * cores;
            }
        }
        let l3 = sysctl_u64("hw.l3cachesize").map(|b| b / 1024).filter(|v| *v > 0);
        (l2 > 0 || l3.is_some()).then_some(CpuCache { l2_kb: (l2 > 0).then_some(l2), l3_kb: l3 })
    }

    fn memory() -> Vec<MemoryModule> {
        let Some(v) = crate::util::system_profiler() else { return Vec::new() };
        let mut out = Vec::new();
        walk(v.get("SPMemoryDataType").unwrap_or(&serde_json::Value::Null), &mut out);
        out
    }

    fn size_bytes(s: &str) -> Option<u64> {
        let mut it = s.split_whitespace();
        let n: f64 = it.next()?.parse().ok()?;
        let mult = match it.next()?.to_uppercase().as_str() {
            "GB" => 1u64 << 30,
            "MB" => 1u64 << 20,
            "TB" => 1u64 << 40,
            _ => return None,
        };
        Some((n * mult as f64) as u64)
    }

    /// Intel Macs list one item per slot; Apple Silicon reports a single entry
    /// with the total under the `SPMemoryDataType` key.
    fn walk(v: &serde_json::Value, out: &mut Vec<MemoryModule>) {
        match v {
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => {
                if let Some(kind) = o.get("dimm_type").and_then(|t| t.as_str()) {
                    let size = o.get("dimm_size").or_else(|| o.get("SPMemoryDataType")).and_then(|s| s.as_str()).and_then(size_bytes);
                    if let Some(size_bytes) = size {
                        let speed = o.get("dimm_speed").and_then(|s| s.as_str())
                            .and_then(|s| s.split_whitespace().next()?.parse::<u32>().ok());
                        let get = |k: &str| o.get(k).and_then(|s| s.as_str()).and_then(clean);
                        out.push(MemoryModule {
                            size_bytes,
                            kind: clean(kind),
                            form_factor: None,
                            speed_mts: speed,
                            configured_mts: speed,
                            manufacturer: get("dimm_manufacturer"),
                            part_number: get("dimm_part_number"),
                            locator: get("_name"),
                            rank: None,
                        });
                    }
                }
                if let Some(items) = o.get("_items") {
                    walk(items, out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    pub fn collect() -> super::Hardware {
        super::Hardware::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structure(kind: u8, formatted: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![kind, (formatted.len() + 4) as u8, 0, 0];
        v.extend_from_slice(formatted);
        if strings.is_empty() {
            v.extend_from_slice(&[0, 0]);
        } else {
            for s in strings {
                v.extend_from_slice(s.as_bytes());
                v.push(0);
            }
            v.push(0);
        }
        v
    }

    fn memory_device(size_mb: u16, speed: u16, configured: u16) -> Vec<u8> {
        // Formatted area after the 4-byte header, offsets relative to structure start.
        let mut f = vec![0u8; 0x5C - 4];
        let put16 = |f: &mut Vec<u8>, off: usize, v: u16| f[off - 4..off - 2].copy_from_slice(&v.to_le_bytes());
        put16(&mut f, 0x0C, size_mb);
        f[0x0E - 4] = 0x09; // DIMM
        f[0x10 - 4] = 1; // locator string
        f[0x12 - 4] = 0x22; // DDR5
        put16(&mut f, 0x15, speed);
        f[0x17 - 4] = 2; // manufacturer
        f[0x18 - 4] = 3; // serial: must never be read
        f[0x1A - 4] = 4; // part number
        f[0x1B - 4] = 0x02; // dual rank
        put16(&mut f, 0x20, configured);
        f
    }

    #[test]
    fn parses_smbios_memory_board_and_skips_empty_slots() {
        let mut t = Vec::new();
        t.extend(structure(0, &[1, 2, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0], &["American Megatrends", "1.40", "05/12/2024"]));
        t.extend(structure(2, &[1, 2, 0, 0], &["Micro-Star International Co., Ltd.", "MAG B650 TOMAHAWK WIFI (MS-7D75)"]));
        t.extend(structure(17, &memory_device(16384, 4800, 6000), &["DIMM_A2", "Corsair", "SERIAL123", "CMK32GX5M2B6000C36  "]));
        t.extend(structure(17, &memory_device(0, 0, 0), &["DIMM_A1", "Unknown", "Unknown", "Unknown"]));
        t.extend(structure(127, &[], &[]));
        let s = parse_smbios(&t);
        assert_eq!(s.memory.len(), 1);
        let m = &s.memory[0];
        assert_eq!(m.size_bytes, 16 << 30);
        assert_eq!(m.kind.as_deref(), Some("DDR5"));
        assert_eq!(m.form_factor.as_deref(), Some("DIMM"));
        assert_eq!((m.speed_mts, m.configured_mts), (Some(4800), Some(6000)));
        assert_eq!(m.manufacturer.as_deref(), Some("Corsair"));
        assert_eq!(m.part_number.as_deref(), Some("CMK32GX5M2B6000C36"));
        assert_eq!(m.locator.as_deref(), Some("DIMM_A2"));
        assert_eq!(m.rank, Some(2));
        let json = serde_json::to_string(&s.memory).unwrap();
        assert!(!json.contains("SERIAL123"));
        let b = s.board.unwrap();
        assert_eq!(b.model.as_deref(), Some("MAG B650 TOMAHAWK WIFI (MS-7D75)"));
        assert_eq!(b.bios_version.as_deref(), Some("1.40"));
    }

    #[test]
    fn parses_udev_memory_records() {
        let text = "E:MEMORY_ARRAY_NUM_DEVICES=4\n\
            E:MEMORY_DEVICE_0_PRESENT=0\n\
            E:MEMORY_DEVICE_1_SIZE=17179869184\nE:MEMORY_DEVICE_1_TYPE=DDR4\nE:MEMORY_DEVICE_1_SPEED_MTS=3200\n\
            E:MEMORY_DEVICE_1_CONFIGURED_SPEED_MTS=2133\nE:MEMORY_DEVICE_1_PART_NUMBER=F4-3200C16-16GVK\n\
            E:MEMORY_DEVICE_1_SERIAL_NUMBER=ABCDEF\nE:MEMORY_DEVICE_1_MANUFACTURER=G Skill Intl\nE:MEMORY_DEVICE_1_LOCATOR=DIMM_B2\n";
        let m = parse_udev_memory(text);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].configured_mts, Some(2133));
        assert_eq!(m[0].part_number.as_deref(), Some("F4-3200C16-16GVK"));
        assert!(!serde_json::to_string(&m).unwrap().contains("ABCDEF"));
    }

    #[test]
    fn placeholders_are_dropped() {
        assert_eq!(clean("To Be Filled By O.E.M."), None);
        assert_eq!(clean("  0000  "), None);
        assert_eq!(clean(" ASUSTeK COMPUTER INC. "), Some("ASUSTeK COMPUTER INC.".into()));
    }
}
