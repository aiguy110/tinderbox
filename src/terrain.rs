//! An unrendered height map under the river.
//!
//! Heights are in cells, on a coarse grid (a channel spans about three
//! texels), wrapping in x like the river's ends do. What's stored is the
//! *relief*: height above `surface(x)`, the floodplain level that the river's
//! water stands at (a tiny tilt downstream). So 0 is the valley floor, a
//! negative texel is a pit that would hold water, and a positive one is
//! higher ground.
//!
//! The valley runs across the grid, left to right, along a gentle wave: a flat
//! floor with a little undulation, then walls rising on both sides with hills
//! on them. The channel carves its bed as it goes; where it has left, the
//! trench fills back up (fast near the channel, as point bars; slowly
//! elsewhere, as silt) and rounds out. Only the texels below the surface are
//! tracked for that, so the upkeep is cheap.

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

/// Tilt of the surface downstream, in cells per cell.
const DOWNSTREAM: f32 = 3e-4;
/// Flat valley floor, in widths wide in total (at most `FLOOR_FRAC` of the
/// grid's height).
const FLOOR_WIDTHS: f32 = 24.0;
const FLOOR_FRAC: f32 = 0.7;
/// Steepness of the valley walls, in cells per cell, far from the floor.
const WALL: f32 = 0.15;
/// The valley centerline wanders by at most this fraction of the height.
const MEANDER_FRAC: f32 = 0.05;
/// Undulation of the floor (only upward), in widths.
const FLOOR_NOISE: f32 = 0.1;
/// Hills vary the wall's height by this fraction either way.
const HILLS: f32 = 0.4;
/// Depth of the channel's bed below the surface, in widths.
pub const DEPTH: f32 = 0.3;
/// Texels below the surface by less than this don't count as pits.
const EPS: f32 = 0.02;
/// Fill rates below the surface, in widths per river-clock step: within
/// `NEAR` widths of the channel (point bars), and elsewhere (silt). The slow
/// rate empties a bed in about `LAKE_LIFE` steps.
const NEAR_FILL: f32 = 1.0e-3;
const SILT_FILL: f32 = DEPTH / crate::river::LAKE_LIFE as f32;
const NEAR: f32 = 2.0;
/// Clock steps for a pit's edges to smooth by about a quarter toward their
/// neighbors.
const SMOOTH_TIME: f32 = 600.0;
/// Quantization of saved heights: steps per cell.
const QUANT: f32 = 64.0;
/// Most texels in the grid.
const MAX_TEXELS: f32 = 1.5e5;

pub struct Terrain {
    gw: f32,
    gh: f32,
    seed: u64,
    width: f32,
    /// Texels across and up, and their size in cells.
    tw: usize,
    th: usize,
    cx: f32,
    cy: f32,
    /// Relief, rows from y = 0.
    rel: Vec<f32>,
    /// The valley centerline's waves along x: (amplitude, wavenumber, phase).
    waves: [(f32, f32, f32); 2],
    /// Half the floor's width.
    half: f32,
    /// Indices of texels below the surface (by more than `EPS`).
    pits: Vec<u32>,
    /// Scratch: 1 within `NEAR` widths of the channel, 2 under it.
    near: Vec<u8>,
}

/// What's saved: the texels are quantized, deflated, and in base64.
#[derive(Serialize, Deserialize)]
pub struct Saved {
    seed: u64,
    width: f32,
    min: f32,
    heights: String,
}

