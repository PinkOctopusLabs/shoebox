//! The live pipe on the GPU (`docs/EDITING-SPEC.md`, Architecture): one
//! wgpu compute pass that runs [`crate::cpu::apply`] for every output pixel,
//! from the cached demosaiced image in a texture. It uses whatever device
//! it's given, so the app shares the one eframe creates.
//!
//! Tests check it against the CPU reference within 1e-4 on synthetic
//! images; they skip where no GPU adapter is available (Linux CI).
//!
//! A [`Frame`] is cheap to build and the pass is one dispatch, so a slider
//! re-renders the whole live pipe. Modules that need a neighbourhood
//! (dehaze, highlights and shadows, clarity) come as small coefficient
//! maps worked out on the CPU (`cpu::maps`, a fast guided filter) and
//! evaluated here per pixel; the maps are uploaded only when they change.

use crate::cpu::{DISPLAY_GREY, Frame, MID_GREY, Rgb};

const SHADER: &str = include_str!("develop.wgsl");
const HISTOGRAM: &str = include_str!("histogram.wgsl");

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
    /// The last local maps uploaded, and their buffer.
    maps: std::sync::Mutex<Option<(std::sync::Arc<crate::cpu::maps::Maps>, wgpu::Buffer)>>,
    /// The last local adjustment masks uploaded, and their buffer.
    masks: std::sync::Mutex<Option<(std::sync::Arc<crate::mask::Masks>, wgpu::Buffer)>>,
}

