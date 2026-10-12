//! A meandering river across the grid, on the CPU.
//!
//! The channel is a centerline of nodes, flowing left to right, moved with
//! the Howard–Knutson migration rate (as in meanderpy): each node moves
//! sideways in proportion to the channel curvature at the node (pulling the
//! bend in) plus a weighted average of the curvature just upstream (pushing
//! it out, and downstream). Bends grow and drift. When two stretches of the
//! channel come within about a width of each other, the loop between them is
//! cut off and left behind as an oxbow lake: water standing in the trench in
//! the height map, which evaporates, silts up, rounds out and dries to sand
//! and then green. The river runs along a valley in an unrendered height map
//! (`terrain`), which holds the meander belt in and records where the channel
//! has been; `lake` holds the water in its hollows.
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

use crate::lake::{self, Lake, Lakes};
use crate::rng::Rng;
use crate::shaders::state::{TREE, WATER};
use crate::terrain::{self, Terrain};

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
/// River clock steps (steps × meander rate) for a trench to silt up, which
/// sets the pace of the lakes, and for fresh sand to green over.
pub const LAKE_LIFE: f64 = 15000.0;
const DEPOSIT_LIFE: f64 = 900.0;
/// The field texture is capped at this many texels; a full one is about 12 MB.
const MAX_TEXELS: f32 = 1.5e6;
/// Strength of a weak, fixed sideways push (as a fraction of KL), standing
/// in for the unevenness of real ground: it starts straight stretches
/// meandering again after a big cutoff.
const FORCING: f32 = 0.15;
/// Trees grow this much faster on the bank (see `riparian`).
pub const RIPARIAN: f32 = 1.0;
/// Roots hold banks together: a bend whose cut bank is bare migrates at
/// `BARE` × the usual rate, and one under full tree cover at `WOODED` ×.
/// Half cover is the usual rate.
const BARE: f32 = 1.6;
const WOODED: f32 = 0.4;
/// Ground higher than the valley floor slows a bank's migration: by half
/// where it stands this many widths above the floor.
const UPHILL: f32 = 0.1;
/// A bank on a slope slides down it at this × KL × the slope, which brings
/// a river that has strayed up a valley wall back toward the floor.
const DRIFT: f32 = 1.0;
/// A chute cutoff joins two stretches of channel at least this many widths
/// apart along it and less than `CHUTE_ACROSS` widths apart across...
const CHUTE_ALONG: f32 = 10.0;
const CHUTE_ACROSS: f32 = 3.0;
/// ...with probability per river-clock step of this × (along ÷ across) ×
/// how low the ground between them lies, for the likeliest such pair. A
/// long straight limb 3 widths from its neighbor goes in a few thousand
/// steps; an ordinary loop, whose neck closes by itself first, rarely does.
const CHUTE_RATE: f64 = 2e-5;
/// Chutes are looked for this often, in river-clock steps.
const CHUTE_EVERY: f64 = 400.0;
/// Clay slows a bank to 1 / (1 + this × hardness) of its rate.
const PLUG: f32 = 4.0;
/// Floods come at random, this many river-clock steps apart on average, and
/// last about `FLOOD_LEN` steps, over which the river's half-width swells
/// by `FLOOD_RISE` and falls back.
const FLOOD_EVERY: f64 = 4000.0;
const FLOOD_LEN: f64 = 400.0;
const FLOOD_RISE: f32 = 0.4;

/// What's saved between runs.
#[derive(Serialize, Deserialize)]
struct Saved {
    gw: u32,
    gh: u32,
    width: f32,
    nodes: Vec<[f32; 2]>,
    lakes: Vec<Lake>,
    clock: f64,
    terrain: terrain::Saved,
}

pub struct River {
    gw: u32,
    gh: u32,
    /// Channel width in cells.
    width: f32,
    nodes: Vec<[f32; 2]>,
    lakes: Lakes,
    /// Steps × meander rate.
    clock: f64,
    terrain: Terrain,
    /// The clock when the ground was last worked, and when chutes were last
    /// looked for.
    ground_clock: f64,
    chute_clock: f64,
    /// For chute cutoffs and floods; not saved.
    rng: Rng,
    /// When the next (or current) flood starts and how long it lasts, and the
    /// flood stage as last drawn.
    flood_at: f64,
    flood_len: f64,
    drawn_stage: f32,
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
    /// Tree cover of the land in `cover_block`-cell squares, `cover_w` per
    /// row, from the last grid read back (see `set_cover`); empty until then.
    cover: Vec<f32>,
    cover_w: usize,
    cover_block: f32,
}

