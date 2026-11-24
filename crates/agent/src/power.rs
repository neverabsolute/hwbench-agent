//! Battery and power-saving detection. A laptop on battery, or in a power-saving mode, can run
//! 30–50% slower than plugged in, so the run reports both and the results page says so.
//! Each field is None when the system doesn't say.

use serde::Serialize;

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Power {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_battery: Option<bool>,
    /// Battery saver, Low Power Mode, a power-saver profile or plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saver: Option<bool>,
}

impl Power {
    pub fn limited(&self) -> bool {
        self.on_battery == Some(true) || self.saver == Some(true)
    }
    pub fn describe(&self) -> String {
        match (self.on_battery == Some(true), self.saver == Some(true)) {
            (true, true) => "running on battery in a power-saving mode".into(),
            (true, false) => "running on battery".into(),
            (false, true) => "in a power-saving mode".into(),
            (false, false) => String::new(),
        }
    }
}

pub fn detect() -> Power {
    imp::detect()
}

#[cfg(windows)]
mod imp {
    use super::Power;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct SystemPowerStatus {
        ac_line_status: u8,
        battery_flag: u8,
        battery_life_percent: u8,
        system_status_flag: u8,
        battery_life_time: u32,
        battery_full_life_time: u32,
    }
    #[repr(C)]
    #[derive(Default, PartialEq, Debug)]
    struct Guid(u32, u16, u16, [u8; 8]);

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
    #[link(name = "powrprof")]
    unsafe extern "system" {
        fn PowerGetActiveScheme(root: *mut c_void, scheme: *mut *mut Guid) -> u32;
    }

