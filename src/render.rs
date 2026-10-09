//! Drawing one view: a window onto the layout (a monitor, or the browser
//! canvas). The scene is drawn into an offscreen target at the adaptive
//! resolution, bolts are added on, and the result is scaled up to the screen,
//! where the panel is drawn at full resolution.

use std::collections::HashMap;

use crate::lightning::{BOLT_FORMAT, BoltUniforms, Lightning};
use crate::params::Renderer;
use crate::shaders;
use crate::sim::{Sim, fullscreen_pass, fullscreen_pipeline, uniform_layout};

pub const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// See `View` in `shaders/view.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewUniforms {
    pub origin: [f32; 2],
    pub scale: [f32; 2],
    pub target_h: f32,
    pub cell_px: f32,
    pub regrow: f32,
    pub progress: f32,
    pub time: f32,
    pub grid_lines: u32,
    pub wrap: u32,
    pub trees: i32,
    pub night: u32,
    pub snag: f32,
    /// River half-width in cells.
    pub river_hw: f32,
    pub _pad: u32,
}

/// A rectangle of the layout in global physical pixels, y down (as monitors
/// are positioned), with the origin at the layout's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// Parse `WxH+X+Y` (X and Y may be negative: `WxH-1920+0`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn parse(s: &str) -> Option<Rect> {
        let (size, pos) = s.split_once(['+', '-']).map(|(a, _)| (a, &s[a.len()..]))?;
        let (w, h) = size.split_once('x')?;
        let mut nums = Vec::new();
        let mut cur = String::new();
        for ch in pos.chars() {
            if (ch == '+' || ch == '-') && !cur.is_empty() && cur != "-" && cur != "+" {
                nums.push(cur.clone());
                cur.clear();
            }
            cur.push(ch);
        }
        nums.push(cur);
        let n: Vec<i32> = nums.iter().map(|s| s.trim_start_matches('+').parse().ok()).collect::<Option<_>>()?;
        let [x, y] = n[..] else { return None };
        Some(Rect { x, y, w: w.parse().ok()?, h: h.parse().ok()? })
    }
}

pub struct Pipelines {
    scene: wgpu::RenderPipeline,
    flat: wgpu::RenderPipeline,
    view_layout: wgpu::BindGroupLayout,
    blit_module: wgpu::ShaderModule,
    blit_layout: wgpu::BindGroupLayout,
    blit_pipeline_layout: wgpu::PipelineLayout,
    blits: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    pub lightning: Lightning,
}

impl Pipelines {
    pub fn new(device: &wgpu::Device, sim: &Sim) -> Self {
        let view_layout = uniform_layout(device, "view uniforms");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("view"),
            bind_group_layouts: &[Some(&sim.state_layout), Some(&view_layout), Some(&sim.field_layout)],
            immediate_size: 0,
        });
        let module = |label, src| shaders::module(device, label, src);
        let scene = fullscreen_pipeline(device, &layout, &module("scene", shaders::scene()), "fs", SCENE_FORMAT, None);
        let flat = fullscreen_pipeline(device, &layout, &module("flat", shaders::flat()), "fs", SCENE_FORMAT, None);
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
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
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Pipelines {
            scene,
            flat,
            view_layout,
            blit_module: module("blit", shaders::blit()),
            blit_layout,
            blit_pipeline_layout,
            blits: HashMap::new(),
            sampler,
            lightning: Lightning::new(device, SCENE_FORMAT),
        }
    }

    fn blit(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        self.blits
            .entry(format)
            .or_insert_with(|| fullscreen_pipeline(device, &self.blit_pipeline_layout, &self.blit_module, "fs", format, None))
    }
}

/// A view's offscreen targets and uniforms.
pub struct ViewTargets {
    /// Size of the scene target, in its own pixels.
    pub size: (u32, u32),
    scene_view: wgpu::TextureView,
    bolts_view: wgpu::TextureView,
    bolts_bind: wgpu::BindGroup,
    blit_bind: wgpu::BindGroup,
    view_buf: wgpu::Buffer,
    view_bind: wgpu::BindGroup,
    bolt_buf: wgpu::Buffer,
    bolt_bind: wgpu::BindGroup,
}

impl ViewTargets {
    pub fn new(device: &wgpu::Device, p: &Pipelines, w: u32, h: u32) -> Self {
        let (w, h) = (w.max(1), h.max(1));
        let target = |label, format| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let scene_view = target("scene", SCENE_FORMAT);
        let bolts_view = target("bolts", BOLT_FORMAT);
        let uniform = |label, size: usize, layout| {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            (buf, bind)
        };
        let (view_buf, view_bind) = uniform("view uniforms", size_of::<ViewUniforms>(), &p.view_layout);
        let (bolt_buf, bolt_bind) = uniform("bolt uniforms", size_of::<BoltUniforms>(), &p.lightning.uniform_layout);
        let bolts_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bolts"),
            layout: &p.lightning.texture_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&bolts_view) }],
        });
        let blit_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &p.blit_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scene_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&p.sampler) },
            ],
        });
        ViewTargets { size: (w, h), scene_view, bolts_view, bolts_bind, blit_bind, view_buf, view_bind, bolt_buf, bolt_bind }
    }

    /// Draw the view into `out` (a surface texture or a screenshot target).
    /// `bolts` says whether `Lightning::prepare` found anything this frame.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        p: &mut Pipelines,
        sim: &Sim,
        renderer: Renderer,
        u: &ViewUniforms,
        bolt_u: &BoltUniforms,
        bolts: bool,
        out: &wgpu::TextureView,
        out_format: wgpu::TextureFormat,
    ) {
        let Some(state) = sim.state() else { return };
        queue.write_buffer(&self.view_buf, 0, bytemuck::bytes_of(u));
        let pipeline = match renderer {
            Renderer::Scene => &p.scene,
            Renderer::Flat => &p.flat,
        };
        fullscreen_pass(enc, &self.scene_view, pipeline, &[state, &self.view_bind, sim.field()]);
        if bolts {
            queue.write_buffer(&self.bolt_buf, 0, bytemuck::bytes_of(bolt_u));
            p.lightning.draw(enc, &self.bolts_view, &self.bolts_bind, &self.bolt_bind, &self.scene_view);
        }
        let blit = p.blit(device, out_format);
        fullscreen_pass(enc, out, blit, &[&self.blit_bind]);
    }
}