/// Field texels per cell: fine enough for smooth banks on big cells, and
/// capped in total.
fn default_scale(gw: u32, gh: u32) -> f32 {
    (640.0 / gw.max(gh) as f32).clamp(1.0, 16.0).min((MAX_TEXELS / (gw * gh) as f32).sqrt())
}

pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
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
        let width = Self::fit_width(width, gh);
        let mut r = Self::blank(gw, gh, width, scale, Terrain::new(gw, gh, width, rng.next_u64()));
        r.rng = Rng::new(rng.next_u64());
        r.force(&mut rng);
        // A gently wavy line along the valley; the wiggles that suit the
        // migration grow.
        let (x0, x1) = (-4.0 * width, gw as f32 + 4.0 * width);
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
                let y = r.terrain.valley_y(x) + r.mobility(x) * waves.iter().map(|(a, k, p)| a * (k * x + p).sin()).sum::<f32>();
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
        // And a little more, drawing as it goes, to lay down point bars and
        // let the latest oxbows (only) settle in.
        r.lakes.clear();
        r.ground_clock = r.clock;
        for _ in 0..8 {
            r.advance(400.0, 1.0);
            r.raster();
        }
        r
    }

    fn force(&mut self, rng: &mut Rng) {
        let w = self.width;
        self.forcing = (0..3)
            .map(|_| (rng.f32() + 0.5, std::f32::consts::TAU / (w * (8.0 + 8.0 * rng.f32())), std::f32::consts::TAU * rng.f32()))
            .collect();
    }

    fn blank(gw: u32, gh: u32, width: f32, scale: f32, terrain: Terrain) -> Self {
        let mut r = River {
            gw,
            gh,
            width: Self::fit_width(width, gh),
            nodes: Vec::new(),
            lakes: Lakes::default(),
            clock: 0.0,
            terrain,
            ground_clock: 0.0,
            chute_clock: 0.0,
            rng: Rng::from_clock(),
            flood_at: 0.0,
            flood_len: FLOOD_LEN,
            drawn_stage: 0.0,
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
            cover: Vec::new(),
            cover_w: 0,
            cover_block: 1.0,
        };
        r.alloc(scale);
        r.schedule_flood();
        r
    }

    /// Restore a saved river, if it was for a grid this size.
    pub fn restore(json: &[u8], gw: u32, gh: u32) -> Option<Self> {
        let s: Saved = serde_json::from_slice(json).ok()?;
        if (s.gw, s.gh) != (gw, gh) || s.nodes.len() < 4 {
            return None;
        }
        let terrain = Terrain::restore(&s.terrain, gw, gh)?;
        let mut r = River::blank(gw, gh, s.width, default_scale(gw, gh), terrain);
        (r.width, r.nodes, r.lakes, r.clock) = (s.width, s.nodes, Lakes::from_saved(s.lakes), s.clock);
        (r.ground_clock, r.chute_clock) = (r.clock, r.clock);
        r.schedule_flood();
        r.force(&mut Rng::from_clock());
        Some(r)
    }

    pub fn save(&self) -> Vec<u8> {
        let s = Saved {
            gw: self.gw,
            gh: self.gh,
            width: self.width,
            nodes: self.nodes.clone(),
            lakes: self.lakes.saved(),
            clock: self.clock,
            terrain: self.terrain.save(),
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
        for p in &mut self.nodes {
            *p = [p[0] * kx, p[1] * ky];
        }
        // The new ground has no hollows for the water to stand in.
        self.lakes.clear();
        (self.gw, self.gh) = (gw, gh);
        self.cover.clear();
        self.width = Self::fit_width(self.width, gh);
        self.alloc(default_scale(gw, gh));
        self.terrain = self.terrain.resized(gw, gh, self.width);
        self.resample();
    }

    pub fn set_width(&mut self, width: f32) {
        let w = Self::fit_width(width, self.gh);
        if (w - self.width).abs() > 1e-3 {
            self.width = w;
            self.terrain = self.terrain.resized(self.gw, self.gh, w);
            self.lakes.clear();
            self.resample();
            self.cut = true;
        }
    }

    /// Whether the river has changed enough since the last `raster` to
    /// be worth uploading again.
    pub fn changed(&self) -> bool {
        let swell = (self.flood_stage() - self.drawn_stage).abs() * 0.5 * self.width * FLOOD_RISE;
        self.cut || self.moved > 0.1 || swell > 0.1 || self.lakes.changed()
    }

    /// How high the river is in the current flood: 0 (none) to 1.
    fn flood_stage(&self) -> f32 {
        let t = (self.clock - self.flood_at) / self.flood_len;
        if t <= 0.0 || t >= 1.0 { 0.0 } else { (std::f64::consts::PI * t).sin().powi(2) as f32 }
    }

    /// Pick the next flood: an exponential wait from now.
    fn schedule_flood(&mut self) {
        self.flood_at = self.clock - FLOOD_EVERY * (1.0 - self.rng.f64()).ln();
        self.flood_len = FLOOD_LEN * (0.6 + 0.8 * self.rng.f64());
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
        if self.clock - self.chute_clock >= CHUTE_EVERY {
            self.chute(self.clock - self.chute_clock);
            self.chute_clock = self.clock;
        }
        if self.clock > self.flood_at + self.flood_len {
            self.schedule_flood();
        }
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
        let v: Vec<f32> = (0..n)
            .map(|i| {
                acc = acc * decay + r0[i];
                wsum = wsum * decay + 1.0;
                let r1 = OMEGA * r0[i] + GAMMA * acc / wsum;
                let x = self.nodes[i][0];
                let f: f32 = self.forcing.iter().map(|(a, k, p)| a * (k * x + p).sin()).sum();
                // Positive curvature (bending left) moves the node right.
                KL * (r1 + FORCING * f) * self.mobility(x)
            })
            .collect();
        // Banks that climb out of the valley floor hold the river in, and
        // wash it back down the slope.
        let v: Vec<f32> = (0..n)
            .map(|i| {
                let (right, left) = (self.bank(i, 1.0), self.bank(i, -1.0));
                let slope = (self.high(right) - self.high(left)) / (2.0 * w);
                let v = v[i] - DRIFT * KL * slope * self.mobility(self.nodes[i][0]);
                let q = self.bank(i, v);
                let up = self.high(q) / (UPHILL * w);
                // And old lake beds, hardened to clay, resist.
                v / (1.0 + up * up).powi(2) / (1.0 + PLUG * self.terrain.hardness_at(q[0], q[1]))
            })
            .collect();
        if self.cover.is_empty() {
            return v;
        }
        // Scale by how easily the bank each node is cutting into erodes,
        // smoothed along the channel so it doesn't start node-scale wiggles.
        let m: Vec<f32> = (0..n)
            .map(|i| {
                let q = self.bank(i, v[i]);
                let c = self.cover_at(q[0], q[1]);
                WOODED + (BARE - WOODED) * (1.0 - c)
            })
            .collect();
        let m: Vec<f32> = (0..n).map(|i| 0.25 * m[i.saturating_sub(1)] + 0.5 * m[i] + 0.25 * m[(i + 1).min(n - 1)]).collect();
        v.iter().zip(m).map(|(v, m)| v * m).collect()
    }

    /// How far above the valley floor the ground is at `q`, if it is.
    fn high(&self, q: [f32; 2]) -> f32 {
        self.terrain.relief_at(q[0], q[1]).max(0.0)
    }

    /// The point one width out from node `i` on the side `v` (a migration
    /// rate) moves it toward.
    fn bank(&self, i: usize, v: f32) -> [f32; 2] {
        let (a, b) = (self.nodes[i.saturating_sub(1)], self.nodes[(i + 1).min(self.nodes.len() - 1)]);
        let (tx, ty) = (b[0] - a[0], b[1] - a[1]);
        let l = (tx * tx + ty * ty).sqrt().max(1e-6);
        let o = v.signum() * self.width;
        let p = self.nodes[i];
        [p[0] + o * ty / l, p[1] - o * tx / l]
    }

    /// Take tree cover from a copy of the grid (`width`×`height` RGBA cells,
    /// state in r), counting only land. Grids of another size are ignored.
    pub fn set_cover(&mut self, cells: &[u8], width: u32, height: u32) {
        if (width, height) != (self.gw, self.gh) || cells.len() != (width * height * 4) as usize {
            return;
        }
        let b = (self.width * 0.5).round().max(1.0) as usize;
        let (gw, gh) = (width as usize, height as usize);
        let (cw, ch) = (gw.div_ceil(b), gh.div_ceil(b));
        let mut trees = vec![0u32; cw * ch];
        let mut land = vec![0u32; cw * ch];
        for y in 0..gh {
            for x in 0..gw {
                let s = cells[(y * gw + x) * 4];
                let k = (y / b) * cw + x / b;
                land[k] += (s != WATER) as u32;
                trees[k] += (s == TREE) as u32;
            }
        }
        // All-water squares count as half covered, the neutral rate.
        self.cover = trees.iter().zip(&land).map(|(&t, &l)| if l == 0 { 0.5 } else { t as f32 / l as f32 }).collect();
        self.cover_w = cw;
        self.cover_block = b as f32;
    }

    /// Tree cover at a point in cells, interpolated between squares.
    fn cover_at(&self, x: f32, y: f32) -> f32 {
        let cw = self.cover_w as i32;
        let ch = (self.cover.len() / self.cover_w) as i32;
        let (fx, fy) = (x / self.cover_block - 0.5, y / self.cover_block - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let at = |x: i32, y: i32| self.cover[(y.clamp(0, ch - 1) * cw + x.clamp(0, cw - 1)) as usize];
        let (x0, y0) = (x0 as i32, y0 as i32);
        let top = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
        let bottom = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        bottom * (1.0 - ty) + top * ty
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
            self.cut_loop(i, j);
        }
    }

    /// Cut off the loop between nodes `i` and `j`, leaving a lake.
    fn cut_loop(&mut self, i: usize, j: usize) {
        let trim = (1.0 / SPACING).ceil() as usize;
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
            let pts: Vec<[f32; 2]> = pts.into();
            self.terrain.carve(&pts, self.width);
            self.lakes.add(&self.terrain, &pts);
        }
        self.resample();
        self.cut = true;
    }

    /// Maybe cut a chute: join two stretches of channel that run side by
    /// side, far apart along the river, which Howard–Knutson never would
    /// (a straight limb doesn't move). `dt` river-clock steps have passed.
    fn chute(&mut self, dt: f64) {
        let w = self.width;
        let across = CHUTE_ACROSS * w;
        let gap = (CHUTE_ALONG / SPACING).ceil() as usize;
        // Nodes in a grid of cells `across` wide, as linked lists.
        let min = self.nodes.iter().fold([f32::INFINITY; 2], |m, p| [m[0].min(p[0]), m[1].min(p[1])]);
        let at = |p: [f32; 2]| (((p[0] - min[0]) / across) as i32, ((p[1] - min[1]) / across) as i32);
        let cols = self.nodes.iter().map(|&p| at(p).0).max().unwrap_or(0) + 1;
        let rows = self.nodes.iter().map(|&p| at(p).1).max().unwrap_or(0) + 1;
        let mut head = vec![usize::MAX; (cols * rows) as usize];
        let mut next = vec![usize::MAX; self.nodes.len()];
        for (i, &p) in self.nodes.iter().enumerate() {
            let (cx, cy) = at(p);
            next[i] = std::mem::replace(&mut head[(cy * cols + cx) as usize], i);
        }
        let mut best = (0.0f64, 0, 0);
        for (i, &p) in self.nodes.iter().enumerate() {
            let (cx, cy) = at(p);
            // The likeliest partner for this node: along ÷ across is highest.
            let mut mate = (0.0f32, 0);
            for gy in (cy - 1).max(0)..=(cy + 1).min(rows - 1) {
                for gx in (cx - 1).max(0)..=(cx + 1).min(cols - 1) {
                    let mut j = head[(gy * cols + gx) as usize];
                    while j != usize::MAX {
                        let d = dist(p, self.nodes[j]);
                        if j > i + gap && d < across {
                            let ratio = (j - i) as f32 * SPACING * w / d.max(w);
                            if ratio > mate.0 {
                                mate = (ratio, j);
                            }
                        }
                        j = next[j];
                    }
                }
            }
            if mate.0 > 0.0 {
                let q = self.nodes[mate.1];
                // Lower ground between them makes the cut likelier.
                let high = (1..4)
                    .map(|k| {
                        let t = 0.25 * k as f32;
                        self.high([p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1])])
                    })
                    .sum::<f32>()
                    / (3.0 * UPHILL * w);
                let score = (mate.0 / (1.0 + high * high)) as f64;
                if score > best.0 {
                    best = (score, i, mate.1);
                }
            }
        }
        if best.0 > 0.0 && self.rng.f64() < 1.0 - (-CHUTE_RATE * best.0 * dt).exp() {
            self.cut_loop(best.1, best.2);
        }
    }

    /// How far from water the field keeps track, in cells.
    fn reach(&self) -> f32 {
        self.width + 2.0
    }

    /// Draw the field (see the module doc), as RGBA f16 texels, rows from
    /// y = 0 (y up, like the grid).
    pub fn raster(&mut self) -> Vec<u16> {
        let dt = self.clock - self.ground_clock;
        self.ground_clock = self.clock;
        self.terrain.update(&self.nodes, self.width, dt);
        let stage = self.flood_stage();
        self.drawn_stage = stage;
        self.lakes.update(&mut self.terrain, self.width, dt as f32, stage);
        let far = self.reach();
        self.dist.fill(far);
        self.flow.fill([0.0; 2]);
        let hw = self.width * 0.5 * (1.0 + FLOOD_RISE * stage);
        let nodes = std::mem::take(&mut self.nodes);
        self.stamp(&nodes, hw, 1.0);
        self.nodes = nodes;
        let mut field = lake::Field { w: self.fw, h: self.fh, sx: self.sx, sy: self.sy, dist: &mut self.dist, flow: &mut self.flow };
        self.lakes.stamp(&self.terrain, &mut field, self.width, far);
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
    fn bare_banks_erode_faster() {
        let (w, h) = (200u32, 100u32);
        let mut r = River::with_scale(w, h, 4.0, 7, 1.0);
        let base = r.velocities();
        let grid = |s: u8| vec![s; (w * h * 4) as usize];
        r.set_cover(&grid(TREE), w, h);
        let wooded = r.velocities();
        r.set_cover(&grid(crate::shaders::state::EMPTY), w, h);
        let bare = r.velocities();
        for i in 0..base.len() {
            assert!((wooded[i] - WOODED * base[i]).abs() <= 1e-6 + 1e-4 * base[i].abs());
            assert!((bare[i] - BARE * base[i]).abs() <= 1e-6 + 1e-4 * base[i].abs());
        }
        // Another size of grid is ignored.
        r.set_cover(&grid(TREE), w + 1, h);
        assert_eq!(r.velocities(), bare);
    }

    #[test]
    fn saves_and_restores() {
        let r = River::with_scale(120, 80, 4.0, 5, 1.0);
        let s = River::restore(&r.save(), 120, 80).unwrap();
        assert_eq!(s.nodes, r.nodes);
        assert!(River::restore(&r.save(), 121, 80).is_none());
        // Ground and all.
        // (Quantizing can tip a texel at the pit threshold either way.)
        assert!(s.terrain.pits().count().abs_diff(r.terrain.pits().count()) <= 5);
        assert!((s.terrain.relief_at(50.0, 20.0) - r.terrain.relief_at(50.0, 20.0)).abs() < 0.01);
        // An old save, from before there was terrain, is dropped.
        let mut old: serde_json::Value = serde_json::from_slice(&r.save()).unwrap();
        old.as_object_mut().unwrap().remove("terrain");
        assert!(River::restore(&serde_json::to_vec(&old).unwrap(), 120, 80).is_none());
    }

    #[test]
    fn saves_small_enough_for_local_storage() {
        let mut r = River::with_scale(640, 360, 2.0, 5, 1.0);
        for _ in 0..40 {
            r.advance(300.0, 1.0);
            r.raster();
        }
        let n = r.save().len();
        assert!(n < 300_000, "{n} bytes");
        eprintln!("save: {n} bytes");
    }

    #[test]
    fn starts_in_the_valley() {
        let r = River::with_scale(300, 300, 5.0, 8, 1.0);
        for p in &r.nodes {
            assert!((p[1] - r.terrain.valley_y(p[0])).abs() < r.terrain.valley_half(), "{p:?}");
        }
    }

    #[test]
    fn stays_in_its_valley_on_a_tall_grid() {
        let (gw, gh, w) = (400, 400, 6.0);
        let mut r = River::with_scale(gw, gh, w, 4, 1.0);
        let reach = r.terrain.valley_half() + 3.0 * w;
        let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
        for k in 0..300 {
            r.advance(300.0, 1.0);
            if k % 5 == 0 {
                r.raster();
            }
            let sin = r.sinuosity();
            (lo, hi) = (lo.min(sin), hi.max(sin));
            for p in r.nodes.iter().filter(|p| p[0] > 3.0 * w && p[0] < gw as f32 - 3.0 * w) {
                assert!((p[1] - r.terrain.valley_y(p[0])).abs() < reach, "step {k}: {p:?}");
            }
        }
        // No comb of tall limbs, and it doesn't just lie straight either.
        assert!(hi < 4.5 && hi > 1.5, "sinuosity {lo}..{hi}");
        // The channel left a trench and its history in the ground.
        assert!(r.terrain.pits().count() > 0);
    }

    #[test]
    fn chutes_cut_a_comb() {
        let w = 6.0;
        let mut r = River::with_scale(400, 400, w, 6, 1.0);
        // Limbs 2.5 widths apart, 15 long, joined at their ends.
        let (y0, x0) = (r.terrain.valley_y(200.0) - 40.0, 80.0);
        let mut pts = vec![[-4.0 * w, y0], [x0 - 20.0, y0]];
        for k in 0..8 {
            let x = x0 + 2.5 * w * k as f32;
            let (a, b) = if k % 2 == 0 { (y0, y0 + 15.0 * w) } else { (y0 + 15.0 * w, y0) };
            pts.extend([[x, a], [x, b]]);
        }
        pts.extend([[x0 + 2.5 * w * 8.0, y0], [404.0 + 4.0 * w, y0]]);
        let mut nodes = vec![pts[0]];
        for q in &pts[1..] {
            let p = *nodes.last().unwrap();
            let n = (dist(p, *q) / (0.3 * w)).ceil() as usize;
            nodes.extend((1..=n).map(|i| [p[0] + (q[0] - p[0]) * i as f32 / n as f32, p[1] + (q[1] - p[1]) * i as f32 / n as f32]));
        }
        r.nodes = nodes;
        r.resample();
        let (before, lakes) = (r.sinuosity(), r.lakes.len());
        // It can't fire at every step: a hundred steps' wait is rarely enough.
        for _ in 0..100 {
            r.chute(1.0);
        }
        assert_eq!(r.lakes.len(), lakes);
        // But left alone long enough, a loop of the comb is cut off.
        r.chute(1e6);
        assert_eq!(r.lakes.len(), lakes + 1);
        assert!(r.sinuosity() < before - 0.3, "{before} -> {}", r.sinuosity());
    }

    #[test]
    fn a_cutoff_leaves_a_lake_that_dries() {
        let mut r = River::with_scale(400, 200, 6.0, 3, 1.0);
        r.lakes.clear();
        let (i, j) = (r.nodes.len() / 3, r.nodes.len() / 3 + 40);
        let loop_pts: Vec<[f32; 2]> = r.nodes[i + 6..j - 6].to_vec();
        r.cut_loop(i, j);
        r.raster();
        assert_eq!(r.lakes.len(), 1);
        // Water lies along the abandoned loop (away from the new channel).
        let wet = loop_pts.iter().filter(|p| r.water(p[0] as u32, p[1] as u32)).count();
        assert!(wet * 2 > loop_pts.len(), "{wet} of {}", loop_pts.len());
        assert!(r.changed());
        let mut steps = 0.0;
        while r.lakes.len() > 0 && steps < 3.0 * LAKE_LIFE {
            r.advance(15.0, 0.0);
            r.clock += 15.0;
            r.raster();
            steps += 15.0;
        }
        assert!(r.lakes.len() == 0 && steps > 0.2 * LAKE_LIFE, "{steps}");
        // It left clay where it stood.
        assert!(loop_pts.iter().any(|p| r.terrain.hardness_at(p[0], p[1]) > 0.3));
    }

    #[test]
    fn clay_slows_migration() {
        let mut r = River::with_scale(300, 200, 5.0, 8, 1.0);
        let base = r.velocities();
        let i = r.nodes.len() / 2;
        let q = r.bank(i, base[i]);
        // A plug on the bank this node cuts into.
        for dx in -3..=3 {
            for dy in -3..=3 {
                let k = r.terrain.index_at(q[0] + dx as f32 * 2.0, q[1] + dy as f32 * 2.0);
                r.terrain.harden(k);
            }
        }
        let slow = r.velocities();
        assert!(slow[i].abs() < base[i].abs() / (1.0 + 0.8 * PLUG), "{} vs {}", slow[i], base[i]);
    }

    #[test]
    fn saves_lakes_and_clay() {
        let mut r = River::with_scale(200, 120, 5.0, 4, 1.0);
        r.lakes.clear();
        let i = r.nodes.len() / 3;
        let (a, b) = (r.nodes[i + 6], r.nodes[i + 30]);
        r.cut_loop(i, i + 40);
        r.raster();
        r.terrain.harden(r.terrain.index_at(a[0], a[1]));
        let s = River::restore(&r.save(), 200, 120).unwrap();
        assert_eq!(s.lakes.len(), r.lakes.len());
        assert!(s.terrain.hardness_at(a[0], a[1]) > 0.1 && s.terrain.hardness_at(b[0] + 60.0, b[1] + 60.0) == 0.0);
        // Restored (with a finer field), it draws about the same water.
        let mut s = s;
        r.flood_at = r.clock + 1e9;
        let (wr, ws) = (r.raster(), s.raster());
        let water = |v: &[u16], sc: f32| v.chunks(4).filter(|p| p[0] & 0x8000 != 0).count() as f32 / (sc * sc);
        let (a, b) = (water(&wr, r.sx), water(&ws, s.sx));
        assert!((a - b).abs() < 0.05 * a, "{a} vs {b}");
    }

    #[test]
    fn floods_swell_the_river_and_recede() {
        let mut r = River::with_scale(300, 200, 6.0, 9, 1.0);
        let count = |r: &River| (0..300).flat_map(|x| (0..200).map(move |y| (x, y))).filter(|&(x, y)| r.water(x, y)).count();
        r.flood_at = r.clock + 1e9;
        r.raster();
        let normal = count(&r);
        (r.flood_at, r.flood_len) = (r.clock - 200.0, 400.0);
        assert!(r.changed());
        r.raster();
        let high = count(&r);
        assert!(high as f32 > 1.2 * normal as f32 && (high as f32) < 1.6 * normal as f32, "{normal} -> {high}");
        // Once over, a new one is scheduled and the water falls back.
        r.advance(400.0, 1.0);
        r.raster();
        assert!(r.flood_at > r.clock - 1.0);
        assert!(count(&r) < high);
    }

    /// Dumps PNGs of the field as it evolves: `TB_DUMP=/dir TB_SEED=n cargo test --release dump -- --ignored`.
    #[test]
    #[ignore]
    fn dump_frames() {
        let dir = std::env::var("TB_DUMP").unwrap();
        let seed: u64 = std::env::var("TB_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let width: f32 = std::env::var("TB_W").ok().and_then(|s| s.parse().ok()).unwrap_or(6.7);
        let every: usize = std::env::var("TB_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(1000);
        let total: usize = std::env::var("TB_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(120000);
        let mut r = River::with_scale(640, 360, width, seed, 1.0);
        let mut t = 0;
        while t <= total {
            // 15 steps between rasters, as at 60 steps/s.
            for _ in 0..every / 15 {
                r.advance(15.0, 1.0);
                r.raster();
            }
            t += every;
            let (w, h) = (r.fw as usize, r.fh as usize);
            let mut img = vec![255u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let (d, f) = (r.dist[i], r.flow[i][0].hypot(r.flow[i][1]));
                    let sand = (1.0 - (r.clock - r.dried[i]) / DEPOSIT_LIFE).clamp(0.0, 1.0) as f32;
                    let clay = r.terrain.hardness_at((x as f32 + 0.5) / r.sx, (y as f32 + 0.5) / r.sy);
                    let mut c = [40.0 + 40.0 * clay, 90.0 - 30.0 * clay, 40.0];
                    if sand > 0.0 && d >= 0.0 {
                        c = [c[0] + (210.0 - c[0]) * sand, c[1] + (190.0 - c[1]) * sand, c[2] + (120.0 - c[2]) * sand];
                    }
                    if d < 0.0 {
                        let k = (f / 0.25).min(1.0);
                        c = if f >= 0.99 { [40.0, 90.0, 220.0] } else { [60.0 - 30.0 * (1.0 - k), 90.0 + 70.0 * k, 90.0 + 130.0 * k] };
                    }
                    let o = ((h - 1 - y) * w + x) * 4;
                    img[o..o + 3].iter_mut().zip(c).for_each(|(p, c)| *p = c as u8);
                }
            }
            let file = std::fs::File::create(format!("{dir}/f{t:06}.png")).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&img).unwrap();
            eprintln!("t={t} lakes={}", r.lakes.len());
        }
    }
}
