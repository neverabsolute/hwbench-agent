//! GPU details wgpu doesn't expose: memory size, driver version, clocks, PCIe link and
//! power limit. All sources work without administrator rights:
//! - NVIDIA (Windows, Linux): `nvidia-smi`, installed with the driver.
//! - Linux, any vendor: sysfs PCI attributes; amdgpu also reports VRAM and clock tables.
//! - Windows, any vendor: the display driver's registry key (memory size, driver version).
//! - macOS: `system_profiler` (VRAM on Intel Macs, GPU core count on Apple Silicon).

use serde::Serialize;

#[derive(Serialize, Debug, Default, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GpuDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_clock_max_mhz: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_clock_max_mhz: Option<u32>,
    /// Highest PCIe generation the card and slot support together (when known), else the card's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_gen: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_gen_max: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_width: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_width_max: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_limit_w: Option<f64>,
    /// Apple GPUs: number of GPU cores.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cores: Option<u32>,
    /// Where the details came from: "nvidia-smi", "sysfs", "registry" or "system_profiler".
    pub sources: Vec<&'static str>,
}

impl GpuDetails {
    fn merge(&mut self, o: GpuDetails) {
        macro_rules! fill { ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = o.$f; } )* } }
        fill!(vram_bytes, driver_version, core_clock_max_mhz, memory_clock_max_mhz, pcie_gen, pcie_gen_max, pcie_width, pcie_width_max, power_limit_w, cores);
        for s in o.sources {
            if !self.sources.contains(&s) {
                self.sources.push(s);
            }
        }
    }
}

pub fn collect(vendor_id: u32, device_id: u32, name: &str) -> Option<GpuDetails> {
    let mut d = GpuDetails::default();
    if vendor_id == 0x10de
        && let Some(n) = nvidia_smi(device_id)
    {
        d.merge(n);
    }
    if let Some(p) = platform::collect(vendor_id, device_id, name) {
        d.merge(p);
    }
    (!d.sources.is_empty()).then_some(d)
}

fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
    let t = s.trim();
    if t.starts_with('[') || t.eq_ignore_ascii_case("n/a") {
        return None;
    }
    t.parse().ok()
}

const SMI_FIELDS: &str = "pci.device_id,memory.total,driver_version,clocks.max.graphics,clocks.max.memory,pcie.link.gen.max,pcie.link.gen.gpumax,pcie.link.width.current,pcie.link.width.max,power.limit";

fn nvidia_smi(device_id: u32) -> Option<GpuDetails> {
    let run = |fields: &str| {
        crate::util::command_output(
            std::process::Command::new("nvidia-smi").args([&format!("--query-gpu={fields}"), "--format=csv,noheader,nounits"]),
            std::time::Duration::from_secs(10),
        )
    };
    // Drivers before R510 don't know pcie.link.gen.gpumax and reject the whole query.
    let out = run(SMI_FIELDS).or_else(|| run(&SMI_FIELDS.replace(",pcie.link.gen.gpumax", ",pcie.link.gen.max")))?;
    parse_nvidia_smi(&out, device_id)
}

/// One CSV line per GPU; pick the one whose PCI device id matches the tested adapter.
pub fn parse_nvidia_smi(out: &str, device_id: u32) -> Option<GpuDetails> {
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split(',').map(str::trim).collect()).filter(|r: &Vec<&str>| r.len() >= 10).collect();
    let row = rows
        .iter()
        .find(|r| u32::from_str_radix(r[0].trim_start_matches("0x").trim_start_matches("0X"), 16).ok().map(|v| v >> 16) == Some(device_id))
        .or(if rows.len() == 1 { rows.first() } else { None })?;
    Some(GpuDetails {
        vram_bytes: num::<u64>(row[1]).map(|mib| mib * 1024 * 1024),
        driver_version: num::<String>(row[2]).filter(|s| !s.is_empty()),
        core_clock_max_mhz: num(row[3]),
        memory_clock_max_mhz: num(row[4]),
        pcie_gen: num(row[5]),
        pcie_gen_max: num(row[6]),
        pcie_width: num(row[7]),
        pcie_width_max: num(row[8]),
        power_limit_w: num(row[9]),
        cores: None,
        sources: vec!["nvidia-smi"],
    })
}

