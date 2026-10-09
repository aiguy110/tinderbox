//! Lightning bolts: a procedural, animated strike from the top of the layout
//! down to a cell that lightning set alight.
//!
//! Each bolt is a jagged polyline (midpoint displacement) with a few forks,
//! in global physical pixels (y up), so a bolt can cross monitors. Every view
//! draws the same segments through its own window onto the layout; see
//! `shaders/lightning.wgsl`.
//!
//! Timeline: a dim leader feels its way down, the return stroke flashes the
//! whole channel, then it decays with a restrike or two.

use std::f32::consts::PI;

use crate::rng::Rng;
use crate::shaders;
use crate::sim::uniform_layout;

/// Bolts on screen at once; more strikes than this are dropped.
const MAX_BOLTS: usize = 12;
/// Seconds for the leader to reach the ground.
const LEADER: f32 = 0.07;
const LIFE: f32 = 0.75;

pub const BOLT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Seg {
    a: [f32; 2],
    b: [f32; 2],
    /// Core width in pixels.
    w: f32,
    /// When the leader reaches the segment's far end, 0 (sky) to 1 (ground).
    reach: f32,
    fork: bool,
}

struct Bolt {
    born: f64,
    segs: Vec<Seg>,
    restrikes: Vec<f32>,
}

type Point = [f32; 2];

/// Jag a straight line by midpoint displacement until segments are short.
fn jag(rng: &mut Rng, a: Point, b: Point, rough: f32, min_len: f32) -> Vec<Point> {
    let min_len = min_len.max(1.0);
    let mut pts = vec![a, b];
    loop {
        let mut next = vec![pts[0]];
        let mut split = false;
        for w in pts.windows(2) {
            let ([ax, ay], [bx, by]) = (w[0], w[1]);
            let (dx, dy) = (bx - ax, by - ay);
            if dx.hypot(dy) > min_len {
                let t = 0.35 + 0.3 * rng.f32();
                let off = (rng.f32() - 0.5) * rough;
                next.push([ax + dx * t - dy * off, ay + dy * t + dx * off]);
                split = true;
            }
            next.push(w[1]);
        }
        pts = next;
        if !split {
            return pts;
        }
    }
}

fn add_path(segs: &mut Vec<Seg>, pts: &[Point], w: f32, reach0: f32, reach1: f32, fork: bool) {
    let n = (pts.len() - 1) as f32;
    for (i, p) in pts.windows(2).enumerate() {
        segs.push(Seg { a: p[0], b: p[1], w, reach: reach0 + (reach1 - reach0) * (i + 1) as f32 / n, fork });
    }
}

