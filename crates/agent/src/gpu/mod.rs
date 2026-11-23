//! GPU compute benchmarks via wgpu (Vulkan / Metal / DX12).
//!
//! Unlike the CPU suite these results carry no proof of work: a GPU does far
//! more work per second than the server could re-check. The server relies on
//! per-model distributions and theoretical-peak ceilings instead.

pub mod details;

use serde::Serialize;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

const WG: u32 = 256;
const INVOCATIONS: u32 = WG * 4096; // ~1M threads: saturates any current GPU
const FP32_FLOP_PER_ITER: f64 = 64.0;
const INT32_OPS_PER_ITER: f64 = 16.0;
const TARGET_DISPATCH: Duration = Duration::from_millis(30);
const MAX_ITERS: u32 = 1 << 22;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdapterReport {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
    pub driver_info: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    /// Operations per dispatch (FLOP, integer ops or bytes moved).
    pub ops_per_dispatch: f64,
    pub best_ns: u64,
    pub median_ns: u64,
    pub runs: u32,
    pub iters: u32,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GpuResult {
    pub adapter: AdapterReport,
    pub fp32_gflops: f64,
    pub int32_giops: f64,
    #[serde(rename = "bandwidthGBs")]
    pub bandwidth_gbs: f64,
    pub buffer_bytes: u64,
    pub tests: std::collections::BTreeMap<&'static str, TestResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<details::GpuDetails>,
}

fn report(info: &wgpu::AdapterInfo) -> AdapterReport {
    AdapterReport {
        name: info.name.trim().to_string(),
        vendor_id: info.vendor,
        device_id: info.device,
        device_type: format!("{:?}", info.device_type),
        backend: format!("{:?}", info.backend),
        driver: info.driver.clone(),
        driver_info: info.driver_info.clone(),
    }
}

fn type_rank(t: wgpu::DeviceType) -> u8 {
    match t {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

fn backend_rank(b: wgpu::Backend) -> u8 {
    match b {
        wgpu::Backend::Metal => 0,
        wgpu::Backend::Dx12 => 1,
        wgpu::Backend::Vulkan => 2,
        _ => 3,
    }
}

/// The same GPU on every graphics API it's available through, freshly enumerated, best first.
/// A handle from the start of the run can go stale (driver reset, power-state change), so
/// the GPU test looks the device up again right before using it.
fn fresh_candidates(target: &AdapterReport) -> Vec<(wgpu::Adapter, AdapterReport)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::PRIMARY;
    let instance = wgpu::Instance::new(desc);
    let mut out: Vec<_> = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY))
        .into_iter()
        .map(|a| {
            let i = a.get_info();
            (a, i)
        })
        .filter(|(_, i)| i.vendor == target.vendor_id && i.device == target.device_id && (target.device_id != 0 || i.name.trim() == target.name))
        .collect();
    out.sort_by_key(|(_, i)| (format!("{:?}", i.backend) != target.backend, backend_rank(i.backend)));
    out.into_iter().map(|(a, i)| (a, report(&i))).collect()
}

/// Run the GPU suite on `target`, retrying on a fresh handle and then on its other graphics
/// API when the device is lost or can't be opened.
pub fn run_with_retry(target: &AdapterReport, window: Duration) -> Result<GpuResult, String> {
    let mut last = String::from("GPU not found when the test started");
    for attempt in 0..2 {
        for (adapter, rep) in fresh_candidates(target) {
            match run(&adapter, rep, window) {
                Ok(r) => return Ok(r),
                Err(e) => last = e,
            }
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_secs(2)); // let the driver finish recovering
        }
    }
    Err(last)
}

