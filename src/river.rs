//! A meandering river across the grid, on the CPU.
//!
//! The channel is a centerline of nodes, flowing left to right, moved with
//! the Howard–Knutson migration rate (as in meanderpy): each node moves
//! sideways in proportion to the channel curvature at the node (pulling the
//! bend in) plus a weighted average of the curvature just upstream (pushing
//! it out, and downstream). Bends grow and drift. When two stretches of the
//! channel come within about a width of each other, the loop between them is
//! cut off and left behind as an oxbow lake, which silts up and greens over.
//!
//! The river reaches the GPU as a field texture (`raster`), a little finer
//! than the cell grid, read by both the step rule and the renderers:
//!   r = signed distance to the nearest water's edge, in cells (< 0 in water)
//!   g, b = the nearest channel's direction of flow, scaled by its flow
//!          (1 for the river; small and fading for a lake as it silts up)
//!   a = fresh sand: 1 where water has just left (point bars, dry lake
//!       beds), fading as it greens over
//! A cell is water when the texel at its center is. Moving water in and out
//! of cells is the step shader's job, so the river never writes the grid.

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

/// Ω and Γ in the migration rate: the local curvature's weight (negative:
/// it straightens) and the upstream curvature's (it bends further).
const OMEGA: f32 = -1.0;
const GAMMA: f32 = 2.5;
/// How far upstream the curvature still pushes on the bank, in widths.
const MEMORY: f32 = 1.5;
/// Cells per step that a node moves at meander rate 1, per unit of
/// curvature × width.
const KL: f32 = 0.008;
/// Node spacing, in widths.
const SPACING: f32 = 0.3;
/// Stretches of channel whose centerlines come this close (in widths) cut
/// the loop between them off...
const CUTOFF: f32 = 1.1;
/// ...if the loop is at least this long, in widths.
const LOOP: f32 = 3.5;
/// River clock steps (steps × meander rate) for an oxbow lake to silt up,
/// and for fresh sand to green over.
const LAKE_LIFE: f64 = 15000.0;
const DEPOSIT_LIFE: f64 = 900.0;
/// The field texture is capped at this many texels; a full one is about 12 MB.
const MAX_TEXELS: f32 = 1.5e6;
/// Strength of a weak, fixed sideways push (as a fraction of KL), standing
/// in for the unevenness of real ground: it starts straight stretches
/// meandering again after a big cutoff.
const FORCING: f32 = 0.15;
/// Trees grow this much faster on the bank (see `riparian`).
pub const RIPARIAN: f32 = 1.0;

#[derive(Clone, Serialize, Deserialize)]
struct Lake {
    pts: Vec<[f32; 2]>,
    /// Half-width when it was cut off.
    hw: f32,
    born: f64,
}

/// What's saved between runs.
#[derive(Serialize, Deserialize)]
struct Saved {
    gw: u32,
    gh: u32,
    width: f32,
    nodes: Vec<[f32; 2]>,
    lakes: Vec<Lake>,
    clock: f64,
}

pub struct River {
    gw: u32,
    gh: u32,
    /// Channel width in cells.
    width: f32,
    nodes: Vec<[f32; 2]>,
    lakes: Vec<Lake>,
    /// Steps × meander rate.
    clock: f64,
    /// Field size in texels, and texels per cell on each axis.
    pub fw: u32,
    pub fh: u32,
    sx: f32,
    sy: f32,
    dist: Vec<f32>,
    flow: Vec<[f32; 2]>,
    wet: Vec<bool>,
    /// Clock when each texel last came out of the water.
    dried: Vec<f64>,
    /// How far the channel has moved since the last raster, in cells.
    moved: f32,
    /// A cutoff since the last raster.
    cut: bool,
    /// The forcing: (amplitude, wavenumber, phase) of waves along x.
    forcing: Vec<(f32, f32, f32)>,
}