/// A read-only storage buffer at `binding`.
fn storage(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A storage buffer holding `bytes` (a placeholder when empty: bindings
/// can't be zero-sized).
fn storage_buffer(device: &wgpu::Device, label: &str, bytes: &[u8]) -> wgpu::Buffer {
    wgpu::util::DeviceExt::create_buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: if bytes.is_empty() { &[0; 16] } else { bytes },
            usage: wgpu::BufferUsages::STORAGE,
        },
    )
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
                storage(3),
                storage(4),
                storage(5),
                storage(6),
                storage(7),
                storage(8),
                storage(9),
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
            maps: Default::default(),
            masks: Default::default(),
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
        // The tone curve's tables (a placeholder when it's off).
        let tone: Vec<u8> = match &frame.display.tone {
            Some(t) => t.iter().flat_map(|v| v.to_le_bytes()).collect(),
            None => vec![0; 16],
        };
        let tone = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("tone curve"),
                contents: &tone,
                usage: wgpu::BufferUsages::STORAGE,
            },
        );
        let maps = self.maps_buffer(device, frame);
        let grid: Vec<u8> = match &frame.lens {
            Some(g) => g.data.iter().flat_map(|v| v.to_le_bytes()).collect(),
            None => vec![0; 28],
        };
        let grid = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("lens grid"),
                contents: &grid,
                usage: wgpu::BufferUsages::STORAGE,
            },
        );
        let lut: Vec<u8> = match &frame.display.look {
            Some(l) => l
                .data
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
            None => vec![0; 12],
        };
        let lut = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("camera look"),
                contents: &lut,
                usage: wgpu::BufferUsages::STORAGE,
            },
        );
        let masks = self.masks_buffer(device, frame);
        let layers: Vec<u8> = frame
            .layers
            .list
            .iter()
            .flat_map(|l| {
                let m = l.matrix;
                let mut v = Vec::with_capacity(20);
                // An inactive layer only shows its mask: identity, no gain.
                for r in m {
                    v.extend([r[0], r[1], r[2], 0.0]);
                }
                v.extend([l.contrast, l.shadows, l.highlights, l.clarity]);
                v.extend([l.dehaze, if l.active { 1.0 } else { 0.0 }, 0.0, 0.0]);
                v
            })
            .flat_map(f32::to_le_bytes)
            .collect();
        let layers = storage_buffer(device, "local layers", &layers);
        let lk = &frame.look;
        let points: Vec<u8> = lk.points[..lk.point_count]
            .iter()
            .flat_map(|p| {
                [
                    p.lightness,
                    p.chroma,
                    p.hue,
                    p.hue_shift,
                    p.saturation,
                    p.luminance,
                    p.hue_width,
                    p.chroma_width,
                    p.lightness_width,
                    0.0,
                    0.0,
                    0.0,
                ]
            })
            .flat_map(f32::to_le_bytes)
            .collect();
        let points = storage_buffer(device, "colour points", &points);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: masks.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: layers.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: points.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: lut.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: grid.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: maps.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tone.as_entire_binding(),
                },
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

    /// The frame's local maps on the GPU: kept while they're the same maps
    /// (dragging a slider that doesn't change them uploads nothing).
    fn maps_buffer(&self, device: &wgpu::Device, frame: &Frame) -> wgpu::Buffer {
        let Some(maps) = &frame.local.maps else {
            return wgpu::util::DeviceExt::create_buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("local maps (none)"),
                    contents: &[0; 16],
                    usage: wgpu::BufferUsages::STORAGE,
                },
            );
        };
        let mut kept = self.maps.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((m, b)) = &*kept
            && std::sync::Arc::ptr_eq(m, maps)
        {
            return b.clone();
        }
        let bytes: Vec<u8> = maps.data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let b = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("local maps"),
                contents: &bytes,
                usage: wgpu::BufferUsages::STORAGE,
            },
        );
        *kept = Some((maps.clone(), b.clone()));
        b
    }

    /// The frame's masks on the GPU, packed four bytes to a word; kept
    /// while they're the same masks.
    fn masks_buffer(&self, device: &wgpu::Device, frame: &Frame) -> wgpu::Buffer {
        let Some(masks) = &frame.layers.masks else {
            return storage_buffer(device, "masks (none)", &[]);
        };
        let mut kept = self.masks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((m, b)) = &*kept
            && std::sync::Arc::ptr_eq(m, masks)
        {
            return b.clone();
        }
        let mut bytes = masks.data.clone();
        bytes.resize(bytes.len().div_ceil(4) * 4, 0);
        let b = storage_buffer(device, "masks", &bytes);
        *kept = Some((masks.clone(), b.clone()));
        b
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

/// Histograms of a rendered texture on the GPU (`cpu::histogram`'s
/// numbers), read back without stalling: [`Histogram::encode`] after the
/// render, then [`Histogram::poll`] on later frames until it answers.
pub struct Histogram {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    bins: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Set by the map callback when the read-back is ready.
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pending: bool,
}

const BINS_BYTES: u64 = 4 * 256 * 4;

impl Histogram {
    pub fn new(device: &wgpu::Device) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("histogram"),
            source: wgpu::ShaderSource::Wgsl(HISTOGRAM.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("histogram"),
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
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("histogram"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("histogram"),
            layout: Some(&pl),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let bins = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram bins"),
            size: BINS_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram read-back"),
            size: BINS_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        if let Some(e) = block_on(scope.pop()) {
            return Err(format!("the histogram shader failed to build: {e}"));
        }
        Ok(Self {
            layout,
            pipeline,
            bins,
            read,
            ready: Default::default(),
            pending: false,
        })
    }

    /// Count `texture` (a render's output, `width`×`height`). Skipped while
    /// an earlier count is still being read back.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> bool {
        if self.pending {
            return false;
        }
        encoder.clear_buffer(&self.bins, 0, None);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("histogram"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.bins.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("histogram"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(width.div_ceil(WORKGROUP), height.div_ceil(WORKGROUP), 1);
        }
        encoder.copy_buffer_to_buffer(&self.bins, 0, &self.read, 0, BINS_BYTES);
        true
    }

    /// After the encoder with [`Self::encode`] is submitted: start reading.
    pub fn start_read(&mut self) {
        let flag = self.ready.clone();
        flag.store(false, std::sync::atomic::Ordering::SeqCst);
        self.read
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                if r.is_ok() {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            });
        self.pending = true;
    }

    /// The counts, once the GPU has them (never blocks; `wait` blocks, for
    /// tests).
    pub fn poll(&mut self, device: &wgpu::Device, wait: bool) -> Option<Box<[[u32; 256]; 4]>> {
        if !self.pending {
            return None;
        }
        let kind = if wait {
            wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            }
        } else {
            wgpu::PollType::Poll
        };
        let _ = device.poll(kind);
        if !self.ready.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let mut h = Box::new([[0u32; 256]; 4]);
        if let Ok(view) = self.read.slice(..).get_mapped_range() {
            for (i, b) in view.as_chunks::<4>().0.iter().enumerate() {
                h[i / 256][i % 256] = u32::from_le_bytes(*b);
            }
        }
        self.read.unmap();
        self.pending = false;
        Some(h)
    }
}

