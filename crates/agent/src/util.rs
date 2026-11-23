pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

pub fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Run a command and return its stdout if it exits successfully within `timeout`.
/// Vendor tools (nvidia-smi, system_profiler) occasionally hang; a hung tool must
/// not hang the benchmark.
pub fn command_output(cmd: &mut std::process::Command, timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    // Read on another thread so a full pipe can't block the child.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().ok()?;
                return status.success().then(|| String::from_utf8_lossy(&buf).into_owned());
            }
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `system_profiler` output for memory and displays, fetched once (it takes about a second).
#[cfg(target_os = "macos")]
pub fn system_profiler() -> Option<&'static serde_json::Value> {
    static CACHE: std::sync::OnceLock<Option<serde_json::Value>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let text = command_output(
                std::process::Command::new("system_profiler").args(["-json", "SPMemoryDataType", "SPDisplaysDataType"]),
                std::time::Duration::from_secs(15),
            )?;
            serde_json::from_str(&text).ok()
        })
        .as_ref()
}
