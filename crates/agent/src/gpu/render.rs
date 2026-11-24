//! 3D rendering tests: geometry, fill rate, texturing and a lit, shadowed scene, rendered
//! off-screen at 1920×1080. These stress the parts of a GPU that compute kernels don't
//! (rasterizers, texture units, blending), which is what games depend on.

use super::TestResult;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

const W: u32 = 1920;
const H: u32 = 1080;
const SHADOW: u32 = 2048;
const TEX: u32 = 2048;

// Work per frame; must match render.wgsl and the draw calls below.
const GRID: u32 = 64;
const GEOM_INSTANCES: u32 = 1024;
pub const GEOM_TRIANGLES_PER_FRAME: f64 = (GRID * GRID * 2 * GEOM_INSTANCES) as f64;
const FILL_LAYERS: u32 = 32;
pub const FILL_PIXELS_PER_FRAME: f64 = (W * H * FILL_LAYERS) as f64;
const TEX_LAYERS: u32 = 8;
pub const TEX_SAMPLES_PER_FRAME: f64 = (W * H * TEX_LAYERS * 8) as f64;
const SCENE_INSTANCES: u32 = 1 + 50 * 40; // ground + cubes

const TARGET_BATCH: Duration = Duration::from_millis(30);
const MAX_FRAMES_PER_BATCH: u32 = 512;
/// Software renderers can take seconds per frame; don't let them hold the run hostage.
const SLOW_FRAME: Duration = Duration::from_millis(400);

pub struct Render {
    device: wgpu::Device,
    queue: wgpu::Queue,
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    shadow: wgpu::TextureView,
    geom: wgpu::RenderPipeline,
    fill: wgpu::RenderPipeline,
    texture: wgpu::RenderPipeline,
    shadow_pipe: wgpu::RenderPipeline,
    scene: wgpu::RenderPipeline,
    tex_group: wgpu::BindGroup,
    scene_group: wgpu::BindGroup,
}

fn target(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, extra: wgpu::TextureUsages) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | extra,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