impl Terrain {
    /// A valley across a `gw`×`gh` grid for a river `width` wide.
    pub fn new(gw: u32, gh: u32, width: f32, seed: u64) -> Self {
        let mut t = Self::blank(gw, gh, width, seed);
        let (nx, ny) = (t.tw, t.th);
        let (floor, hills) = (Noise::new(seed, 1), Noise::new(seed, 2));
        for ix in 0..nx {
            let x = (ix as f32 + 0.5) * t.cx;
            let yc = t.valley_y(x);
            for iy in 0..ny {
                let y = (iy as f32 + 0.5) * t.cy;
                let wall = wall(((y - yc).abs() - t.half).max(0.0), width);
                let f = FLOOR_NOISE * width * floor.fbm(x, y, 8.0 * width, 2, t.gw);
                let h = 1.0 + HILLS * (2.0 * hills.fbm(x, y, 12.0 * width, 3, t.gw) - 1.0);
                t.rel[iy * nx + ix] = f + wall * h;
            }
        }
        t
    }

    /// The grid and the valley's course, with a flat floor.
    fn blank(gw: u32, gh: u32, width: f32, seed: u64) -> Self {
        let (gwf, ghf) = (gw as f32, gh as f32);
        let mut texel = (width / 3.0).max(1.0);
        while gwf * ghf / (texel * texel) > MAX_TEXELS {
            texel *= 1.25;
        }
        let (tw, th) = (((gwf / texel).ceil() as usize).max(2), ((ghf / texel).ceil() as usize).max(2));
        let half = (0.5 * FLOOR_WIDTHS * width).min(0.5 * FLOOR_FRAC * ghf);
        let amp = (MEANDER_FRAC * ghf).min(0.5 * ghf - half - 4.0 * width).max(0.0);
        let mut rng = Rng::new(seed);
        let tau = std::f32::consts::TAU;
        let waves = [(0.6 * amp, tau / gwf, tau * rng.f32()), (0.4 * amp, 2.0 * tau / gwf, tau * rng.f32())];
        Terrain {
            gw: gwf,
            gh: ghf,
            seed,
            width,
            tw,
            th,
            cx: gwf / tw as f32,
            cy: ghf / th as f32,
            rel: vec![0.0; tw * th],
            waves,
            half,
            pits: Vec::new(),
            near: Vec::new(),
        }
    }

    /// A fresh valley for a resized grid or channel, from the same seed.
    pub fn resized(&self, gw: u32, gh: u32, width: f32) -> Self {
        Self::new(gw, gh, width, self.seed)
    }

    /// Where the valley's centerline crosses `x` (period `gw`).
    pub fn valley_y(&self, x: f32) -> f32 {
        0.5 * self.gh + self.waves.iter().map(|(a, k, p)| a * (k * x + p).sin()).sum::<f32>()
    }

    /// Half the width of the flat valley floor, in cells.
    #[allow(dead_code)] // for the lakes
    pub fn valley_half(&self) -> f32 {
        self.half
    }

    /// The floodplain level at `x`, which the river's water stands at.
    pub fn surface(&self, x: f32) -> f32 {
        -DOWNSTREAM * x
    }

