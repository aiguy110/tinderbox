//! Everything about a running tinderbox that doesn't depend on windows: the
//! layout, the sim and its timing, bolts, and the adaptive resolution.

use web_time::Instant;

use crate::critical;
use crate::lightning::BoltUniforms;
use crate::params::{Params, Renderer};
use crate::render::{Pipelines, Rect, ViewTargets, ViewUniforms};
use crate::save;
use crate::shaders::hashf;
use crate::sim::{Sim, StepParams, Strike};

const MAX_STEPS_PER_FRAME: u32 = 16;
/// While paused, a manual step's transitions play at most this fast.
const PAUSED_STEPS_PER_SECOND: f32 = 2.0;
/// Phones report 3, which is 9× the pixels of 1× for little visible gain.
pub const MAX_PIXEL_RATIO: f64 = 2.0;
const MIN_PIXEL_RATIO: f64 = 0.5;
const SLOW_FPS: f64 = 40.0;
const FAST_FPS: f64 = 57.0;
/// How often the app saves the grid (see save.rs).
const SAVE_SECS: f64 = 5.0;

/// Where the views sit. The grid covers the bounding box of all of them;
/// cells in gaps between monitors are simulated but not seen.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub views: Vec<Rect>,
    /// Physical pixels per logical pixel. Cell size is in logical pixels.
    pub scale: f64,
}

impl Layout {
    pub fn size(&self) -> (u32, u32) {
        let w = self.views.iter().map(|r| r.x + r.w as i32).max().unwrap_or(1);
        let h = self.views.iter().map(|r| r.y + r.h as i32).max().unwrap_or(1);
        (w.max(1) as u32, h.max(1) as u32)
    }

    /// Shift views so the bounding box starts at (0, 0).
    pub fn normalized(mut self) -> Self {
        let x0 = self.views.iter().map(|r| r.x).min().unwrap_or(0);
        let y0 = self.views.iter().map(|r| r.y).min().unwrap_or(0);
        for r in &mut self.views {
            r.x -= x0;
            r.y -= y0;
        }
        self
    }
}

pub struct Engine {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub sim: Sim,
    pub pipelines: Pipelines,
    pub params: Params,
    saved: Params,
    pub layout: Layout,
    cell_size: f32,
    max_texture: u32,
    start: Instant,
    last: Instant,
    /// Fractional steps owed. After stepping, it's also how far we are
    /// through the current step, which is what transitions are timed by.
    pub debt: f32,
    pub fps: f64,
    /// Rendered pixels per logical pixel. Starts at the device's ratio
    /// (capped) and is lowered while frames are slow; the grid doesn't change.
    pub pixel_ratio: f64,
    adapt: Adapt,
    bolts: bool,
    saved_at: Instant,
}

/// Lower the render resolution while frames are slow, and creep back up after
/// a spell of full frame rate, but never back to a ratio that was too slow.
#[derive(Default)]
struct Adapt {
    ceiling: f64,
    window_at: Option<Instant>,
    frames: u32,
    fast_windows: u32,
    /// The last drop, so we can undo it if it didn't help (a frame rate
    /// capped by the browser, like iOS Low Power Mode's 30 fps, isn't the
    /// GPU's fault).
    dropped: Option<(f64, f64)>,
    gave_up: bool,
}