fn build_bolt(rng: &mut Rng, x: f32, y: f32, top: f32, scale: f32) -> Vec<Seg> {
    let mut segs = Vec::new();
    // The top row of cells can stick out above the layout, so a strike there
    // is above the top; a negative height would never stop jagging.
    let h = (top - y).max(8.0 * scale);
    let start = [x + (rng.f32() - 0.5) * 0.5 * h, top + 8.0 * scale];
    // A few large kinks first so the channel wanders, then fine jaggedness.
    let coarse = jag(rng, start, [x, y], 0.5, h / 5.0);
    let mut main = vec![coarse[0]];
    for c in coarse.windows(2) {
        main.extend_from_slice(&jag(rng, c[0], c[1], 0.45, 5.0 * scale)[1..]);
    }
    let w = 1.4 * scale;
    add_path(&mut segs, &main, w, 0.0, 1.0, false);

    // Forks peel off the upper part of the channel, heading down and away.
    let forks = 2 + rng.below(4);
    for _ in 0..forks {
        let i = (rng.f32() * main.len() as f32 * 0.75) as usize;
        let [px, py] = main[i];
        let at = i as f32 / (main.len() - 1) as f32;
        let len = (0.12 + 0.25 * rng.f32()) * h * (1.0 - at);
        let ang = -PI / 2.0 + if rng.f32() < 0.5 { -1.0 } else { 1.0 } * (0.3 + 0.6 * rng.f32());
        let end = [px + ang.cos() * len, py + ang.sin() * len];
        let pts = jag(rng, [px, py], end, 0.6, 5.0 * scale);
        add_path(&mut segs, &pts, w * 0.55, at, (at + len / h).min(1.0), true);
    }
    segs
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BoltUniforms {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub sky: f32,
    pub _pad: [f32; 3],
}

pub struct Lightning {
    bolts: Vec<Bolt>,
    rng: Rng,
    seg_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    pub uniform_layout: wgpu::BindGroupLayout,
    pub texture_layout: wgpu::BindGroupLayout,
    vbuf: Option<wgpu::Buffer>,
    vertices: u32,
    /// The sky flash this frame.
    pub sky: f32,
}

impl Lightning {
    pub fn new(device: &wgpu::Device, scene_format: wgpu::TextureFormat) -> Self {
        let module = shaders::module(device, "lightning", shaders::lightning());
        let uniform_layout = uniform_layout(device, "bolt uniforms");
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bolts"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline =
            |label, layouts: &[Option<&wgpu::BindGroupLayout>], vs, fs, buffers: &[Option<wgpu::VertexBufferLayout>], format, blend| {
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts: layouts,
                    immediate_size: 0,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: Default::default(), buffers },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let seg_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: 8 * 4,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x2],
        })];
        let seg_pipeline = pipeline(
            "bolt segments",
            &[Some(&uniform_layout)],
            "vs_seg",
            "fs_seg",
            &seg_buffers,
            BOLT_FORMAT,
            wgpu::BlendState { color: max, alpha: max },
        );
        let composite_pipeline = pipeline(
            "bolt composite",
            &[Some(&uniform_layout), Some(&texture_layout)],
            "vs_full",
            "fs_composite",
            &[],
            scene_format,
            wgpu::BlendState { color: add, alpha: add },
        );
        Lightning {
            bolts: Vec::new(),
            rng: Rng::from_clock(),
            seg_pipeline,
            composite_pipeline,
            uniform_layout,
            texture_layout,
            vbuf: None,
            vertices: 0,
            sky: 0.0,
        }
    }

    /// Strike at (x, y) in global physical pixels (y up), from a sky at `top`.
    /// `scale` is physical pixels per logical pixel; `now` is in seconds.
    pub fn strike(&mut self, x: f32, y: f32, top: f32, scale: f32, now: f64) {
        if self.bolts.len() >= MAX_BOLTS {
            return;
        }
        let mut restrikes = vec![LEADER + 0.1 + 0.1 * self.rng.f32()];
        if self.rng.f32() < 0.6 {
            restrikes.push(restrikes[0] + 0.08 + 0.12 * self.rng.f32());
        }
        let segs = build_bolt(&mut self.rng, x, y, top, scale);
        self.bolts.push(Bolt { born: now, segs, restrikes });
    }

    /// Build this frame's segment quads. Returns whether there's anything to draw.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, now: f64) -> bool {
        self.bolts.retain(|b| ((now - b.born) as f32) < LIFE);
        self.vertices = 0;
        self.sky = 0.0;
        if self.bolts.is_empty() {
            return false;
        }
        let mut verts: Vec<f32> = Vec::new();
        for b in &self.bolts {
            let age = (now - b.born) as f32;
            let mut stroke = 0.0f32;
            let mut reach = 1.0;
            if age < LEADER {
                reach = age / LEADER;
            } else {
                stroke = (-(age - LEADER) / 0.06).exp();
                for &r in &b.restrikes {
                    if age >= r {
                        stroke = stroke.max(0.85 * (-(age - r) / 0.05).exp());
                    }
                }
            }
            let fade = 1.0 - ((age - LIFE + 0.2) / 0.2).max(0.0);
            let main = if age < LEADER { 0.4 } else { (stroke * 1.5).max(0.2 * (-(age - LEADER) / 0.2).exp()) * fade };
            let fork = if age < LEADER { 0.3 } else { stroke * 0.7 * fade };
            self.sky += 0.2 * stroke;
            for s in &b.segs {
                if s.reach > reach + 0.02 {
                    continue;
                }
                let k = if s.fork { fork } else { main };
                if k < 0.01 {
                    continue;
                }
                // A quad around the segment, wide enough for the glow.
                let r = s.w * 14.0;
                let (dx, dy) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
                let len = dx.hypot(dy).max(1e-6);
                let (ux, uy) = (dx / len * r, dy / len * r);
                let corners = [
                    [s.a[0] - ux - uy, s.a[1] - uy + ux],
                    [s.a[0] - ux + uy, s.a[1] - uy - ux],
                    [s.b[0] + ux + uy, s.b[1] + uy - ux],
                    [s.b[0] + ux - uy, s.b[1] + uy + ux],
                ];
                for i in [0, 1, 2, 0, 2, 3] {
                    verts.extend_from_slice(&[corners[i][0], corners[i][1], s.a[0], s.a[1], s.b[0], s.b[1], s.w, k]);
                }
            }
        }
        self.sky = self.sky.min(0.35);
        if verts.is_empty() {
            return self.sky > 0.0;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&verts);
        if self.vbuf.as_ref().is_none_or(|b| b.size() < bytes.len() as u64) {
            self.vbuf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bolt segments"),
                size: (bytes.len() as u64).next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        queue.write_buffer(self.vbuf.as_ref().unwrap(), 0, bytes);
        self.vertices = (verts.len() / 8) as u32;
        true
    }

    /// Draw this frame's bolts into `bolts` (cleared), then add them onto `scene`.
    pub fn draw(
        &self,
        enc: &mut wgpu::CommandEncoder,
        bolts: &wgpu::TextureView,
        bolts_bind: &wgpu::BindGroup,
        uniforms: &wgpu::BindGroup,
        scene: &wgpu::TextureView,
    ) {
        {
            let mut pass = begin(enc, bolts, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            if let (Some(vbuf), true) = (&self.vbuf, self.vertices > 0) {
                pass.set_pipeline(&self.seg_pipeline);
                pass.set_bind_group(0, uniforms, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.draw(0..self.vertices, 0..1);
            }
        }
        let mut pass = begin(enc, scene, wgpu::LoadOp::Load);
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, uniforms, &[]);
        pass.set_bind_group(1, bolts_bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn begin<'e>(enc: &'e mut wgpu::CommandEncoder, target: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    })
}