    /// The power mode slider ("Best power efficiency" … "Best performance"), Windows 10 1709+.
    /// powrprof.dll exports it but its import library doesn't, so it's looked up at run time.
    fn effective_overlay() -> Option<Guid> {
        let name: Vec<u16> = "powrprof.dll".encode_utf16().chain(Some(0)).collect();
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            return None;
        }
        let f = unsafe { GetProcAddress(module, c"PowerGetEffectiveOverlayScheme".as_ptr().cast()) };
        if f.is_null() {
            return None;
        }
        let f: unsafe extern "system" fn(*mut Guid) -> u32 = unsafe { std::mem::transmute(f) };
        let mut g = Guid::default();
        (unsafe { f(&mut g) } == 0).then_some(g)
    }

    // Power saver plan, and the "Best power efficiency" power mode.
    const POWER_SAVER_PLAN: Guid = Guid(0xa1841308, 0x3541, 0x4fab, [0xbc, 0x81, 0xf7, 0x15, 0x56, 0xf2, 0x0b, 0x4a]);
    const EFFICIENCY_OVERLAY: Guid = Guid(0x961cc777, 0x2547, 0x4f9d, [0x81, 0x74, 0x7d, 0x86, 0x18, 0x1b, 0x8a, 0x7a]);

    pub fn detect() -> Power {
        let mut s = SystemPowerStatus::default();
        let have = unsafe { GetSystemPowerStatus(&mut s) } != 0;
        // ACLineStatus: 0 battery, 1 mains, 255 unknown. BatteryFlag 128: no battery at all.
        let on_battery = if !have || s.ac_line_status == 255 { None } else { Some(s.ac_line_status == 0 && s.battery_flag != 128) };
        let battery_saver = have && s.system_status_flag & 1 == 1;
        let efficiency_mode = effective_overlay().is_some_and(|g| g == EFFICIENCY_OVERLAY);
        let mut plan: *mut Guid = std::ptr::null_mut();
        let saver_plan = unsafe { PowerGetActiveScheme(std::ptr::null_mut(), &mut plan) } == 0 && !plan.is_null() && {
            let is = unsafe { &*plan } == &POWER_SAVER_PLAN;
            unsafe { LocalFree(plan.cast()) };
            is
        };
        Power { on_battery, saver: Some(battery_saver || efficiency_mode || saver_plan) }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Power;
    use std::time::Duration;

    pub fn detect() -> Power {
        let run = |args: &[&str]| crate::util::command_output(std::process::Command::new("pmset").args(args), Duration::from_secs(5));
        let on_battery = run(&["-g", "batt"]).map(|o| parse_source(&o));
        let saver = run(&["-g"]).and_then(|o| parse_low_power(&o));
        Power { on_battery: on_battery.flatten(), saver }
    }

    /// `pmset -g batt`: "Now drawing from 'Battery Power'" or "'AC Power'".
    pub fn parse_source(out: &str) -> Option<bool> {
        let first = out.lines().next()?;
        if first.contains("Battery Power") { Some(true) } else if first.contains("AC Power") { Some(false) } else { None }
    }

    /// `pmset -g`: "lowpowermode 1" (macOS 12–13) or "powermode 1" (macOS 14+, where 1 is Low Power).
    pub fn parse_low_power(out: &str) -> Option<bool> {
        out.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            match (it.next(), it.next()) {
                (Some("lowpowermode") | Some("powermode"), Some(v)) => Some(v == "1"),
                _ => None,
            }
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Power;
    use std::path::Path;
    use std::time::Duration;

    pub fn detect() -> Power {
        Power { on_battery: on_battery(Path::new("/sys/class/power_supply")), saver: saver() }
    }

    /// On battery when a battery is present and no mains or USB-C supply is online.
    pub fn on_battery(root: &Path) -> Option<bool> {
        let read = |p: &Path, f: &str| std::fs::read_to_string(p.join(f)).ok().map(|s| s.trim().to_string());
        let mut battery = false;
        let mut mains_online = false;
        for e in std::fs::read_dir(root).ok()?.flatten() {
            let p = e.path();
            match read(&p, "type").as_deref() {
                Some("Battery") if read(&p, "scope").as_deref() != Some("Device") => battery = true, // not a mouse or headset
                Some("Mains") | Some("USB") if read(&p, "online").as_deref() == Some("1") => mains_online = true,
                _ => {}
            }
        }
        Some(battery && !mains_online)
    }

    /// The firmware's platform profile, or power-profiles-daemon's active profile.
    fn saver() -> Option<bool> {
        if let Ok(p) = std::fs::read_to_string("/sys/firmware/acpi/platform_profile") {
            return Some(matches!(p.trim(), "low-power" | "quiet" | "cool"));
        }
        crate::util::command_output(std::process::Command::new("powerprofilesctl").arg("get"), Duration::from_secs(3))
            .map(|o| o.trim() == "power-saver")
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn detect() -> super::Power {
        super::Power::default()
    }
}

#[cfg(test)]
mod tests {
    use super::Power;

    #[test]
    fn describes_limits() {
        assert!(!Power::default().limited());
        let p = Power { on_battery: Some(true), saver: Some(false) };
        assert!(p.limited());
        assert_eq!(p.describe(), "running on battery");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pmset_parsing() {
        assert_eq!(super::imp::parse_source("Now drawing from 'Battery Power'\n -InternalBattery-0"), Some(true));
        assert_eq!(super::imp::parse_source("Now drawing from 'AC Power'"), Some(false));
        assert_eq!(super::imp::parse_low_power("System-wide power settings:\n lowpowermode         1\n"), Some(true));
        assert_eq!(super::imp::parse_low_power(" powermode            0\n"), Some(false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_supplies() {
        let dir = std::env::temp_dir().join(format!("hwbench-ps-{}", std::process::id()));
        let mk = |name: &str, files: &[(&str, &str)]| {
            let d = dir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            for (f, v) in files {
                std::fs::write(d.join(f), v).unwrap();
            }
        };
        mk("BAT0", &[("type", "Battery\n")]);
        mk("AC", &[("type", "Mains\n"), ("online", "0\n")]);
        assert_eq!(super::imp::on_battery(&dir), Some(true));
        mk("AC", &[("type", "Mains\n"), ("online", "1\n")]);
        assert_eq!(super::imp::on_battery(&dir), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
