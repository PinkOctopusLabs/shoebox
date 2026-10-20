//! The live pipe on the GPU (`docs/EDITING-SPEC.md`, Architecture): one
//! wgpu compute pass that runs [`crate::cpu::apply`] for every output pixel,
//! from the cached demosaiced image in a texture. It uses whatever device
//! it's given, so the app shares the one eframe creates.
//!
//! Tests check it against the CPU reference within 1e-4 on synthetic
//! images; they skip where no GPU adapter is available (Linux CI).
//!
//! A [`Frame`] is cheap to build and the pass is one dispatch, so a slider
//! re-renders the whole live pipe. Stages that cost more (dehaze, local
//! tone, from Phase 2) get their own passes and cached textures, so a
//! slider only re-runs from the first stage it changes.

use crate::cpu::{DISPLAY_GREY, Frame, MID_GREY, Rgb};

const SHADER: &str = include_str!("develop.wgsl");

/// The output texture formats the pipe can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// For read-back (tests, export).
    Rgba32Float,
    /// For display.
    Rgba16Float,
}

impl Output {
    pub fn format(self) -> wgpu::TextureFormat {
        match self {
            Output::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
            Output::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Output::Rgba32Float => "rgba32float",
            Output::Rgba16Float => "rgba16float",
        }
    }
}

/// The compiled pipe for one output format.
pub struct Pipe {
    output: Output,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

/// The demosaiced image on the GPU.
pub struct Source {
    pub texture: wgpu::Texture,
    view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

const WORKGROUP: u32 = 16;

impl Pipe {
    /// Compile the pipe. Validation errors (a driver that rejects the
    /// shader, say) come back as an error rather than wgpu's default panic.
    pub fn new(device: &wgpu::Device, output: Output) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipe = Self::build(device, output);
        match block_on(scope.pop()) {
            Some(e) => Err(format!("the develop shader failed to build: {e}")),
            None => Ok(pipe),
        }
    }

    fn build(device: &wgpu::Device, output: Output) -> Self {
        let code = SHADER.replace("OUT_FORMAT", output.wgsl());
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("develop"),
            source: wgpu::ShaderSource::Wgsl(code.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("develop"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: output.format(),
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("develop"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("develop"),
            layout: Some(&pl),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            output,
            layout,
            pipeline,
        }
    }

    /// Upload a demosaiced image (RGB f32) as an RGBA float texture.
    pub fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, img: &Rgb) -> Source {
        let (width, height) = (img.width as u32, img.height as u32);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("demosaiced"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut bytes = Vec::with_capacity(img.width * img.height * 16);
        for p in img.data.as_chunks::<3>().0 {
            for v in [p[0], p[1], p[2], 1.0] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 16),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        Source {
            texture,
            view,
            width,
            height,
        }
    }

    /// A texture the pipe can write a `frame` into (and copy out of).
    pub fn output_texture(&self, device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("developed"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.output.format(),
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Record the pass that renders `frame` from `src` into `out` (which
    /// must be at least the frame's size). With `display` false it writes
    /// scene-linear Rec.2020 instead of display values.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src: &Source,
        frame: &Frame,
        display: bool,
        out: &wgpu::TextureView,
    ) {
        let params = params(frame, src, display);
        let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        let buffer = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("develop params"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(out),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("develop"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            (frame.width as u32).div_ceil(WORKGROUP),
            (frame.height as u32).div_ceil(WORKGROUP),
            1,
        );
    }

    /// Render `frame` and read it back (an [`Output::Rgba32Float`] pipe):
    /// for tests and export.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &Source,
        frame: &Frame,
        display: bool,
    ) -> Result<Rgb, String> {
        if self.output != Output::Rgba32Float {
            return Err("read-back needs an Rgba32Float pipe".into());
        }
        let (w, h) = (frame.width as u32, frame.height as u32);
        let out = self.output_texture(device, w, h);
        let view = out.create_view(&Default::default());
        let row = (w * 16).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop read-back"),
            size: u64::from(row) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        self.encode(device, &mut encoder, src, frame, display, &view);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &out,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = read.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| e.to_string())?;
        let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let mut img = Rgb::new(frame.width, frame.height);
        for y in 0..frame.height {
            let line = &mapped[y * row as usize..];
            for x in 0..frame.width {
                for c in 0..3 {
                    let at = x * 16 + c * 4;
                    let v =
                        f32::from_le_bytes([line[at], line[at + 1], line[at + 2], line[at + 3]]);
                    img.data[(y * frame.width + x) * 3 + c] = v;
                }
            }
        }
        drop(mapped);
        read.unmap();
        Ok(img)
    }
}

/// The uniform block (`Params` in the shader), as 17 vec4s.
fn params(f: &Frame, src: &Source, display: bool) -> Vec<f32> {
    let row = |r: [f32; 3]| [r[0], r[1], r[2], 0.0];
    let d = &f.display;
    let mut v = Vec::with_capacity(68);
    v.extend(row(f.map[0]));
    v.extend(row(f.map[1]));
    for r in f.colour {
        v.extend(row(r));
    }
    for m in [d.inset, d.outset, d.to_target] {
        for r in m {
            v.extend(row(r));
        }
    }
    v.extend([d.curve.up.0, d.curve.up.1, d.curve.a, d.curve.floor]);
    v.extend([
        DISPLAY_GREY,
        MID_GREY,
        if d.preserve_hue { 1.0 } else { 0.0 },
        if display { 1.0 } else { 0.0 },
    ]);
    v.extend([
        f.width as f32,
        f.height as f32,
        src.width as f32,
        src.height as f32,
    ]);
    v
}

/// A device of our own (tests, the command line, export without a window).
/// None where there's no usable GPU adapter.
pub fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("shoebox develop"),
        ..Default::default()
    }))
    .ok()
}

/// Run a future to completion on this thread (wgpu's native futures are
/// ready immediately or after a device poll).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