/// The uniform block (`Params` in the shader), as 52 vec4s.
fn params(f: &Frame, src: &Source, display: bool) -> Vec<f32> {
    let row = |r: [f32; 3]| [r[0], r[1], r[2], 0.0];
    let d = &f.display;
    let mut v = Vec::with_capacity(52 * 4);
    v.extend([
        f.map[0][0],
        f.map[0][1],
        f.map[0][2],
        if f.warnings { 1.0 } else { 0.0 },
    ]);
    v.extend([
        f.map[1][0],
        f.map[1][1],
        f.map[1][2],
        if d.referred.is_some() { 1.0 } else { 0.0 },
    ]);
    for r in f.colour {
        v.extend(row(r));
    }
    for m in [d.inset, d.outset, d.to_target] {
        for r in m {
            v.extend(row(r));
        }
    }
    match &d.referred {
        Some(l) => v.extend([l.black, l.white, l.contrast, 0.0]),
        None => v.extend([d.curve.up.0, d.curve.up.1, d.curve.a, d.curve.floor]),
    }
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
    let l = &f.look;
    for m in [l.to_lms, l.from_lms] {
        for r in m {
            v.extend(row(r));
        }
    }
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    v.extend([flag(l.active), l.shadows_tint, l.vibrance, l.saturation]);
    v.extend([l.luma[0], l.luma[1], l.luma[2], flag(l.mono)]);
    for band in [l.hue, l.sat, l.lum, l.mix] {
        v.extend(band);
    }
    v.extend([flag(d.tone.is_some()), 0.0, 0.0, 0.0]);
    let lc = &f.local;
    v.extend([lc.shadows, lc.highlights, lc.clarity, lc.dehaze]);
    let (mw, mh, air) = lc.maps.as_ref().map_or((1.0, 1.0, [1.0; 3]), |m| {
        (m.width as f32, m.height as f32, m.airlight)
    });
    v.extend([flag(lc.maps.is_some()), mw, mh, 0.0]);
    v.extend([lc.scale[0], lc.scale[1], 0.0, 0.0]);
    let air_y = lc.luma[0] * air[0] + lc.luma[1] * air[1] + lc.luma[2] * air[2];
    v.extend([air[0], air[1], air[2], air_y]);
    match &f.lens {
        Some(g) => v.extend([1.0, g.cols as f32, g.rows as f32, flag(g.chromatic)]),
        None => v.extend([0.0, 2.0, 2.0, 0.0]),
    }
    v.extend([f.map[2][0], f.map[2][1], f.map[2][2], 0.0]);
    v.extend([flag(l.fringe), l.purple, l.green, 0.0]);
    match &d.look {
        Some(t) => v.extend([1.0, t.size as f32, 0.0, 0.0]),
        None => v.extend([0.0, 2.0, 0.0, 0.0]),
    }
    let (to, back) = d
        .proof
        .map_or(([[0.0; 3]; 3], [[0.0; 3]; 3]), |p| (p.to, p.back));
    for (i, r) in to.into_iter().enumerate() {
        v.extend([
            r[0],
            r[1],
            r[2],
            if i == 0 { flag(d.proof.is_some()) } else { 0.0 },
        ]);
    }
    for (i, r) in back.into_iter().enumerate() {
        v.extend([
            r[0],
            r[1],
            r[2],
            if i == 0 { flag(f.gamut_warning) } else { 0.0 },
        ]);
    }
    let ls = &f.layers;
    let (lw, lh) = ls
        .masks
        .as_ref()
        .map_or((1.0, 1.0), |m| (m.width as f32, m.height as f32));
    let (shown, only) = f
        .mask_view
        .map_or((0.0, 0.0), |(k, o)| ((k + 1) as f32, flag(o)));
    v.extend([ls.list.len() as f32, lw, lh, shown]);
    v.extend([ls.scale[0], ls.scale[1], only, 0.0]);
    let shown_point = f.point_view.map_or(0.0, |k| (k + 1) as f32);
    v.extend([l.point_count as f32, shown_point, 0.0, 0.0]);
    v.extend([lc.smart, lc.smart_bias, 0.0, 0.0]);
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