    /// Height above `surface(x)` at a point in cells (bilinear). Below 0 is a
    /// pit.
    pub fn relief_at(&self, x: f32, y: f32) -> f32 {
        let (fx, fy) = (x / self.cx - 0.5, (y / self.cy - 0.5).clamp(0.0, (self.th - 1) as f32));
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as usize);
        let y1 = (y0 + 1).min(self.th - 1);
        let at = |x: i64, y: usize| self.rel[y * self.tw + x.rem_euclid(self.tw as i64) as usize];
        let bottom = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        let top = at(x0, y1) * (1.0 - tx) + at(x0 + 1, y1) * tx;
        bottom * (1.0 - ty) + top * ty
    }

    /// Height at a point in cells.
    #[allow(dead_code)] // for fire and lightning
    pub fn height_at(&self, x: f32, y: f32) -> f32 {
        self.surface(x) + self.relief_at(x, y)
    }

    #[allow(dead_code)] // for fire
    /// Slope (d height / dx, d height / dy) at a point in cells.
    pub fn gradient_at(&self, x: f32, y: f32) -> [f32; 2] {
        let (hx, hy) = (self.cx, self.cy);
        [
            (self.height_at(x + hx, y) - self.height_at(x - hx, y)) / (2.0 * hx),
            (self.height_at(x, y + hy) - self.height_at(x, y - hy)) / (2.0 * hy),
        ]
    }

    /// Texels below the surface, as (x, y) in cells at their centers.
    #[allow(dead_code)] // for the lakes
    pub fn pits(&self) -> impl Iterator<Item = [f32; 2]> + '_ {
        self.pits.iter().map(|&i| {
            let (ix, iy) = (i as usize % self.tw, i as usize / self.tw);
            [(ix as f32 + 0.5) * self.cx, (iy as f32 + 0.5) * self.cy]
        })
    }

    /// Lower the bed under a channel `width` wide along `pts` (parabolic
    /// across, `DEPTH` × width at the middle), where it's higher.
    pub fn carve(&mut self, pts: &[[f32; 2]], width: f32) {
        let hw = 0.5 * width;
        let depth = DEPTH * width;
        for p in pts {
            let (x0, x1) = (((p[0] - hw) / self.cx - 0.5).ceil() as i64, ((p[0] + hw) / self.cx - 0.5).floor() as i64);
            let y0 = (((p[1] - hw) / self.cy - 0.5).ceil() as i64).max(0);
            let y1 = (((p[1] + hw) / self.cy - 0.5).floor() as i64).min(self.th as i64 - 1);
            for iy in y0..=y1 {
                let dy = (iy as f32 + 0.5) * self.cy - p[1];
                for ix in x0..=x1 {
                    let dx = (ix as f32 + 0.5) * self.cx - p[0];
                    let d2 = (dx * dx + dy * dy) / (hw * hw);
                    if d2 >= 1.0 {
                        continue;
                    }
                    let i = iy as usize * self.tw + ix.rem_euclid(self.tw as i64) as usize;
                    let target = -depth * (1.0 - d2);
                    if self.rel[i] > target {
                        if self.rel[i] >= -EPS && target < -EPS {
                            self.pits.push(i as u32);
                        }
                        self.rel[i] = target;
                    }
                }
            }
        }
    }

    /// Run the ground on by `dt` river-clock steps with the channel along
    /// `nodes`: pits away from it fill and round out, then it carves its bed.
    pub fn update(&mut self, nodes: &[[f32; 2]], width: f32, dt: f64) {
        self.mark(nodes, width);
        self.settle(width, dt as f32);
        self.carve(nodes, width);
    }

    /// Fill `near`: 2 under the channel, 1 close to it.
    fn mark(&mut self, nodes: &[[f32; 2]], width: f32) {
        self.near.clear();
        self.near.resize(self.rel.len(), 0);
        for (r, v, step) in [(NEAR * width, 1, 2), (0.5 * width, 2, 1)] {
            for p in nodes.iter().step_by(step) {
                let (x0, x1) = (((p[0] - r) / self.cx - 0.5).ceil() as i64, ((p[0] + r) / self.cx - 0.5).floor() as i64);
                let y0 = (((p[1] - r) / self.cy - 0.5).ceil() as i64).max(0);
                let y1 = (((p[1] + r) / self.cy - 0.5).floor() as i64).min(self.th as i64 - 1);
                for iy in y0..=y1 {
                    let dy = (iy as f32 + 0.5) * self.cy - p[1];
                    for ix in x0..=x1 {
                        let dx = (ix as f32 + 0.5) * self.cx - p[0];
                        if dx * dx + dy * dy < r * r {
                            self.near[iy as usize * self.tw + ix.rem_euclid(self.tw as i64) as usize] = v;
                        }
                    }
                }
            }
        }
    }

    /// Fill and smooth the pits not under the channel.
    fn settle(&mut self, width: f32, dt: f32) {
        let noise = Noise::new(self.seed, 3);
        let mix = (dt / SMOOTH_TIME).min(0.25);
        let mut k = 0;
        while k < self.pits.len() {
            let i = self.pits[k] as usize;
            if self.near[i] == 2 {
                k += 1;
                continue;
            }
            let (ix, iy) = (i % self.tw, i / self.tw);
            // Slightly uneven filling, so shapes aren't uniform.
            let (x, y) = ((ix as f32 + 0.5) * self.cx, (iy as f32 + 0.5) * self.cy);
            let rate = if self.near[i] == 1 { NEAR_FILL } else { SILT_FILL } * width * (0.4 + 1.2 * noise.fbm(x, y, 2.0 * width, 1, self.gw));
            let at = |dx: i64, dy: i64| {
                let (jx, jy) = ((ix as i64 + dx).rem_euclid(self.tw as i64) as usize, (iy as i64 + dy).clamp(0, self.th as i64 - 1) as usize);
                self.rel[jy * self.tw + jx]
            };
            let around = 0.25 * (at(-1, 0) + at(1, 0) + at(0, -1) + at(0, 1));
            let h = self.rel[i] + rate * dt;
            let h = h + mix * (around - h);
            if h >= -EPS {
                self.rel[i] = h.max(0.0);
                self.pits.swap_remove(k);
            } else {
                self.rel[i] = h;
                k += 1;
            }
        }
    }

    pub fn save(&self) -> Saved {
        let min = (self.rel.iter().fold(f32::INFINITY, |m, &h| m.min(h)) * QUANT).floor() / QUANT;
        let mut prev = 0u16;
        let mut bytes = Vec::with_capacity(self.rel.len() * 2);
        for &h in &self.rel {
            let q = ((h - min) * QUANT).round().clamp(0.0, 65535.0) as u16;
            // Differences along the row compress far better than heights.
            bytes.extend_from_slice(&q.wrapping_sub(prev).to_le_bytes());
            prev = q;
        }
        let deflated = miniz_oxide::deflate::compress_to_vec(&bytes, 6);
        Saved { seed: self.seed, width: self.width, min, heights: base64(&deflated) }
    }

    /// Restore a saved terrain for a `gw`×`gh` grid.
    pub fn restore(s: &Saved, gw: u32, gh: u32) -> Option<Self> {
        let mut t = Self::blank(gw, gh, s.width, s.seed);
        let bytes = miniz_oxide::inflate::decompress_to_vec_with_limit(&unbase64(&s.heights)?, t.rel.len() * 2).ok()?;
        if bytes.len() != t.rel.len() * 2 {
            return None;
        }
        let mut prev = 0u16;
        for (h, b) in t.rel.iter_mut().zip(bytes.chunks_exact(2)) {
            prev = prev.wrapping_add(u16::from_le_bytes([b[0], b[1]]));
            *h = s.min + prev as f32 / QUANT;
        }
        t.pits = (0..t.rel.len() as u32).filter(|&i| t.rel[i as usize] < -EPS).collect();
        Some(t)
    }

    /// The relief texels for the GPU: texels across and up, and heights with
    /// rows from y = 0. Texel (i, j) is centered at ((i + ½) × gw / tw, ...),
    /// wrapping in x, as `relief_at` reads it.
    pub fn relief_texels(&self) -> (u32, u32, &[f32]) {
        (self.tw as u32, self.th as u32, &self.rel)
    }
}