/// "16.0 GT/s PCIe" → 4.
#[allow(dead_code)]
fn pcie_gen_from_speed(s: &str) -> Option<u8> {
    let gts: f64 = s.split_whitespace().next()?.parse().ok()?;
    Some(match gts {
        x if x < 3.0 => 1,
        x if x < 6.0 => 2,
        x if x < 10.0 => 3,
        x if x < 20.0 => 4,
        x if x < 40.0 => 5,
        _ => 6,
    })
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn read(p: &Path) -> Option<String> {
        fs::read_to_string(p).ok().map(|s| s.trim().to_string())
    }

    /// Highest clock in an amdgpu DPM table ("0: 500Mhz\n1: 2600Mhz *").
    fn dpm_max(p: &Path) -> Option<u32> {
        read(p)?.lines().filter_map(|l| l.split_whitespace().nth(1)?.to_lowercase().trim_end_matches("mhz").parse().ok()).max()
    }

    pub fn collect(vendor_id: u32, device_id: u32, _name: &str) -> Option<GpuDetails> {
        let hex = |p: &Path, f: &str| read(&p.join(f)).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        let dev = fs::read_dir("/sys/bus/pci/devices").ok()?.flatten().map(|e| e.path()).find(|p| {
            hex(p, "vendor") == Some(vendor_id) && hex(p, "device") == Some(device_id) && hex(p, "class").is_some_and(|c| c >> 16 == 0x03)
        })?;
        let mut d = GpuDetails {
            vram_bytes: read(&dev.join("mem_info_vram_total")).and_then(|s| s.parse().ok()).filter(|v: &u64| *v > 0),
            core_clock_max_mhz: dpm_max(&dev.join("pp_dpm_sclk")),
            memory_clock_max_mhz: dpm_max(&dev.join("pp_dpm_mclk")),
            // The link drops to a lower generation when idle, so report what it can do.
            pcie_gen_max: read(&dev.join("max_link_speed")).and_then(|s| pcie_gen_from_speed(&s)),
            pcie_width: read(&dev.join("current_link_width")).and_then(|s| s.parse().ok()).filter(|w: &u8| *w > 0),
            pcie_width_max: read(&dev.join("max_link_width")).and_then(|s| s.parse().ok()).filter(|w: &u8| *w > 0),
            sources: vec!["sysfs"],
            ..Default::default()
        };
        if let Ok(hw) = fs::read_dir(dev.join("hwmon")) {
            for h in hw.flatten() {
                if let Some(uw) = read(&h.path().join("power1_cap")).and_then(|s| s.parse::<f64>().ok()) {
                    d.power_limit_w = Some((uw / 1e6 * 10.0).round() / 10.0);
                }
            }
        }
        Some(d)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    const DISPLAY_CLASS: &str = r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

    pub fn collect(vendor_id: u32, device_id: u32, _name: &str) -> Option<GpuDetails> {
        let out = crate::util::command_output(
            std::process::Command::new("reg").args(["query", DISPLAY_CLASS, "/s"]),
            std::time::Duration::from_secs(10),
        )?;
        parse_registry(&out, vendor_id, device_id)
    }

    /// `reg query /s` output: a key line, then indented "Name  REG_TYPE  Data" lines.
    pub fn parse_registry(out: &str, vendor_id: u32, device_id: u32) -> Option<GpuDetails> {
        let want = format!("ven_{vendor_id:04x}&dev_{device_id:04x}");
        let mut key_ok = false;
        let mut cur: Vec<(String, String, String)> = Vec::new();
        let mut blocks: Vec<Vec<(String, String, String)>> = Vec::new();
        for line in out.lines() {
            if line.starts_with("HKEY_") {
                if key_ok {
                    blocks.push(std::mem::take(&mut cur));
                }
                cur.clear();
                // Adapter keys end in a four-digit index (…\0000); deeper subkeys are settings.
                key_ok = line.trim().rsplit('\\').next().is_some_and(|k| k.len() == 4 && k.chars().all(|c| c.is_ascii_digit()));
                continue;
            }
            let parts: Vec<&str> = line.trim().splitn(3, "    ").map(str::trim).collect();
            if key_ok && parts.len() == 3 {
                cur.push((parts[0].to_string(), parts[1].to_string(), parts[2].to_string()));
            }
        }
        if key_ok {
            blocks.push(cur);
        }
        let block = blocks.into_iter().find(|b| {
            b.iter().any(|(n, _, v)| n.eq_ignore_ascii_case("MatchingDeviceId") && v.to_lowercase().contains(&want))
        })?;
        let get = |name: &str| block.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name));
        let vram = get("HardwareInformation.qwMemorySize")
            .and_then(|(_, _, v)| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .or_else(|| {
                let (_, t, v) = get("HardwareInformation.MemorySize")?;
                if t == "REG_DWORD" {
                    u64::from_str_radix(v.trim_start_matches("0x"), 16).ok()
                } else {
                    // REG_BINARY, little-endian hex bytes.
                    let bytes: Vec<u8> = (0..v.len() / 2).filter_map(|i| u8::from_str_radix(&v[i * 2..i * 2 + 2], 16).ok()).collect();
                    let mut arr = [0u8; 8];
                    for (i, b) in bytes.iter().take(8).enumerate() {
                        arr[i] = *b;
                    }
                    Some(u64::from_le_bytes(arr))
                }
            })
            .filter(|v| *v > 0);
        Some(GpuDetails {
            vram_bytes: vram,
            driver_version: get("DriverVersion").map(|(_, _, v)| v.clone()),
            sources: vec!["registry"],
            ..Default::default()
        })
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub fn collect(_vendor_id: u32, _device_id: u32, name: &str) -> Option<GpuDetails> {
        let v = crate::util::system_profiler()?;
        let list = v.get("SPDisplaysDataType")?.as_array()?;
        let lname = name.to_lowercase();
        let g = list
            .iter()
            .find(|g| g.get("sppci_model").and_then(|m| m.as_str()).is_some_and(|m| {
                let m = m.to_lowercase();
                lname.contains(&m) || m.contains(&lname)
            }))
            .or(list.first())?;
        let s = |k: &str| g.get(k).and_then(|x| x.as_str());
        let vram = s("spdisplays_vram").or(s("spdisplays_vram_shared")).and_then(|t| {
            let mut it = t.split_whitespace();
            let n: u64 = it.next()?.parse().ok()?;
            Some(match it.next()?.to_uppercase().as_str() { "GB" => n << 30, "MB" => n << 20, _ => return None })
        });
        let cores = s("sppci_cores").and_then(|c| c.parse().ok());
        if vram.is_none() && cores.is_none() {
            return None;
        }
        Some(GpuDetails { vram_bytes: vram, cores, sources: vec!["system_profiler"], ..Default::default() })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    pub fn collect(_: u32, _: u32, _: &str) -> Option<super::GpuDetails> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_picks_matching_gpu() {
        let out = "0x250310DE, 12288, 566.36, 2100, 7501, 4, 4, 16, 16, 170.00\n\
                   0x268410DE, 24564, 566.36, 2520, 10501, 4, 4, 16, 16, 450.00\n";
        let d = parse_nvidia_smi(out, 0x2503).unwrap();
        assert_eq!(d.vram_bytes, Some(12288 * 1024 * 1024));
        assert_eq!(d.driver_version.as_deref(), Some("566.36"));
        assert_eq!((d.pcie_gen, d.pcie_width), (Some(4), Some(16)));
        assert_eq!(d.power_limit_w, Some(170.0));
        assert_eq!(parse_nvidia_smi(out, 0x2684).unwrap().vram_bytes, Some(24564 * 1024 * 1024));
        assert!(parse_nvidia_smi(out, 0x1111).is_none());
    }

    #[test]
    fn nvidia_smi_unsupported_fields() {
        let d = parse_nvidia_smi("0x1C8210DE, 4096, 470.1, [N/A], [N/A], 3, 3, 8, 16, [Not Supported]\n", 0x1C82).unwrap();
        assert_eq!(d.core_clock_max_mhz, None);
        assert_eq!(d.power_limit_w, None);
        assert_eq!((d.pcie_width, d.pcie_width_max), (Some(8), Some(16)));
    }

    #[cfg(windows)]
    #[test]
    fn registry_memory_size() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}\\0000\r\n    MatchingDeviceId    REG_SZ    pci\\ven_10de&dev_2503\r\n    DriverVersion    REG_SZ    32.0.15.6636\r\n    HardwareInformation.qwMemorySize    REG_QWORD    0x300000000\r\n\r\nHKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}\\0000\\Settings\r\n    MatchingDeviceId    REG_SZ    pci\\ven_1002&dev_73bf\r\n";
        let d = super::platform::parse_registry(out, 0x10de, 0x2503).unwrap();
        assert_eq!(d.vram_bytes, Some(12 << 30));
        assert_eq!(d.driver_version.as_deref(), Some("32.0.15.6636"));
        assert!(super::platform::parse_registry(out, 0x1002, 0x73bf).is_none());
    }

    #[test]
    fn link_speed_to_generation() {
        assert_eq!(pcie_gen_from_speed("16.0 GT/s PCIe"), Some(4));
        assert_eq!(pcie_gen_from_speed("8.0 GT/s PCIe"), Some(3));
        assert_eq!(pcie_gen_from_speed("2.5 GT/s PCIe"), Some(1));
    }
}