/// Field texels per cell: fine enough for smooth banks on big cells, and
/// capped in total.
fn default_scale(gw: u32, gh: u32) -> f32 {
    (640.0 / gw.max(gh) as f32).clamp(1.0, 16.0).min((MAX_TEXELS / (gw * gh) as f32).sqrt())
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The growth multiplier at signed distance `d` from water (in cells) for a
/// channel `width` wide. Also in step.wgsl.
#[cfg(not(target_arch = "wasm32"))]
pub fn riparian(d: f32, width: f32) -> f32 {
    1.0 + RIPARIAN * (1.0 - smoothstep(0.0, width.max(1.0), d))
}

impl River {
    /// A river across a `gw`×`gh` grid, already meandering.
    pub fn new(gw: u32, gh: u32, width: f32, seed: u64) -> Self {
        Self::with_scale(gw, gh, width, seed, default_scale(gw, gh))
    }

    /// With `scale` field texels per cell.
    pub fn with_scale(gw: u32, gh: u32, width: f32, seed: u64, scale: f32) -> Self {
        let mut rng = Rng::new(seed);
        let mut r = Self::blank(gw, gh, width, scale);
        r.force(&mut rng);
        let width = r.width;
        // A gently wavy line; the wiggles that suit the migration grow.
        let (x0, x1) = (-4.0 * width, gw as f32 + 4.0 * width);
        let y0 = gh as f32 * (0.35 + 0.3 * rng.f32());
        let waves: Vec<(f32, f32, f32)> = (0..4)
            .map(|_| {
                let amp = width * (0.2 + 0.4 * rng.f32());
                let len = width * (6.0 + 10.0 * rng.f32());
                (amp, std::f32::consts::TAU / len, std::f32::consts::TAU * rng.f32())
            })
            .collect();
        let ds = width * SPACING;
        let n = ((x1 - x0) / ds).ceil() as usize + 1;
        r.nodes = (0..n)
            .map(|i| {
                let x = x0 + (x1 - x0) * i as f32 / (n - 1) as f32;
                let y = y0 + r.mobility(x) * waves.iter().map(|(a, k, p)| a * (k * x + p).sin()).sum::<f32>();
                [x, y]
            })
            .collect();
        // Run it on until it meanders like a river that's been at it a while.
        for _ in 0..800 {
            r.advance(300.0, 1.0);
            if r.sinuosity() > 1.8 {
                break;
            }
        }
        // And a little more, drawing as it goes, to lay down point bars.
        for _ in 0..6 {
            r.raster();
            r.advance(150.0, 1.0);
        }
        r
    }

    fn force(&mut self, rng: &mut Rng) {
        let w = self.width;
        self.forcing = (0..3)
            .map(|_| (rng.f32() + 0.5, std::f32::consts::TAU / (w * (8.0 + 8.0 * rng.f32())), std::f32::consts::TAU * rng.f32()))
            .collect();
    }

    fn blank(gw: u32, gh: u32, width: f32, scale: f32) -> Self {
        let mut r = River {
            gw,
            gh,
            width: Self::fit_width(width, gh),
            nodes: Vec::new(),
            lakes: Vec::new(),
            clock: 0.0,
            fw: 0,
            fh: 0,
            sx: 1.0,
            sy: 1.0,
            dist: Vec::new(),
            flow: Vec::new(),
            wet: Vec::new(),
            dried: Vec::new(),
            moved: 0.0,
            cut: true,
            forcing: Vec::new(),
        };
        r.alloc(scale);
        r
    }

    /// Restore a saved river, if it was for a grid this size.
    pub fn restore(json: &[u8], gw: u32, gh: u32) -> Option<Self> {
        let s: Saved = serde_json::from_slice(json).ok()?;
        if (s.gw, s.gh) != (gw, gh) || s.nodes.len() < 4 {
            return None;
        }
        let mut r = River::blank(gw, gh, s.width, default_scale(gw, gh));
        (r.width, r.nodes, r.lakes, r.clock) = (s.width, s.nodes, s.lakes, s.clock);
        r.force(&mut Rng::from_clock());
        Some(r)
    }

    pub fn save(&self) -> Vec<u8> {
        let s = Saved {
            gw: self.gw,
            gh: self.gh,
            width: self.width,
            nodes: self.nodes.clone(),
            lakes: self.lakes.clone(),
            clock: self.clock,
        };
        serde_json::to_vec(&s).unwrap_or_default()
    }

    fn fit_width(width: f32, gh: u32) -> f32 {
        width.clamp(2.0, (gh as f32 / 6.0).max(2.0))
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    fn alloc(&mut self, scale: f32) {
        self.fw = ((self.gw as f32 * scale).ceil() as u32).max(1);
        self.fh = ((self.gh as f32 * scale).ceil() as u32).max(1);
        self.sx = self.fw as f32 / self.gw as f32;
        self.sy = self.fh as f32 / self.gh as f32;
        let n = (self.fw * self.fh) as usize;
        self.dist = vec![0.0; n];
        self.flow = vec![[0.0; 2]; n];
        self.wet = vec![false; n];
        self.dried = vec![f64::NEG_INFINITY; n];
        self.cut = true;
    }

    /// Stretch the river onto a resized grid.
    pub fn fit(&mut self, gw: u32, gh: u32) {
        if (gw, gh) == (self.gw, self.gh) {
            return;
        }
        let (kx, ky) = (gw as f32 / self.gw as f32, gh as f32 / self.gh as f32);
        for p in self.nodes.iter_mut().chain(self.lakes.iter_mut().flat_map(|l| l.pts.iter_mut())) {
            *p = [p[0] * kx, p[1] * ky];
        }
        (self.gw, self.gh) = (gw, gh);
        self.width = Self::fit_width(self.width, gh);
        self.alloc(default_scale(gw, gh));
        self.resample();
    }

    pub fn set_width(&mut self, width: f32) {
        let w = Self::fit_width(width, self.gh);
        if (w - self.width).abs() > 1e-3 {
            self.width = w;
            self.resample();
            self.cut = true;
        }
    }

    /// Whether the river has changed enough since the last `raster` to
    /// be worth uploading again.
    pub fn changed(&self) -> bool {
        self.cut || self.moved > 0.1
    }

    /// Nodes this close to the grid's left and right edges hardly move, so
    /// the river enters and leaves at the same height (seamless with wrap).
    fn mobility(&self, x: f32) -> f32 {
        let w = self.width;
        smoothstep(0.0, 2.0 * w, x) * smoothstep(0.0, 2.0 * w, self.gw as f32 - x)
    }

    fn sinuosity(&self) -> f32 {
        let len: f32 = self.nodes.windows(2).map(|w| dist(w[0], w[1])).sum();
        len / (self.gw as f32 + 8.0 * self.width)
    }

    /// Run the river on by `steps` sim steps at meander `rate`.
    // Nodes are updated from their neighbors, so index loops read best.
    #[allow(clippy::needless_range_loop)]
    pub fn advance(&mut self, steps: f32, rate: f32) {
        let mut left = steps * rate;
        if left <= 0.0 {
            return;
        }
        self.clock += left as f64;
        let ds = self.width * SPACING;
        while left > 0.0 {
            let v = self.velocities();
            let vmax = v.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
            let dt = left.min(0.08 * ds / vmax);
            let n = self.nodes.len();
            let (gw, gh) = (self.gw as f32, self.gh as f32);
            // Bends slow to a stop as they near the top and bottom edges
            // (and the sides, past the ends), rather than run along them.
            let m0 = self.width * 0.5 + 1.0;
            let m1 = m0 + 3.0 * self.width;
            let mut moved = 0.0f32;
            for i in 1..n - 1 {
                let (a, b) = (self.nodes[i - 1], self.nodes[i + 1]);
                let (tx, ty) = (b[0] - a[0], b[1] - a[1]);
                let l = (tx * tx + ty * ty).sqrt().max(1e-6);
                let d = v[i] * dt;
                let (mx, my) = (d * ty / l, -d * tx / l);
                let p = self.nodes[i];
                let room_x = if mx < 0.0 { p[0] + 2.0 * self.width } else { gw + 2.0 * self.width - p[0] };
                let room_y = if my < 0.0 { p[1] } else { gh - p[1] };
                let k = smoothstep(m0, m1, room_x).min(smoothstep(m0, m1, room_y));
                self.nodes[i] = [p[0] + k * mx, (p[1] + k * my).clamp(m0, (gh - m0).max(m0))];
                moved = moved.max((k * d).abs());
            }
            self.moved += moved;
            self.resample();
            left -= dt;
        }
        self.cutoffs();
        self.lakes.retain(|l| self.clock - l.born < LAKE_LIFE);
    }

    /// Migration rate of each node, in cells per river-clock step, to its
    /// right (looking downstream).
    #[allow(clippy::needless_range_loop)]
    fn velocities(&self) -> Vec<f32> {
        let n = self.nodes.len();
        let w = self.width;
        let mut r0 = vec![0.0f32; n];
        for i in 1..n - 1 {
            let (a, p, b) = (self.nodes[i - 1], self.nodes[i], self.nodes[i + 1]);
            let (dx, dy) = ((b[0] - a[0]) * 0.5, (b[1] - a[1]) * 0.5);
            let (ddx, ddy) = (b[0] - 2.0 * p[0] + a[0], b[1] - 2.0 * p[1] + a[1]);
            let c = (dx * ddy - dy * ddx) / (dx * dx + dy * dy).powf(1.5).max(1e-6);
            r0[i] = w * c;
        }
        // A light smoothing keeps node-scale wiggles from growing.
        let r0: Vec<f32> = (0..n)
            .map(|i| if i == 0 || i == n - 1 { 0.0 } else { 0.25 * r0[i - 1] + 0.5 * r0[i] + 0.25 * r0[i + 1] })
            .collect();
        let decay = (-SPACING / MEMORY).exp();
        let (mut acc, mut wsum) = (0.0f32, 0.0f32);
        (0..n)
            .map(|i| {
                acc = acc * decay + r0[i];
                wsum = wsum * decay + 1.0;
                let r1 = OMEGA * r0[i] + GAMMA * acc / wsum;
                let x = self.nodes[i][0];
                let f: f32 = self.forcing.iter().map(|(a, k, p)| a * (k * x + p).sin()).sum();
                // Positive curvature (bending left) moves the node right.
                KL * (r1 + FORCING * f) * self.mobility(x)
            })
            .collect()
    }

    /// Respace the nodes evenly along the channel, keeping both ends.
    fn resample(&mut self) {
        let ds = self.width * SPACING;
        let pts = &self.nodes;
        let mut cum = Vec::with_capacity(pts.len());
        let mut s = 0.0;
        cum.push(0.0);
        for w in pts.windows(2) {
            s += dist(w[0], w[1]);
            cum.push(s);
        }
        let n = ((s / ds).round() as usize).max(2);
        let mut out = Vec::with_capacity(n + 1);
        let mut j = 0;
        for k in 0..=n {
            let t = s * k as f32 / n as f32;
            while j + 2 < pts.len() && cum[j + 1] < t {
                j += 1;
            }
            let f = ((t - cum[j]) / (cum[j + 1] - cum[j]).max(1e-6)).clamp(0.0, 1.0);
            out.push([pts[j][0] + f * (pts[j + 1][0] - pts[j][0]), pts[j][1] + f * (pts[j + 1][1] - pts[j][1])]);
        }
        self.nodes = out;
    }

    /// Cut off loops whose necks have closed, leaving oxbow lakes.
    fn cutoffs(&mut self) {
        let cr = CUTOFF * self.width;
        let gap = (LOOP / SPACING).ceil() as usize;
        let trim = (1.0 / SPACING).ceil() as usize;
        loop {
            let mut bins: std::collections::HashMap<(i32, i32), Vec<usize>> = Default::default();
            for (i, p) in self.nodes.iter().enumerate() {
                bins.entry(((p[0] / cr).floor() as i32, (p[1] / cr).floor() as i32)).or_default().push(i);
            }
            let mut found = None;
            'search: for (i, p) in self.nodes.iter().enumerate() {
                let (bx, by) = ((p[0] / cr).floor() as i32, (p[1] / cr).floor() as i32);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        for &j in bins.get(&(bx + dx, by + dy)).into_iter().flatten() {
                            if j > i + gap && dist(*p, self.nodes[j]) < cr {
                                found = Some((i, j));
                                break 'search;
                            }
                        }
                    }
                }
            }
            let Some((i, j)) = found else { break };
            let mut pts: std::collections::VecDeque<[f32; 2]> = self.nodes[i + trim..=j - trim].iter().copied().collect();
            self.nodes.drain(i + 1..j);
            // Trim the lake back from the new channel, so it lies apart.
            let near = |q: [f32; 2], nodes: &[[f32; 2]]| nodes.iter().any(|&p| dist(p, q) < 1.5 * self.width);
            while pts.front().is_some_and(|&q| near(q, &self.nodes)) {
                pts.pop_front();
            }
            while pts.back().is_some_and(|&q| near(q, &self.nodes)) {
                pts.pop_back();
            }
            if pts.len() > 4 {
                self.lakes.push(Lake { pts: pts.into(), hw: self.width * 0.5, born: self.clock });
            }
            self.resample();
            self.cut = true;
        }
    }

    /// How far from water the field keeps track, in cells.
    fn reach(&self) -> f32 {
        self.width + 2.0
    }

    /// Draw the field (see the module doc), as RGBA f16 texels, rows from
    /// y = 0 (y up, like the grid).
    pub fn raster(&mut self) -> Vec<u16> {
        let far = self.reach();
        self.dist.fill(far);
        self.flow.fill([0.0; 2]);
        let hw = self.width * 0.5;
        let nodes = std::mem::take(&mut self.nodes);
        self.stamp(&nodes, hw, 1.0);
        self.nodes = nodes;
        let lakes = std::mem::take(&mut self.lakes);
        for l in &lakes {
            let age = ((self.clock - l.born) / LAKE_LIFE).clamp(0.0, 1.0) as f32;
            let hw = l.hw * (1.0 - age).sqrt();
            if hw > 0.25 {
                self.stamp(&l.pts, hw, 0.25 * (1.0 - age));
            }
        }
        self.lakes = lakes;
        let mut out = Vec::with_capacity(self.dist.len() * 4);
        let far16 = f16(far);
        for i in 0..self.dist.len() {
            if self.dist[i] >= far && !self.wet[i] && self.dried[i] == f64::NEG_INFINITY {
                out.extend([far16, 0, 0, 0]);
                continue;
            }
            let wet = self.dist[i] < 0.0;
            if self.wet[i] && !wet {
                self.dried[i] = self.clock;
            }
            self.wet[i] = wet;
            let sand = (1.0 - (self.clock - self.dried[i]) / DEPOSIT_LIFE).clamp(0.0, 1.0) as f32;
            let f = self.flow[i];
            out.extend([f16(self.dist[i]), f16(f[0]), f16(f[1]), f16(if wet { 0.0 } else { sand })]);
        }
        self.moved = 0.0;
        self.cut = false;
        out
    }

    /// Mark texels within `reach` of the water's edge along a polyline.
    /// Each segment only visits the strip beside it (with some overlap, to
    /// cover the outsides of bends), not its whole bounding box.
    fn stamp(&mut self, pts: &[[f32; 2]], hw: f32, flow: f32) {
        let r = hw + self.reach();
        for s in pts.windows(2) {
            let (a, b) = (s[0], s[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = (dx * dx + dy * dy).max(1e-9);
            let l = l2.sqrt();
            let (ux, uy) = (dx / l, dy / l);
            let dir = [ux * flow, uy * flow];
            // Along the segment, from a: [-k, l + k]; across it: [-r, r].
            let k = (0.4 * r).max(l);
            let y0 = ((a[1].min(b[1]) - r - k) * self.sy).floor().max(0.0) as i32;
            let y1 = ((a[1].max(b[1]) + r + k) * self.sy).ceil().min(self.fh as f32 - 1.0) as i32;
            for ty in y0..=y1 {
                let qy = (ty as f32 + 0.5) / self.sy - a[1];
                // x (relative to a) where -k <= ux x + uy qy <= l + k, and
                // -r <= -uy x + ux qy <= r.
                let (mut lo, mut hi) = (f32::NEG_INFINITY, f32::INFINITY);
                for (c, m, n0, n1) in [(ux, uy * qy, -k, l + k), (-uy, ux * qy, -r, r)] {
                    if c.abs() < 1e-6 {
                        if m < n0 || m > n1 {
                            (lo, hi) = (1.0, 0.0);
                        }
                    } else {
                        let (e0, e1) = ((n0 - m) / c, (n1 - m) / c);
                        lo = lo.max(e0.min(e1));
                        hi = hi.min(e0.max(e1));
                    }
                }
                if lo > hi {
                    continue;
                }
                let x0 = ((a[0] + lo) * self.sx - 0.5).ceil().max(0.0) as i32;
                let x1 = ((a[0] + hi) * self.sx - 0.5).floor().min(self.fw as f32 - 1.0) as i32;
                let row = ty as u32 * self.fw;
                for tx in x0..=x1 {
                    let qx = (tx as f32 + 0.5) / self.sx - a[0];
                    let t = ((qx * dx + qy * dy) / l2).clamp(0.0, 1.0);
                    let (ex, ey) = (qx - t * dx, qy - t * dy);
                    let e = (ex * ex + ey * ey).sqrt() - hw;
                    let i = (row + tx as u32) as usize;
                    if e < self.dist[i] {
                        self.dist[i] = e;
                        self.flow[i] = dir;
                    }
                }
            }
        }
    }

    /// Signed distance from a cell's center to water, in cells, as of the
    /// last `raster` (the texel the step shader reads).
    pub fn cell_dist(&self, x: u32, y: u32) -> f32 {
        let tx = (((x as f32 + 0.5) * self.sx) as u32).min(self.fw - 1);
        let ty = (((y as f32 + 0.5) * self.sy) as u32).min(self.fh - 1);
        self.dist[(ty * self.fw + tx) as usize]
    }

    pub fn water(&self, x: u32, y: u32) -> bool {
        self.cell_dist(x, y) < 0.0
    }
}

/// A field with no water in it, for when the river is off.
pub fn dry_field() -> Vec<u16> {
    vec![f16(1e3), 0, 0, 0]
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// f32 to IEEE half, truncating; out-of-range values saturate.
fn f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    if e <= 0 {
        return sign;
    }
    if e >= 31 {
        return sign | 0x7bff;
    }
    sign | (e as u16) << 10 | ((b & 0x7f_ffff) >> 13) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16(1.0), 0x3c00);
        assert_eq!(f16(-2.0), 0xc000);
        assert_eq!(f16(0.0), 0);
        assert_eq!(f16(1e6), 0x7bff);
    }

    #[test]
    fn meanders_and_cuts_off_loops() {
        let mut r = River::with_scale(400, 200, 6.0, 3, 1.0);
        assert!(r.sinuosity() > 1.3, "sinuosity {}", r.sinuosity());
        for _ in 0..400 {
            r.advance(300.0, 1.0);
        }
        assert!(r.clock > 1e5);
        // Cutoffs keep it from coiling up without limit.
        assert!(r.sinuosity() < 4.0, "sinuosity {}", r.sinuosity());
        r.raster();
        let water = (0..400).flat_map(|x| (0..200).map(move |y| (x, y))).filter(|&(x, y)| r.water(x, y)).count();
        assert!(water > 400 * 6 && water < 400 * 6 * 6, "water {water}");
        // The river crosses every column (so it splits the forest).
        assert!((0..400).all(|x| (0..200).any(|y| r.water(x, y))));
    }

    #[test]
    fn saves_and_restores() {
        let r = River::with_scale(120, 80, 4.0, 5, 1.0);
        let s = River::restore(&r.save(), 120, 80).unwrap();
        assert_eq!(s.nodes, r.nodes);
        assert!(River::restore(&r.save(), 121, 80).is_none());
    }
}