impl Engine {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, layout: Layout, params: Params) -> Self {
        let sim = Sim::new(&device);
        let pipelines = Pipelines::new(&device, &sim);
        let max_texture = device.limits().max_texture_dimension_2d;
        let now = Instant::now();
        let mut e = Engine {
            device,
            queue,
            sim,
            pipelines,
            saved: params.clone(),
            params,
            layout,
            cell_size: 0.0,
            max_texture,
            start: now,
            last: now,
            debt: 0.0,
            fps: 0.0,
            pixel_ratio: 1.0,
            adapt: Adapt::default(),
            bolts: false,
            saved_at: now,
        };
        e.reset_resolution();
        e.fit_grid();
        e
    }

    /// Physical pixels per cell.
    pub fn cell_phys(&self) -> f64 {
        self.params.cell_size as f64 * self.layout.scale
    }

    /// Rendered pixels per physical pixel.
    pub fn render_ratio(&self) -> f64 {
        self.pixel_ratio / self.layout.scale
    }

    pub fn set_layout(&mut self, layout: Layout) {
        if layout != self.layout {
            let scale_changed = layout.scale != self.layout.scale;
            self.layout = layout;
            if scale_changed {
                self.reset_resolution();
            }
            self.fit_grid();
        }
    }

    /// Fit the grid to the layout. Cells overlapping the old grid are kept
    /// unless the cell size changed.
    fn fit_grid(&mut self) {
        let (w, h) = self.layout.size();
        let c = self.cell_phys();
        let gw = ((w as f64 / c).ceil() as u32).clamp(1, self.max_texture);
        let gh = ((h as f64 / c).ceil() as u32).clamp(1, self.max_texture);
        let fresh = self.params.cell_size != self.cell_size;
        self.cell_size = self.params.cell_size;
        self.sim.resize(&self.device, &self.queue, gw, gh, self.params.initial_density, fresh);
        self.lock_critical();
    }

    /// With the lock on, growth is capped and lightning follows from it.
    fn lock_critical(&mut self) {
        let p = &mut self.params;
        if p.critical {
            (p.growth, p.lightning) = critical::lock(p.growth, self.sim.width * self.sim.height, p.moore);
        }
    }

    /// Call after the panel (or a key) may have changed params.
    pub fn params_changed(&mut self) {
        self.lock_critical();
        if self.params == self.saved {
            return;
        }
        if self.params.cell_size != self.saved.cell_size {
            self.fit_grid();
        }
        if self.params.renderer != self.saved.renderer {
            self.reset_resolution();
        }
        // Pausing isn't saved, so it shouldn't cause a write.
        let mut cmp = self.params.clone();
        cmp.paused = self.saved.paused;
        if cmp != self.saved {
            self.params.save();
        }
        self.saved = self.params.clone();
    }

    /// Pick up the grid an earlier run saved, if it used the same cell size.
    /// Call once the layout has settled, so none of it is cropped away.
    pub fn resume(&mut self) {
        if let Some((s, cell_size)) = save::load()
            && cell_size == self.cell_size
        {
            self.sim.restore(&self.queue, &s);
        }
    }

    /// Save the grid every few seconds. Call once a frame; the copy back from
    /// the GPU arrives a frame or so later.
    pub fn autosave(&mut self) {
        if let Some(s) = self.sim.captured() {
            save::store(&s, self.cell_size);
        }
        if self.saved_at.elapsed().as_secs_f64() >= SAVE_SECS {
            self.saved_at = Instant::now();
            self.sim.capture(&self.device, &self.queue);
        }
    }

    /// Save the grid now, waiting for the GPU.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_now(&mut self) {
        self.sim.capture(&self.device, &self.queue);
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        if let Some(s) = self.sim.captured() {
            save::store(&s, self.cell_size);
        }
    }

    pub fn reseed(&mut self) {
        self.sim.reseed(&self.queue, self.params.initial_density);
    }

    fn step_params(&self) -> StepParams {
        StepParams {
            growth: self.params.growth,
            lightning: self.params.lightning,
            moore: self.params.moore,
            wrap: self.params.wrap,
            steps_per_second: self.params.steps_per_second,
        }
    }

    pub fn step_once(&mut self) {
        let p = self.step_params();
        self.sim.step(&self.device, &self.queue, &p);
        self.debt = 0.0;
    }

    pub fn seconds(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Advance the clock and the sim for a new frame, and get bolts ready.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.25);
        self.last = now;
        self.fps = self.fps * 0.95 + if dt > 0.0 { 1.0 / dt as f64 } else { 0.0 } * 0.05;

        if !self.params.paused {
            self.debt += dt * self.params.steps_per_second;
            let n = (self.debt.floor() as u32).min(MAX_STEPS_PER_FRAME);
            let p = self.step_params();
            for _ in 0..n {
                self.sim.step(&self.device, &self.queue, &p);
            }
            self.debt = if n == MAX_STEPS_PER_FRAME { 0.0 } else { self.debt - n as f32 };
        } else {
            self.debt = (self.debt + dt * self.params.steps_per_second.min(PAUSED_STEPS_PER_SECOND)).min(1.0);
        }
        for hit in self.sim.hits() {
            self.strike(hit);
        }
        self.adapt_resolution(now);
        let t = self.seconds();
        self.bolts = self.pipelines.lightning.prepare(&self.device, &self.queue, t);
    }

    fn strike(&mut self, s: Strike) {
        // Where in the cell the fire starts; matches arrival() in the shaders.
        let c = self.cell_phys() as f32;
        let x = (s.x as f32 + 0.3 + 0.4 * hashf(s.x, s.y, 11)) * c;
        let y = (s.y as f32 + 0.3 + 0.4 * hashf(s.x, s.y, 12)) * c;
        let top = self.layout.size().1 as f32;
        let t = self.seconds();
        self.pipelines.lightning.strike(x, y, top, self.layout.scale as f32, t);
    }

    /// Strike lightning at a point in global physical pixels (y down, as
    /// monitors and cursors are positioned). Returns whether it hit the grid.
    pub fn ignite(&mut self, gx: f64, gy: f64, bolt: bool) -> bool {
        let c = self.cell_phys();
        let h = self.layout.size().1 as f64;
        let x = (gx / c).floor() as i32;
        let y = ((h - gy) / c).floor() as i32;
        match self.sim.ignite(&self.queue, x, y) {
            Some(s) => {
                if bolt {
                    self.strike(s);
                }
                true
            }
            None => false,
        }
    }

    /// Size of a view's scene target at the current render resolution.
    pub fn target_size(&self, r: &Rect) -> (u32, u32) {
        let k = self.render_ratio();
        let side = |v: u32| ((v as f64 * k).round() as u32).clamp(1, self.max_texture);
        (side(r.w), side(r.h))
    }

    /// Draw a view whose targets are already `target_size`.
    pub fn draw_view(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        r: &Rect,
        targets: &ViewTargets,
        out: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let (_, total_h) = self.layout.size();
        // Bottom-left corner, y up.
        let origin = [r.x as f32, (total_h as i32 - r.y - r.h as i32) as f32];
        let (tw, th) = targets.size;
        let cell = self.cell_phys() as f32;
        let cell_rendered = cell * (tw as f32 / r.w as f32);
        let p = &self.params;
        let u = ViewUniforms {
            origin,
            scale: [r.w as f32 / tw as f32, r.h as f32 / th as f32],
            target_h: th as f32,
            cell_px: cell,
            regrow: p.regrow.max(1.0),
            snag: p.snag.max(0.0),
            progress: self.debt.clamp(0.0, 1.0),
            // Wrapped so flicker keeps f32 precision in a long-running show.
            time: (self.seconds() % 10_000.0) as f32,
            grid_lines: (p.grid_lines && cell_rendered >= 6.0) as u32,
            wrap: p.wrap as u32,
            // Keep trees at least 4 px apart.
            trees: (p.trees.round() as i32).min((cell_rendered / 4.0).floor() as i32).max(1),
            night: p.night as u32,
            _pad: [0; 2],
        };
        let bu = BoltUniforms { origin, size: [r.w as f32, r.h as f32], sky: self.pipelines.lightning.sky, _pad: [0.0; 3] };
        let renderer = if p.renderer == Renderer::Flat { Renderer::Flat } else { Renderer::Scene };
        targets.draw(&self.device, &self.queue, enc, &mut self.pipelines, &self.sim, renderer, &u, &bu, self.bolts, out, format);
    }

    pub fn reset_resolution(&mut self) {
        self.adapt.ceiling = self.layout.scale.min(MAX_PIXEL_RATIO);
        self.pixel_ratio = self.adapt.ceiling;
        self.adapt.fast_windows = 0;
        self.adapt.dropped = None;
        self.adapt.gave_up = false;
    }

    /// A background tab or hidden window stops frames; don't count that as slow.
    pub fn pause_adapting(&mut self) {
        self.adapt.window_at = None;
    }

    fn adapt_resolution(&mut self, now: Instant) {
        let a = &mut self.adapt;
        let Some(at) = a.window_at else {
            a.window_at = Some(now);
            a.frames = 0;
            return;
        };
        a.frames += 1;
        let span = (now - at).as_secs_f64();
        if span < 1.0 {
            return;
        }
        let rate = a.frames as f64 / span;
        a.window_at = Some(now);
        a.frames = 0;
        if let Some((ratio, before)) = a.dropped.take()
            && rate < before * 1.15
        {
            self.pixel_ratio = ratio;
            a.gave_up = true;
            return;
        }
        if rate < SLOW_FPS && self.pixel_ratio > MIN_PIXEL_RATIO && !a.gave_up {
            a.dropped = Some((self.pixel_ratio, rate));
            a.ceiling = self.pixel_ratio * 0.95;
            // Pixel cost goes with the square of the ratio.
            self.pixel_ratio = MIN_PIXEL_RATIO.max(self.pixel_ratio * (rate / FAST_FPS).sqrt());
            a.fast_windows = 0;
        } else if rate >= FAST_FPS && self.pixel_ratio < a.ceiling {
            a.fast_windows += 1;
            if a.fast_windows >= 4 {
                self.pixel_ratio = a.ceiling.min(self.pixel_ratio * 1.15);
                a.fast_windows = 0;
            }
        }
    }

    pub fn stats(&self, views: usize, backend: &str) -> String {
        let screens = if views > 1 { format!(" · {views} screens") } else { String::new() };
        format!(
            "{}×{} cells · gen {} · {:.0} fps · {:.2}× res{screens} · {backend}",
            self.sim.width, self.sim.height, self.sim.generation, self.fps, self.pixel_ratio
        )
    }
}