/// All distinct GPUs (one entry per physical device), best first.
pub fn list_adapters() -> Vec<(wgpu::Adapter, AdapterReport)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::PRIMARY;
    let instance = wgpu::Instance::new(desc);
    let mut adapters: Vec<_> = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY))
        .into_iter()
        .map(|a| {
            let info = a.get_info();
            (a, info)
        })
        .collect();
    adapters.sort_by_key(|(_, i)| (type_rank(i.device_type), backend_rank(i.backend)));
    let mut seen = std::collections::HashSet::new();
    adapters
        .into_iter()
        .filter(|(_, i)| {
            // Same GPU appears once per backend; vendor/device 0 means unknown, so key on name too.
            seen.insert((i.vendor, i.device, if i.device == 0 { i.name.clone() } else { String::new() }))
        })
        .map(|(a, i)| {
            let r = report(&i);
            (a, r)
        })
        .collect()
}

struct Bench {
    device: wgpu::Device,
    queue: wgpu::Queue,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pipelines: [wgpu::ComputePipeline; 3],
    buffer_bytes: u64,
    /// First device error or loss seen during the run (wgpu would otherwise panic).
    failure: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl Bench {
    fn new(adapter: &wgpu::Adapter) -> Result<Bench, String> {
        let limits = adapter.limits();
        // Bandwidth buffer: as large as allowed, up to 512 MiB, at least 64 MiB.
        let max_binding = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let buffer_bytes = max_binding.min(512 << 20) & !0xFFFF;
        if buffer_bytes < (64 << 20) {
            return Err(format!("GPU allows only {} MiB storage buffers", buffer_bytes >> 20));
        }
        let required_limits = wgpu::Limits {
            max_storage_buffer_binding_size: buffer_bytes,
            max_buffer_size: buffer_bytes,
            ..wgpu::Limits::downlevel_defaults()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("hwbench"),
            required_limits,
            ..Default::default()
        }))
        .map_err(|e| format!("could not open GPU: {e}"))?;
        let failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let f1 = failure.clone();
        device.on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| {
            f1.lock().unwrap().get_or_insert_with(|| e.to_string());
        }));
        let f2 = failure.clone();
        device.set_device_lost_callback(move |reason, msg| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                f2.lock().unwrap().get_or_insert_with(|| format!("GPU was reset during the test ({msg})"));
            }
        });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hwbench kernels"),
            source: wgpu::ShaderSource::Wgsl(include_str!("kernels.wgsl").into()),
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let outf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("outf"),
            size: INVOCATIONS as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // Seed the bandwidth buffer with non-zero data so compression can't help.
        let words = (buffer_bytes / 4) as usize;
        let init: Vec<u32> = (0..words as u32).map(|i| i.wrapping_mul(2654435761)).collect();
        let outu = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("outu"),
            contents: unsafe { std::slice::from_raw_parts(init.as_ptr().cast::<u8>(), init.len() * 4) },
            usage: wgpu::BufferUsages::STORAGE,
        });
        drop(init);

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: false }),
                entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: outf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: outu.as_entire_binding() },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let make = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipelines = [make("fp32"), make("int32"), make("copy")];
        Ok(Bench { device, queue, params, bind_group, pipelines, buffer_bytes, failure })
    }

    /// Run one dispatch and return its wall-clock duration (submit → completion).
    fn dispatch(&self, kernel: usize, iters: u32, workgroups: u32) -> Duration {
        let len = (self.buffer_bytes / 4) as u32;
        let p = [iters, 0x5eed_u32 ^ iters, len, 0];
        self.queue.write_buffer(&self.params, 0, unsafe { std::slice::from_raw_parts(p.as_ptr().cast::<u8>(), 16) });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines[kernel]);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        // Make sure the parameter upload isn't counted in the timing.
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let t = Instant::now();
        let idx = self.queue.submit([enc.finish()]);
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: Some(idx), timeout: None });
        t.elapsed()
    }

    fn check(&self) -> Result<(), String> {
        match self.failure.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn measure(&self, kernel: usize, window: Duration, ops_per_iter_dispatch: impl Fn(u32) -> f64) -> TestResult {
        let workgroups = if kernel == 2 { 4096 } else { INVOCATIONS / WG };
        // Calibrate: grow work until one dispatch is long enough to time reliably.
        let mut iters = if kernel == 2 { 1 } else { 64 };
        if kernel != 2 {
            loop {
                let d = self.dispatch(kernel, iters, workgroups);
                if d >= TARGET_DISPATCH || iters >= MAX_ITERS {
                    break;
                }
                let scale = (TARGET_DISPATCH.as_secs_f64() / d.as_secs_f64().max(1e-6)).clamp(1.5, 16.0);
                iters = ((iters as f64 * scale) as u32).min(MAX_ITERS);
            }
        }
        let mut times = Vec::new();
        let start = Instant::now();
        while start.elapsed() < window || times.len() < 3 {
            times.push(self.dispatch(kernel, iters, workgroups));
            if times.len() >= 2000 || self.failure.lock().unwrap().is_some() {
                break;
            }
        }
        times.sort();
        TestResult {
            ops_per_dispatch: ops_per_iter_dispatch(iters),
            best_ns: times[0].as_nanos() as u64,
            median_ns: times[times.len() / 2].as_nanos() as u64,
            runs: times.len() as u32,
            iters,
        }
    }
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

pub fn run(adapter: &wgpu::Adapter, report: AdapterReport, window: Duration) -> Result<GpuResult, String> {
    let b = Bench::new(adapter)?;
    // Warm up so clocks ramp before measuring.
    let warm = Instant::now();
    while warm.elapsed() < Duration::from_millis(800) {
        b.dispatch(0, 256, INVOCATIONS / WG);
    }
    let inv = INVOCATIONS as f64;
    b.check()?;
    let fp32 = b.measure(0, window, |it| inv * it as f64 * FP32_FLOP_PER_ITER);
    b.check()?;
    let int32 = b.measure(1, window, |it| inv * it as f64 * INT32_OPS_PER_ITER);
    b.check()?;
    let half = (b.buffer_bytes / 2) as f64;
    let copy = b.measure(2, window, |_| 2.0 * half); // read half + write half
    b.check()?;
    let rate = |t: &TestResult| t.ops_per_dispatch / (t.best_ns as f64 / 1e9);
    let mut tests = std::collections::BTreeMap::new();
    let (f, i, c) = (rate(&fp32) / 1e9, rate(&int32) / 1e9, rate(&copy) / 1e9);
    tests.insert("fp32", fp32);
    tests.insert("int32", int32);
    tests.insert("copy", copy);
    Ok(GpuResult { adapter: report, fp32_gflops: f, int32_giops: i, bandwidth_gbs: c, buffer_bytes: b.buffer_bytes, tests, details: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server validates these exact key names (see docs/PROTOCOL.md).
    #[test]
    fn payload_keys_match_protocol() {
        let t = || TestResult { ops_per_dispatch: 1.0, best_ns: 1, median_ns: 1, runs: 1, iters: 1 };
        let mut tests = std::collections::BTreeMap::new();
        tests.insert("fp32", t());
        tests.insert("int32", t());
        tests.insert("copy", t());
        let r = GpuResult {
            adapter: AdapterReport {
                name: "x".into(), vendor_id: 1, device_id: 1, device_type: "DiscreteGpu".into(),
                backend: "Vulkan".into(), driver: String::new(), driver_info: String::new(),
            },
            fp32_gflops: 1.0, int32_giops: 1.0, bandwidth_gbs: 1.0, buffer_bytes: 1, tests, details: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        for k in ["adapter", "fp32Gflops", "int32Giops", "bandwidthGBs", "bufferBytes", "tests"] {
            assert!(v.get(k).is_some(), "missing key {k}: {v}");
        }
        for k in ["name", "vendorId", "deviceId", "deviceType", "backend", "driver", "driverInfo"] {
            assert!(v["adapter"].get(k).is_some(), "missing adapter key {k}");
        }
        for k in ["opsPerDispatch", "bestNs", "medianNs", "runs", "iters"] {
            assert!(v["tests"]["fp32"].get(k).is_some(), "missing test key {k}");
        }
    }
}
