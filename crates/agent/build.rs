//! Windows: embeds version information (product, publisher, description, version) in the .exe.
//! Executables without it look anonymous to antivirus heuristics and in Windows' file
//! properties. Done only when building on Windows, where the resource compiler lives.

fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = env!("CARGO_PKG_VERSION");
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "hwbench")
            .set("FileDescription", "hwbench.io hardware benchmark")
            .set("CompanyName", "hwbench.io")
            .set("LegalCopyright", "MIT License. Source: github.com/neverabsolute/hwbench-agent")
            .set("OriginalFilename", "hwbench.exe")
            .set("InternalName", "hwbench")
            .set("Comments", "Benchmarks this computer's CPU, GPU and storage and submits the results to hwbench.io.")
            .set("FileVersion", version)
            .set("ProductVersion", version);
        res.compile().expect("embedding Windows version information");
    }
}
