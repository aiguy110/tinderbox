//! GPU ping-pong Drossel–Schwabl forest-fire simulation.
//!
//! The grid covers the whole layout (every monitor), with one Rgba8Uint
//! texel per cell; see `shaders/common.wgsl` for the encoding. Each step is
//! one fullscreen pass from one texture into the other.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::rng::Rng;
use crate::shaders::{self, MAX_STRIKES, SETTLED, cause, state};

pub const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Uint;
/// The river field (see river.rs).
const FIELD_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Lightning strikes per second that get a bolt. Strikes are split between a
/// short list picked here (so we know where they land) and the rest, which the
/// step shader rolls per cell; together they're the same Poisson process.
const BOLTS_PER_SECOND: f64 = 3.0;

pub struct StepParams {
    pub growth: f32,
    pub lightning: f32,
    pub moore: bool,
    pub wrap: bool,
    pub steps_per_second: f32,
    pub spotting: f32,
    /// River width in cells.
    pub river_w: f32,
}

/// A cell that lightning set alight.
#[derive(Clone, Copy, Debug)]
pub struct Strike {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StepUniforms {
    seed: u32,
    growth: f32,
    lightning: f32,
    moore: u32,
    wrap: u32,
    strike_count: u32,
    spotting: f32,
    river_w: f32,
    strikes: [[i32; 4]; MAX_STRIKES],
}

struct Field {
    tex: wgpu::Texture,
    bind: wgpu::BindGroup,
}

struct Buffer {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

/// The listed strikes of one step, waiting for the probe's readback to say
/// which of them lit a tree. Mapping is asynchronous (and on the web, only
/// completes after we yield to the browser), so bolts appear a frame or two
/// after their step, which nobody can see.
struct Readback {
    buf: wgpu::Buffer,
    strikes: Vec<Strike>,
    /// 0 pending, 1 mapped, 2 failed.
    status: Arc<AtomicU8>,
}

/// A copy of the grid on the CPU, for saving: rows of RGBA texels, y up.
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub generation: u64,
    pub cells: Vec<u8>,
}

/// A grid being copied back for a snapshot.
struct Capture {
    buf: wgpu::Buffer,
    width: u32,
    height: u32,
    generation: u64,
    status: Arc<AtomicU8>,
}

pub struct Sim {
    pub width: u32,
    pub height: u32,
    pub generation: u64,
    /// Binds a state texture for reading in a fragment shader (group 0).
    pub state_layout: wgpu::BindGroupLayout,
    /// Binds the river field and a linear sampler (group 2).
    pub field_layout: wgpu::BindGroupLayout,
    field: Field,
    field_sampler: wgpu::Sampler,
    buffers: Option<[Buffer; 2]>,
    current: usize,
    step_pipeline: wgpu::RenderPipeline,
    probe_pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    probe_view: wgpu::TextureView,
    probe_tex: wgpu::Texture,
    readbacks: Vec<Readback>,
    spare: Vec<wgpu::Buffer>,
    capture: Option<Capture>,
    rng: Rng,
}

/// Readback rows must be aligned to 256 bytes.
const PROBE_BYTES: u64 = 256;
const PROBE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Sim {
    pub fn new(device: &wgpu::Device) -> Self {
        let state_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("state"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let field_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("river field"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let field_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("river field"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let field = make_field(device, &field_layout, &field_sampler, 1, 1);
        let uniform_layout = uniform_layout(device, "step uniforms");
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("step uniforms"),
            size: size_of::<StepUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("step uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let module = shaders::module(device, "step", shaders::step());
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("step"),
            bind_group_layouts: &[Some(&state_layout), Some(&uniform_layout), Some(&field_layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, format| fullscreen_pipeline(device, &layout, &module, entry, format, None);
        let step_pipeline = pipeline("fs_step", STATE_FORMAT);
        let probe_pipeline = pipeline("fs_probe", PROBE_FORMAT);
        let probe_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("strike probe"),
            size: wgpu::Extent3d { width: MAX_STRIKES as u32, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PROBE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Sim {
            width: 0,
            height: 0,
            generation: 0,
            state_layout,
            field_layout,
            field,
            field_sampler,
            buffers: None,
            current: 0,
            step_pipeline,
            probe_pipeline,
            uniforms,
            uniform_bind,
            probe_view: probe_tex.create_view(&Default::default()),
            probe_tex,
            readbacks: Vec::new(),
            spare: Vec::new(),
            capture: None,
            rng: Rng::from_clock(),
        }
    }

    /// The current state, bound for reading (group 0).
    pub fn state(&self) -> Option<&wgpu::BindGroup> {
        self.buffers.as_ref().map(|b| &b[self.current].bind)
    }

    /// The river field, bound for reading (group 2).
    pub fn field(&self) -> &wgpu::BindGroup {
        &self.field.bind
    }

    /// Replace the river field: `w`×`h` RGBA f16 texels.
    pub fn set_field(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, texels: &[u16]) {
        let size = self.field.tex.size();
        if (size.width, size.height) != (w, h) {
            self.field = make_field(device, &self.field_layout, &self.field_sampler, w, h);
        }
        queue.write_texture(
            self.field.tex.as_image_copy(),
            bytemuck::cast_slice(texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Size the grid. Cells overlapping the old grid are kept unless `fresh`
    /// (the cell size changed); new cells are seeded at `density`, with
    /// water where `water` says.
    #[allow(clippy::too_many_arguments)]
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
        density: f32,
        fresh: bool,
        water: &dyn Fn(u32, u32) -> bool,
    ) {
        let (w, h) = (w.max(1), h.max(1));
        if w == self.width && h == self.height && self.buffers.is_some() && !fresh {
            return;
        }
        let next = [self.make_buffer(device, w, h), self.make_buffer(device, w, h)];
        self.write_seed(queue, &next[0].tex, w, h, density, water);
        match &self.buffers {
            Some(old) if !fresh => {
                let mut enc = device.create_command_encoder(&Default::default());
                enc.copy_texture_to_texture(
                    old[self.current].tex.as_image_copy(),
                    next[0].tex.as_image_copy(),
                    wgpu::Extent3d { width: w.min(self.width), height: h.min(self.height), depth_or_array_layers: 1 },
                );
                queue.submit([enc.finish()]);
            }
            _ => self.generation = 0,
        }
        self.buffers = Some(next);
        self.current = 0;
        self.width = w;
        self.height = h;
    }

    pub fn reseed(&mut self, queue: &wgpu::Queue, density: f32, water: &dyn Fn(u32, u32) -> bool) {
        if let Some(b) = &self.buffers {
            let tex = b[self.current].tex.clone();
            self.write_seed(queue, &tex, self.width, self.height, density, water);
            self.generation = 0;
        }
    }

    /// Advance one step. Strikes that lit a tree come back from `hits()`.
    pub fn step(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, p: &StepParams) {
        let Some(buffers) = &self.buffers else { return };
        let cells = (self.width * self.height) as f64;
        let expected = p.lightning as f64 * cells;
        let listed_rate = expected.min(BOLTS_PER_SECOND / p.steps_per_second as f64).min(MAX_STRIKES as f64 / 4.0);
        let n = (self.rng.poisson(listed_rate) as usize).min(MAX_STRIKES);
        let mut u = StepUniforms {
            seed: self.rng.u32(),
            growth: p.growth,
            lightning: ((expected - listed_rate) / cells) as f32,
            moore: p.moore as u32,
            wrap: p.wrap as u32,
            strike_count: n as u32,
            spotting: p.spotting,
            river_w: p.river_w,
            strikes: [[0; 4]; MAX_STRIKES],
        };
        let mut strikes = Vec::with_capacity(n);
        for s in u.strikes.iter_mut().take(n) {
            let (x, y) = (self.rng.below(self.width) as i32, self.rng.below(self.height) as i32);
            *s = [x, y, 0, 0];
            strikes.push(Strike { x, y });
        }
        // Each step gets its own submit: write_buffer lands at the start of
        // the next submit, so several steps can't share one.
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));

        let src = &buffers[self.current];
        let dst = &buffers[1 - self.current];
        let mut enc = device.create_command_encoder(&Default::default());
        let binds = |state| [state, &self.uniform_bind, &self.field.bind];
        fullscreen_pass(&mut enc, &dst.view, &self.step_pipeline, &binds(&src.bind));
        let readback = if n > 0 {
            fullscreen_pass(&mut enc, &self.probe_view, &self.probe_pipeline, &binds(&dst.bind));
            let buf = self.spare.pop().unwrap_or_else(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("strike readback"),
                    size: PROBE_BYTES,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            enc.copy_texture_to_buffer(
                self.probe_tex.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(PROBE_BYTES as u32), rows_per_image: None },
                },
                wgpu::Extent3d { width: MAX_STRIKES as u32, height: 1, depth_or_array_layers: 1 },
            );
            Some(buf)
        } else {
            None
        };
        queue.submit([enc.finish()]);
        if let Some(buf) = readback {
            let status = Arc::new(AtomicU8::new(0));
            let s = status.clone();
            buf.map_async(wgpu::MapMode::Read, .., move |r| s.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release));
            self.readbacks.push(Readback { buf, strikes, status });
        }
        self.current = 1 - self.current;
        self.generation += 1;
    }

    /// Listed strikes from earlier steps whose readback has arrived and that
    /// set a tree alight.
    pub fn hits(&mut self) -> Vec<Strike> {
        let mut hits = Vec::new();
        let mut i = 0;
        while i < self.readbacks.len() {
            let status = self.readbacks[i].status.load(Ordering::Acquire);
            if status == 0 {
                i += 1;
                continue;
            }
            let r = self.readbacks.swap_remove(i);
            if status == 1 {
                if let Ok(data) = r.buf.slice(..).get_mapped_range() {
                    hits.extend(r.strikes.iter().enumerate().filter(|(j, _)| data[j * 4] > 127).map(|(_, s)| *s));
                }
                r.buf.unmap();
                self.spare.push(r.buf);
            }
        }
        hits
    }

    /// Set a cell alight as if by lightning. Returns the cell if it's on the grid.
    pub fn ignite(&self, queue: &wgpu::Queue, x: i32, y: i32) -> Option<Strike> {
        let b = self.buffers.as_ref()?;
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &b[self.current].tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x as u32, y: y as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &[state::FIRE, state::TREE, cause::LIGHTNING, 0],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: None },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        Some(Strike { x, y })
    }

    /// Start copying the grid back from the GPU; `captured()` has it once the
    /// copy is mapped. Does nothing while a capture is already under way.
    pub fn capture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Some(b) = &self.buffers else { return };
        if self.capture.is_some() {
            return;
        }
        let (w, h) = (self.width, self.height);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid capture"),
            size: (row_bytes(w) * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            b[self.current].tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_bytes(w)), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let status = Arc::new(AtomicU8::new(0));
        let s = status.clone();
        buf.map_async(wgpu::MapMode::Read, .., move |r| s.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release));
        self.capture = Some(Capture { buf, width: w, height: h, generation: self.generation, status });
    }

    /// The grid from `capture()`, once it has arrived.
    pub fn captured(&mut self) -> Option<Snapshot> {
        let status = self.capture.as_ref()?.status.load(Ordering::Acquire);
        if status == 0 {
            return None;
        }
        let c = self.capture.take()?;
        if status != 1 {
            return None;
        }
        let row = (c.width * 4) as usize;
        let cells = {
            let data = c.buf.slice(..).get_mapped_range().ok()?;
            data.chunks_exact(row_bytes(c.width) as usize).flat_map(|r| &r[..row]).copied().collect()
        };
        c.buf.unmap();
        Some(Snapshot { width: c.width, height: c.height, generation: c.generation, cells })
    }

    /// Put a saved grid back. Where it's a different size, the overlap is
    /// restored and the rest of the grid is left as it is.
    pub fn restore(&mut self, queue: &wgpu::Queue, s: &Snapshot) {
        let Some(b) = &self.buffers else { return };
        let (w, h) = (s.width.min(self.width), s.height.min(self.height));
        if w == 0 || h == 0 || s.cells.len() != (s.width * s.height * 4) as usize {
            return;
        }
        queue.write_texture(
            b[self.current].tex.as_image_copy(),
            &s.cells,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(s.width * 4), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.generation = s.generation;
    }

    fn make_buffer(&self, device: &wgpu::Device, w: u32, h: u32) -> Buffer {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("state"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: STATE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("state"),
            layout: &self.state_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
        });
        Buffer { tex, view, bind }
    }

    fn write_seed(&mut self, queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, density: f32, water: &dyn Fn(u32, u32) -> bool) {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for (i, px) in data.chunks_exact_mut(4).enumerate() {
            if water(i as u32 % w, i as u32 / w) {
                px.copy_from_slice(&[state::WATER, state::WATER, cause::NONE, SETTLED]);
            } else if self.rng.f32() < density {
                px[0] = state::TREE;
                px[1] = state::TREE;
            } else {
                px[3] = SETTLED;
            }
        }
        queue.write_texture(
            tex.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }
}

fn make_field(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, w: u32, h: u32) -> Field {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("river field"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FIELD_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("river field"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    });
    Field { tex, bind }
}

/// Bytes per row of a texture copied into a buffer, which must be a multiple
/// of 256.
fn row_bytes(w: u32) -> u32 {
    (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

pub fn uniform_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        }],
    })
}

/// A pipeline that draws one fullscreen triangle (vertex entry point `vs`).
pub fn fullscreen_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    fragment: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Draw a fullscreen triangle into `target`, overwriting it.
pub fn fullscreen_pass(
    enc: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    binds: &[&wgpu::BindGroup],
) {
    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    for (i, b) in binds.iter().enumerate() {
        pass.set_bind_group(i as u32, *b, &[]);
    }
    pass.draw(0..3, 0..1);
}
