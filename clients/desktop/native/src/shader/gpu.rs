//! One low-power device for every shader surface, opened on first use, and a
//! renderer per surface that draws offscreen and reads the pixels back.
//!
//! Everything here blocks: the device opens, pipelines compile and frames
//! read back synchronously, so callers run it off the main thread.

use std::borrow::Cow;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::program::{FRAGMENT_ENTRY, Program, UNIFORM_BYTES, Uniforms, VERTEX_ENTRY};

/// Largest side rendered, in pixels. Backdrops are soft and the GPUI atlas
/// keeps images up to 1024 in one shared texture, so a frame never needs a
/// texture of its own.
pub const MAX_SIDE: u32 = 1024;

/// A frame in GPUI's image byte order: tight BGRA rows, straight alpha.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    lost: Arc<AtomicBool>,
    /// Adapter name and API, for Settings and the log.
    pub adapter: String,
}

enum Slot {
    Empty,
    Ready(Arc<Gpu>),
    /// No usable adapter. Not retried: the machine's GPUs do not change
    /// while the app runs.
    Failed(String),
}

static SHARED: Mutex<Slot> = Mutex::new(Slot::Empty);

/// The shared device, opening it if needed. A lost device is replaced.
pub fn shared() -> Result<Arc<Gpu>, String> {
    let mut slot = SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match &*slot {
        Slot::Ready(gpu) if !gpu.lost.load(Ordering::Relaxed) => return Ok(gpu.clone()),
        Slot::Failed(error) => return Err(error.clone()),
        _ => {}
    }
    match Gpu::open() {
        Ok(gpu) => {
            let gpu = Arc::new(gpu);
            log::info!("backdrop shaders on {}", gpu.adapter);
            *slot = Slot::Ready(gpu.clone());
            Ok(gpu)
        }
        Err(error) => {
            log::warn!("backdrop shaders unavailable: {error}");
            *slot = Slot::Failed(error.clone());
            Err(error)
        }
    }
}

impl Gpu {
    fn open() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = smol::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|error| format!("no GPU adapter: {error}"))?;
        let info = adapter.get_info();
        let (device, queue) = smol::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("phux_shader"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .map_err(|error| format!("could not open {}: {error}", info.name))?;
        // wgpu panics on an error nobody captured; a shader must never take
        // the window down.
        device.on_uncaptured_error(Arc::new(|error| log::warn!("shader GPU error: {error}")));
        let lost = Arc::new(AtomicBool::new(false));
        device.set_device_lost_callback({
            let lost = lost.clone();
            move |reason, message| {
                if reason != wgpu::DeviceLostReason::Destroyed {
                    log::warn!("shader GPU lost ({reason:?}): {message}");
                    lost.store(true, Ordering::Relaxed);
                }
            }
        });
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        let format = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]
        .into_iter()
        .find(|format| {
            adapter
                .get_texture_format_features(*format)
                .allowed_usages
                .contains(usage)
        })
        .ok_or_else(|| format!("{} cannot render 8-bit color offscreen", info.name))?;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phux_shader_uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(UNIFORM_BYTES as u64),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("phux_shader"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        Ok(Self {
            device,
            queue,
            layout,
            pipeline_layout,
            format,
            lost,
            adapter: format!("{} ({:?})", info.name, info.backend),
        })
    }

    fn pipeline(&self, program: &Program) -> Result<wgpu::RenderPipeline, String> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&program.name),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(&program.wgsl)),
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&program.name),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(VERTEX_ENTRY),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(FRAGMENT_ENTRY),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        match smol::block_on(scope.pop()) {
            Some(error) => Err(error.to_string()),
            None => Ok(pipeline),
        }
    }
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_row: u32,
}

/// One surface's GPU state. Keeps its pipeline and target across frames.
pub struct Renderer {
    gpu: Arc<Gpu>,
    pipeline: Option<(u64, wgpu::RenderPipeline)>,
    target: Option<Target>,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Renderer {
    /// A renderer on the shared device. Blocks while the device opens.
    pub fn new() -> Result<Self, String> {
        let gpu = shared()?;
        let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phux_shader_uniforms"),
            size: UNIFORM_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("phux_shader_uniforms"),
            layout: &gpu.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        Ok(Self {
            gpu,
            pipeline: None,
            target: None,
            uniforms,
            bind_group,
        })
    }

    /// Whether the device under this renderer is gone; make a new one.
    pub fn lost(&self) -> bool {
        self.gpu.lost.load(Ordering::Relaxed)
    }

    /// Draw `program` at `width` x `height` pixels (clamped to
    /// [`MAX_SIDE`]) and read it back.
    pub fn render(
        &mut self,
        program: &Program,
        uniforms: &Uniforms,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        if self
            .pipeline
            .as_ref()
            .is_none_or(|(hash, _)| *hash != program.hash)
        {
            self.pipeline = Some((program.hash, self.gpu.pipeline(program)?));
        }
        let (width, height) = (width.clamp(1, MAX_SIDE), height.clamp(1, MAX_SIDE));
        if self
            .target
            .as_ref()
            .is_none_or(|target| (target.width, target.height) != (width, height))
        {
            self.target = Some(self.target(width, height));
        }
        let (Some((_, pipeline)), Some(target)) = (&self.pipeline, &self.target) else {
            return Err("renderer not prepared".into());
        };
        let gpu = &self.gpu;
        gpu.queue.write_buffer(&self.uniforms, 0, &uniforms.pack());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("phux_shader"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("phux_shader"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let submission = gpu.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        let slice = target.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(2)),
            })
            .map_err(|error| format!("shader frame did not finish: {error}"))?;
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "shader frame was not read back".to_owned())?
            .map_err(|error| format!("shader frame was not read back: {error}"))?;
        let row = width as usize * 4;
        let mut bgra = Vec::with_capacity(row * height as usize);
        {
            let mapped = slice.get_mapped_range();
            for line in mapped
                .chunks_exact(target.padded_row as usize)
                .take(height as usize)
            {
                bgra.extend_from_slice(&line[..row]);
            }
        }
        target.readback.unmap();
        if gpu.format == wgpu::TextureFormat::Rgba8Unorm {
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        }
        Ok(Frame {
            width,
            height,
            bgra,
        })
    }

    fn target(&self, width: u32, height: u32) -> Target {
        let device = &self.gpu.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("phux_shader_target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.gpu.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phux_shader_readback"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Target {
            texture,
            view,
            readback,
            width,
            height,
            padded_row,
        }
    }
}

#[cfg(test)]
mod tests;