impl Render {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, seed: u32) -> Render {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hwbench 3D"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render.wgsl").into()),
        });
        let color_format = wgpu::TextureFormat::Rgba8Unorm;
        let color = target(device, W, H, color_format, wgpu::TextureUsages::empty());
        let depth = target(device, W, H, wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::empty());
        let shadow = target(device, SHADOW, SHADOW, wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::TEXTURE_BINDING);

        // Noise texture for the texturing test and the scene's surfaces.
        let mut x = seed | 1;
        let texels: Vec<u8> = (0..TEX * TEX * 4)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect();
        let noise = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("noise"),
                size: wgpu::Extent3d { width: TEX, height: TEX, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &texels,
        );
        let noise = noise.create_view(&Default::default());
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let compare = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        let tex_entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty, count: None };
        let float_tex = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[tex_entry(0, float_tex), tex_entry(1, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering))],
        });
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex_entry(0, wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                }),
                tex_entry(1, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison)),
                tex_entry(2, float_tex),
                tex_entry(3, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let tex_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&noise) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&linear) },
            ],
        });
        let scene_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &scene_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&shadow) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&compare) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&noise) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&linear) },
            ],
        });

        let layout = |groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: groups, ..Default::default() })
        };
        let empty = layout(&[]);
        let tex_pl = layout(&[Some(&tex_layout)]);
        let scene_pl = layout(&[Some(&scene_layout)]);
        let depth_state = |write: bool| wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        };
        let pipeline = |pl: &wgpu::PipelineLayout, vs: &str, fs: Option<&str>, blend: Option<wgpu::BlendState>, depth: Option<wgpu::DepthStencilState>| {
            let targets = [Some(wgpu::ColorTargetState { format: color_format, blend, write_mask: wgpu::ColorWrites::ALL })];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth,
                multisample: Default::default(),
                fragment: fs.map(|fs| wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let geom = pipeline(&empty, "vs_geom", Some("fs_flat"), None, Some(depth_state(true)));
        let fill = pipeline(&empty, "vs_full", Some("fs_fill"), Some(wgpu::BlendState::ALPHA_BLENDING), None);
        let texture = pipeline(&tex_pl, "vs_full", Some("fs_tex"), None, None);
        let shadow_pipe = pipeline(&empty, "vs_shadow", None, None, Some(depth_state(true)));
        let scene = pipeline(&scene_pl, "vs_scene", Some("fs_scene"), None, Some(depth_state(true)));
        Render {
            device: device.clone(), queue: queue.clone(), color, depth, shadow,
            geom, fill, texture, shadow_pipe, scene, tex_group, scene_group,
        }
    }

    fn color_pass<'a>(&'a self, enc: &'a mut wgpu::CommandEncoder, with_depth: bool) -> wgpu::RenderPass<'a> {
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: with_depth.then_some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// Encode one frame of `test` (0 geometry, 1 fill, 2 texture, 3 scene).
    fn frame(&self, enc: &mut wgpu::CommandEncoder, test: usize) {
        match test {
            0 => {
                let mut p = self.color_pass(enc, true);
                p.set_pipeline(&self.geom);
                p.draw(0..GRID * GRID * 6, 0..GEOM_INSTANCES);
            }
            1 => {
                let mut p = self.color_pass(enc, false);
                p.set_pipeline(&self.fill);
                p.draw(0..3, 0..FILL_LAYERS);
            }
            2 => {
                let mut p = self.color_pass(enc, false);
                p.set_pipeline(&self.texture);
                p.set_bind_group(0, &self.tex_group, &[]);
                p.draw(0..3, 0..1); // all TEX_LAYERS are sampled inside the one pass (see fs_tex)
            }
            _ => {
                {
                    let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &self.shadow,
                            depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    p.set_pipeline(&self.shadow_pipe);
                    p.draw(0..36, 0..SCENE_INSTANCES);
                }
                let mut p = self.color_pass(enc, true);
                p.set_pipeline(&self.scene);
                p.set_bind_group(0, &self.scene_group, &[]);
                p.draw(0..36, 0..SCENE_INSTANCES);
            }
        }
    }

    /// Time `frames` frames of `test` from submission to completion.
    fn batch(&self, test: usize, frames: u32) -> Duration {
        let mut enc = self.device.create_command_encoder(&Default::default());
        for _ in 0..frames {
            self.frame(&mut enc, test);
        }
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let t = Instant::now();
        let idx = self.queue.submit([enc.finish()]);
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: Some(idx), timeout: None });
        t.elapsed()
    }

    /// Measure `test` for about `window`; `ops_per_frame` converts frames into work.
    pub fn measure(&self, test: usize, window: Duration, ops_per_frame: f64, failed: &dyn Fn() -> bool) -> TestResult {
        // Warm up shaders and caches, then grow the batch until it's long enough to time.
        let first = self.batch(test, 1);
        let mut frames = 1u32;
        if first < SLOW_FRAME {
            loop {
                let d = self.batch(test, frames);
                if d >= TARGET_BATCH || frames >= MAX_FRAMES_PER_BATCH || failed() {
                    break;
                }
                let scale = (TARGET_BATCH.as_secs_f64() / d.as_secs_f64().max(1e-6)).clamp(1.5, 16.0);
                frames = ((frames as f64 * scale).ceil() as u32).min(MAX_FRAMES_PER_BATCH);
            }
        }
        let mut times = Vec::new();
        let start = Instant::now();
        let min_runs = if first < SLOW_FRAME { 3 } else { 1 };
        while start.elapsed() < window || times.len() < min_runs {
            times.push(self.batch(test, frames));
            if times.len() >= 2000 || failed() || (first >= SLOW_FRAME && times.len() >= min_runs) {
                break;
            }
        }
        times.sort();
        TestResult {
            ops_per_dispatch: ops_per_frame * frames as f64,
            best_ns: times[0].as_nanos() as u64,
            median_ns: times[times.len() / 2].as_nanos() as u64,
            runs: times.len() as u32,
            iters: frames,
        }
    }
}

#[cfg(test)]
mod tests {
    /// Both shader files must parse and validate (no GPU needed).
    #[test]
    fn shaders_validate() {
        use wgpu::naga;
        for (name, src) in [("render.wgsl", include_str!("render.wgsl")), ("kernels.wgsl", include_str!("kernels.wgsl"))] {
            let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
}