/// Height of the valley wall `t` cells past the floor's edge: curving up
/// from the floor, then `WALL` steep.
fn wall(t: f32, width: f32) -> f32 {
    WALL * t * t / (t + width)
}

/// Value noise that repeats across the grid's width.
struct Noise(u64);

impl Noise {
    fn new(seed: u64, which: u64) -> Self {
        Noise(seed ^ which.wrapping_mul(0x9e3779b97f4a7c15))
    }

    /// A lattice value in [0, 1).
    fn lattice(&self, ix: i64, iy: i64, octave: u64) -> f32 {
        let mut z = self.0 ^ (ix as u64).wrapping_mul(0xbf58476d1ce4e5b9) ^ (iy as u64).wrapping_mul(0x94d049bb133111eb) ^ octave.wrapping_mul(0xd6e8feb86659fd93);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Smooth noise in [0, 1) with features about `len` cells across, whole
    /// lattice cells to the width `period`.
    fn at(&self, x: f32, y: f32, len: f32, period: f32, octave: u64) -> f32 {
        let nx = (period / len).round().max(1.0);
        let (u, v) = (x * nx / period, y / len);
        let (u0, v0) = (u.floor(), v.floor());
        let (tu, tv) = (u - u0, v - v0);
        let (tu, tv) = (tu * tu * (3.0 - 2.0 * tu), tv * tv * (3.0 - 2.0 * tv));
        let (i0, j0) = (u0 as i64, v0 as i64);
        let at = |i: i64, j: i64| self.lattice(i.rem_euclid(nx as i64), j, octave);
        let a = at(i0, j0) * (1.0 - tu) + at(i0 + 1, j0) * tu;
        let b = at(i0, j0 + 1) * (1.0 - tu) + at(i0 + 1, j0 + 1) * tu;
        a * (1.0 - tv) + b * tv
    }

    /// Sum of `octaves` octaves, each half the size and amplitude of the
    /// last, in [0, 1).
    fn fbm(&self, x: f32, y: f32, len: f32, octaves: u64, period: f32) -> f32 {
        let (mut sum, mut amp, mut norm) = (0.0, 1.0, 0.0);
        for o in 0..octaves {
            sum += amp * self.at(x, y, len / (1u64 << o) as f32, period, o);
            norm += amp;
            amp *= 0.5;
        }
        sum / norm
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &v)| n | (v as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

fn unbase64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.as_bytes().chunks(4) {
        let mut n = 0;
        for (i, &c) in chunk.iter().enumerate() {
            n |= (B64.iter().position(|&d| d == c)? as u32) << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

/// How the terrain affects the fire (the shaders get these from shaders.rs).
///
/// Relief (cells) at which ground counts as high: moisture, lightning and
/// the like ease in over this height.
pub const RELIEF_SCALE: f32 = 4.0;
/// Spread onto a tree downhill of the fire is slowed: its chance is
/// 1 - `SLOPE_K` × strength × slope, but not below `SLOPE_MIN`. A valley wall
/// (slope up to 0.15) then keeps a quarter to a half of the spread.
pub const SLOPE_K: f32 = 4.0;
pub const SLOPE_MIN: f32 = 0.1;
/// Fire reaches a tree two cells uphill with chance `JUMP_K` × strength ×
/// (slope - `JUMP_DEADZONE`), at most `JUMP_MAX` a step; gentle slopes
/// (floor undulations) don't count.
pub const JUMP_K: f32 = 3.0;
pub const JUMP_DEADZONE: f32 = 0.04;
pub const JUMP_MAX: f32 = 0.5;
/// Growth is multiplied by `MOIST_WET` on the floor, easing to `MOIST_DRY`
/// at `RELIEF_SCALE` above it (both pulled toward 1 or past it by strength).
pub const MOIST_WET: f32 = 1.2;
pub const MOIST_DRY: f32 = 0.7;
/// Lightning's pull toward high ground: the weight of a strike on the floor,
/// rising to 1 at `RELIEF_SCALE` (scaled out by strength, as 1 - strength ×
/// (1 - weight)).
pub const STRIKE_FLOOR: f32 = 0.3;
/// The mean strike weight never counts as less than this.
pub const MIN_STRIKE_MEAN: f32 = 0.05;

/// How likely a lightning strike at ground `relief` cells high is to be kept,
/// for terrain `strength`. The shader's `strike_weight` does the same.
pub fn strike_weight(relief: f32, strength: f32) -> f32 {
    let t = (relief / RELIEF_SCALE).clamp(0.0, 1.0);
    let w = STRIKE_FLOOR + (1.0 - STRIKE_FLOOR) * t * t * (3.0 - 2.0 * t);
    (1.0 - strength * (1.0 - w)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_in_x() {
        let t = Terrain::new(200, 120, 4.0, 9);
        for y in [5.0, 40.5, 77.0, 110.0] {
            // The same ground one grid over, once the tilt's taken out.
            for x in [0.0, 13.3, 100.0, 199.0] {
                assert!((t.relief_at(x, y) - t.relief_at(x + 200.0, y)).abs() < 1e-4);
            }
            assert!((t.relief_at(-0.1, y) - t.relief_at(199.9, y)).abs() < 0.05);
        }
        assert!((t.valley_y(0.0) - t.valley_y(200.0)).abs() < 1e-3);
    }

    #[test]
    fn valley_holds_a_floor_between_walls() {
        let (gw, gh, w) = (400, 400, 6.0);
        let t = Terrain::new(gw, gh, w, 4);
        assert!(t.valley_half() > 10.0 * w);
        for x in (0..gw).step_by(7) {
            let (x, yc) = (x as f32, t.valley_y(x as f32));
            assert!(yc - t.valley_half() > 0.0 && yc + t.valley_half() < gh as f32);
            // Floor: level, never below the surface, and rises past the edge.
            for d in [0.0, 0.5 * t.valley_half(), 0.95 * t.valley_half()] {
                let r = t.relief_at(x, yc + d);
                assert!((-1e-4..FLOOR_NOISE * w + 1e-3).contains(&r), "floor relief {r}");
            }
            assert!(t.relief_at(x, yc + t.valley_half() + 6.0 * w) > FLOOR_NOISE * w);
        }
        let g = t.gradient_at(100.0, t.valley_y(100.0) + t.valley_half() + 20.0 * w);
        assert!(g[1] > 0.02, "wall slope {}", g[1]);
    }

    #[test]
    fn carves_fills_and_rounds_out() {
        let mut t = Terrain::new(200, 120, 6.0, 2);
        let y = t.valley_y(100.0);
        let line: Vec<[f32; 2]> = (0..40).map(|i| [60.0 + i as f32 * 1.8, y]).collect();
        t.update(&line, 6.0, 100.0);
        let bed = t.relief_at(100.0, y);
        assert!(bed < -0.8 * DEPTH * 6.0, "bed {bed}");
        assert!(t.pits().count() > 20);
        // Moved away, it fills in near the old line, then silts more slowly.
        let away: Vec<[f32; 2]> = line.iter().map(|p| [p[0], y + 40.0]).collect();
        t.update(&away, 6.0, 20.0);
        t.update(&away, 6.0, 20.0);
        let now = t.relief_at(100.0, y);
        assert!(now > bed, "{now} vs {bed}");
        for _ in 0..40 {
            t.update(&away, 6.0, 1000.0);
        }
        assert_eq!(t.pits().filter(|p| (p[1] - y).abs() < 6.0).count(), 0);
    }

    #[test]
    fn saves_compactly() {
        let mut t = Terrain::new(640, 360, 2.0, 5);
        let y = t.valley_y(300.0);
        let line: Vec<[f32; 2]> = (0..300).map(|i| [100.0 + i as f32, y]).collect();
        t.update(&line, 2.0, 100.0);
        let s = t.save();
        let json = serde_json::to_vec(&s).unwrap();
        assert!(json.len() < 250_000, "{} bytes", json.len());
        let r = Terrain::restore(&serde_json::from_slice(&json).unwrap(), 640, 360).unwrap();
        assert_eq!(r.pits.len(), t.pits.len());
        for (a, b) in r.rel.iter().zip(&t.rel) {
            assert!((a - b).abs() <= 0.5 / QUANT + 1e-6);
        }
        assert!(Terrain::restore(&s, 640, 361).is_none());
    }

    #[test]
    fn base64_round_trip() {
        for n in 0..7 {
            let b: Vec<u8> = (0..n).map(|i| (i * 91 + 7) as u8).collect();
            assert_eq!(unbase64(&base64(&b)).unwrap(), b);
        }
    }
}
