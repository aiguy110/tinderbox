//! Lakes: water standing in hollows of the terrain.
//!
//! A lake is a water level and a few seeds. Its *region* is the set of
//! texels reachable from the seeds that lie below the level and below the
//! surface (so a lake never spreads over the flat floor), excluding the
//! channel's own bed, which is drawn as the channel. Each update:
//!
//! - the level drops by evaporation, or is held at the river's surface while
//!   a lake touches the channel (a backwater), and is topped up in a flood;
//! - the region is found again from the old region's texels, so a lake that
//!   silting has split in two becomes two ponds, and one the channel has run
//!   into is reabsorbed;
//! - lakes whose regions meet are merged, and a lake with no water is dropped;
//! - texels the water has left are hardened into clay (see `Terrain`).
//!
//! The field is drawn from the height map: signed distance to the shore is
//! (height - level) / slope, in cells, from the bilinear height.

use serde::{Deserialize, Serialize};

use crate::river::{LAKE_LIFE, smoothstep};
use crate::terrain::{DEPTH, DOWNSTREAM, EPS, Terrain};

/// The level falls this many widths per river-clock step. With the bed
/// silting up as well, an oxbow lasts about 8 000 steps.
const EVAP: f32 = 0.4 * DEPTH / LAKE_LIFE as f32;
/// Water in fewer texels than this isn't a pond.
const MIN_POND: usize = 4;
/// Seeds a new lake gets along its length.
const SEEDS: usize = 5;
/// Flow of a deep lake (see the field's gb); it fades to 0 as the water
/// shallows, which makes the scene paint it murky, green and reedy.
const FLOW: f32 = 0.25;
/// Water deeper than this fraction of the bed's original depth counts as deep.
const DEEP: f32 = 0.5;
/// The slope below which the shore's distance is taken as if it were this
/// (cells per cell), so a flat bed doesn't make every distance huge.
const GRAD_MIN: f32 = 0.1;
/// A flood tops lakes near the channel up to this many widths above the
/// river's surface.
const FLOOD_TOP: f32 = 0.1;
/// The redraw accumulator gains 1 for this fraction of the bed depth the
/// level falls, and for each texel gained or lost.
const LEVEL_UNIT: f32 = 0.03;
/// Distance reported for a texel just outside the water in the field, when
/// the height says water but no lake region is near (a puddle we don't keep).
const DRY: f32 = 0.25;

#[derive(Clone, Serialize, Deserialize)]
pub struct Lake {
    /// One texel in each pond (the deepest), as points in cells.
    seeds: Vec<[f32; 2]>,
    level: f32,
    /// Texel indices under water as of the last update.
    #[serde(skip)]
    region: Vec<u32>,
    /// The region touches the channel.
    #[serde(skip)]
    connected: bool,
    /// The region lies within reach of the channel (for floods).
    #[serde(skip)]
    near: bool,
}

/// The raster the lakes are drawn into (see `River::raster`).
pub struct Field<'a> {
    pub w: u32,
    pub h: u32,
    pub sx: f32,
    pub sy: f32,
    pub dist: &'a mut [f32],
    pub flow: &'a mut [[f32; 2]],
}

#[derive(Default)]
pub struct Lakes {
    list: Vec<Lake>,
    /// Scratch: the lake (index + 1) that holds each texel.
    owner: Vec<u16>,
    /// Scratch for drawing: the last pass each terrain cell was listed in.
    seen: Vec<u32>,
    pass: u32,
    /// How much has changed since the last draw (see `LEVEL_UNIT`).
    change: f32,
}

/// Whether texel `i` is a hollow that water at `level` fills.
fn holds(t: &Terrain, level: f32, i: usize) -> bool {
    t.relief_of(i) < -EPS && t.height_of(i) < level && !t.under_channel(i)
}

impl Lakes {
    pub fn from_saved(list: Vec<Lake>) -> Self {
        Lakes { list, ..Default::default() }
    }

    pub fn saved(&self) -> Vec<Lake> {
        self.list.clone()
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether enough has changed to be worth drawing again.
    pub fn changed(&self) -> bool {
        self.change >= 1.0
    }

    /// Water in the trench `pts` that an abandoned loop left, level with the
    /// river's surface.
    pub fn add(&mut self, t: &Terrain, pts: &[[f32; 2]]) {
        let seeds = (0..SEEDS).map(|k| pts[(2 * k + 1) * pts.len() / (2 * SEEDS)]).collect();
        self.list.push(Lake { seeds, level: t.surface(pts[pts.len() / 2][0]), region: Vec::new(), connected: false, near: false });
        self.change += 1.0;
    }

    /// Run the lakes on by `dt` river-clock steps, `flood` (0 to 1) deep in
    /// a flood, for a channel `width` wide.
    pub fn update(&mut self, t: &mut Terrain, width: f32, dt: f32, flood: f32) {
        if self.list.is_empty() {
            return;
        }
        let (tw, th) = t.dims();
        self.owner.resize(tw * th, 0);
        let before: Vec<u32> = self.list.iter().flat_map(|l| l.region.iter().copied()).collect();
        for l in &mut self.list {
            let top = t.surface(l.seeds[0][0]);
            if l.connected {
                l.level = top;
            } else {
                l.level -= EVAP * width * dt;
                self.change += EVAP * width * dt / (LEVEL_UNIT * DEPTH * width);
            }
            if l.near && flood > 0.0 {
                l.level = l.level.max(top + FLOOD_TOP * width * flood);
            }
        }
        self.regrow(t);
        t.set_backwater(self.list.iter().filter(|l| l.connected).flat_map(|l| l.region.iter().map(|&i| i as usize)));
        let dried = before.iter().filter(|&&i| self.owner[i as usize] == 0 && !t.under_channel(i as usize)).count();
        for &i in &before {
            if self.owner[i as usize] == 0 && !t.under_channel(i as usize) {
                t.harden(i as usize);
            }
        }
        let now: usize = self.list.iter().map(|l| l.region.len()).sum();
        self.change += (dried + (now + dried).saturating_sub(before.len())) as f32;
    }

    /// Find every lake's region again, merging lakes that meet, and drop
    /// those left with no water.
    fn regrow(&mut self, t: &Terrain) {
        loop {
            self.owner.fill(0);
            let mut merge = None;
            let mut grown = Vec::with_capacity(self.list.len());
            for (k, l) in self.list.iter().enumerate() {
                let (region, seeds, hit) = grow(&mut self.owner, t, l, k as u16 + 1);
                if let Some(j) = hit {
                    merge = Some((j as usize - 1, k));
                    break;
                }
                grown.push((region, seeds));
            }
            if let Some((j, k)) = merge {
                let l = self.list.remove(k);
                let into = &mut self.list[j];
                into.seeds.extend(l.seeds);
                into.region.extend(l.region);
                into.level = into.level.max(l.level);
                into.connected |= l.connected;
                continue;
            }
            for (l, (region, seeds)) in self.list.iter_mut().zip(grown) {
                l.connected = region.iter().any(|&i| touches(t, i as usize));
                l.near = region.iter().any(|&i| t.near_channel(i as usize));
                (l.region, l.seeds) = (region, seeds);
            }
            // Dropping a lake renumbers the rest, so keep owners in step.
            if self.list.iter().any(|l| l.region.is_empty()) {
                self.list.retain(|l| !l.region.is_empty());
                continue;
            }
            return;
        }
    }

    /// Draw the lakes into `f` as signed distance to their shores (in
    /// cells, at most `far`) and a flow that fades with depth.
    pub fn stamp(&mut self, t: &Terrain, f: &mut Field, width: f32, far: f32) {
        let (tw, th) = t.dims();
        let (tw, th) = (tw as i64, th as i64);
        self.seen.resize((tw * (th + 1)) as usize, 0);
        let deep = DEEP * DEPTH * width;
        for (k, l) in self.list.iter().enumerate() {
            // The terrain cells (between four texel centers) around the
            // region, one ring out, so the shore's surroundings are drawn too.
            self.pass += 1;
            let mut cells = Vec::new();
            for &i in &l.region {
                let (ix, iy) = (i as i64 % tw, i as i64 / tw);
                for ky in (iy - 2).max(-1)..=(iy + 1).min(th - 1) {
                    for kx in ix - 2..=ix + 1 {
                        let key = ((ky + 1) * tw + kx.rem_euclid(tw)) as usize;
                        if self.seen[key] != self.pass {
                            self.seen[key] = self.pass;
                            cells.push((kx.rem_euclid(tw), ky));
                        }
                    }
                }
            }
            for (kx, ky) in cells {
                // The last column's cell is also the one left of the first.
                let images = if kx == tw - 1 { &[kx, -1][..] } else { &[kx][..] };
                for &kx in images {
                    stamp_cell(t, f, &self.owner, k as u16 + 1, l.level, (kx, ky), (deep, far));
                }
            }
        }
    }
}

/// Grow the region of lake `l` (owner id `id`) from its seeds and its old
/// region: the region, a seed for each pond, and the id of another lake
/// whose water it ran into, if any.
fn grow(owner: &mut [u16], t: &Terrain, l: &Lake, id: u16) -> (Vec<u32>, Vec<[f32; 2]>, Option<u16>) {
    let (mut region, mut seeds, mut hit) = (Vec::new(), Vec::new(), None);
    let cap = owner.len() / 6;
    let mut stack = Vec::new();
    let starts = l.seeds.iter().filter_map(|s| snap(t, l.level, *s)).chain(l.region.iter().map(|&i| i as usize));
    for s in starts {
        if owner[s] == id || !holds(t, l.level, s) {
            continue;
        }
        if owner[s] != 0 {
            hit = Some(owner[s]);
            continue;
        }
        let first = region.len();
        owner[s] = id;
        region.push(s as u32);
        stack.push(s);
        while let Some(i) = stack.pop() {
            for j in t.neighbors(i) {
                if owner[j] == id || !holds(t, l.level, j) {
                    continue;
                }
                if owner[j] != 0 {
                    hit = Some(owner[j]);
                    continue;
                }
                owner[j] = id;
                region.push(j as u32);
                stack.push(j);
            }
            if region.len() > cap {
                stack.clear();
            }
        }
        if region.len() - first < MIN_POND {
            region[first..].iter().for_each(|&i| owner[i as usize] = 0);
            region.truncate(first);
        } else {
            let deepest = region[first..].iter().map(|&i| i as usize).min_by(|&a, &b| t.relief_of(a).total_cmp(&t.relief_of(b))).unwrap();
            seeds.push(center(t, deepest));
        }
    }
    (region, seeds, hit)
}

/// Whether channel water is within a texel or two of texel `i`.
fn touches(t: &Terrain, i: usize) -> bool {
    t.near_channel(i) && t.neighbors(i).iter().any(|&j| t.under_channel(j) || t.neighbors(j).iter().any(|&k| t.under_channel(k)))
}

/// The lowest texel holding water at `level` at or beside the one at `p`.
fn snap(t: &Terrain, level: f32, p: [f32; 2]) -> Option<usize> {
    let i = t.index_at(p[0], p[1]);
    std::iter::once(i)
        .chain(t.neighbors(i))
        .filter(|&j| holds(t, level, j))
        .min_by(|&a, &b| t.relief_of(a).total_cmp(&t.relief_of(b)))
}

/// The center of texel `i`, in cells.
fn center(t: &Terrain, i: usize) -> [f32; 2] {
    let (tw, _) = t.dims();
    let (cx, cy) = t.texel();
    [((i % tw) as f32 + 0.5) * cx, ((i / tw) as f32 + 0.5) * cy]
}

/// Draw the field texels inside terrain cell `(kx, ky)` (whose corners are
/// the texels `kx..=kx+1`, `ky..=ky+1`; the cells at the top and bottom
/// edges are half cells): lake `id` at `level`.
fn stamp_cell(t: &Terrain, f: &mut Field, owner: &[u16], id: u16, level: f32, (kx, ky): (i64, i64), (deep, far): (f32, f32)) {
    let (tw, th) = t.dims();
    let (cx, cy) = t.texel();
    let (x0, x1) = (kx.rem_euclid(tw as i64) as usize, (kx + 1).rem_euclid(tw as i64) as usize);
    let (y0, y1) = (ky.clamp(0, th as i64 - 1) as usize, (ky + 1).clamp(0, th as i64 - 1) as usize);
    let [a, b, c, d] = [y0 * tw + x0, y0 * tw + x1, y1 * tw + x0, y1 * tw + x1];
    let (ra, rb, rc, rd) = (t.relief_of(a), t.relief_of(b), t.relief_of(c), t.relief_of(d));
    // Water can only show where a corner holds it: no isolated puddles.
    let kept = [a, b, c, d].iter().any(|&i| owner[i] == id);
    let (xlo, ylo) = ((kx as f32 + 0.5) * cx, (ky as f32 + 0.5) * cy);
    let span = |lo: f32, size: f32, scale: f32, n: u32| {
        let first = ((lo * scale - 0.5).ceil().max(0.0)) as u32;
        let last = (((lo + size) * scale - 0.5).ceil() as i64 - 1).min(n as i64 - 1);
        first..(last + 1).max(0) as u32
    };
    for ty in span(ylo, cy, f.sy, f.h) {
        let y = (ty as f32 + 0.5) / f.sy;
        let v = ((y - ylo) / cy).clamp(0.0, 1.0);
        for tx in span(xlo, cx, f.sx, f.w) {
            let x = (tx as f32 + 0.5) / f.sx;
            let u = ((x - xlo) / cx).clamp(0.0, 1.0);
            let rel = (ra * (1.0 - u) + rb * u) * (1.0 - v) + (rc * (1.0 - u) + rd * u) * v;
            let gx = ((rb - ra) * (1.0 - v) + (rd - rc) * v) / cx - DOWNSTREAM;
            let gy = ((rc - ra) * (1.0 - u) + (rd - rb) * u) / cy;
            let depth = level - (rel + t.surface(x));
            let mut e = -depth / gx.hypot(gy).max(GRAD_MIN);
            if e < 0.0 && !kept {
                e = DRY;
            }
            let i = (ty * f.w + tx) as usize;
            let e = e.clamp(-far, far);
            if e < f.dist[i] {
                f.dist[i] = e;
                f.flow[i] = [FLOW * smoothstep(0.0, deep, depth), 0.0];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terrain with a round pit at (60, 50) and no channel.
    fn pit() -> (Terrain, Vec<[f32; 2]>) {
        let mut t = Terrain::new(200, 120, 6.0, 2);
        let y = t.valley_y(60.0);
        let pts: Vec<[f32; 2]> = (0..30).map(|i| [48.0 + i as f32 * 0.8, y]).collect();
        t.carve(&pts, 6.0);
        (t, pts)
    }

    #[test]
    fn evaporates_and_dries() {
        let (mut t, pts) = pit();
        let mut lakes = Lakes::default();
        lakes.add(&t, &pts);
        lakes.update(&mut t, 6.0, 1.0, 0.0);
        assert_eq!(lakes.len(), 1);
        let first = lakes.list[0].region.len();
        assert!(first > 20, "{first}");
        let mut steps = 0.0;
        while lakes.len() > 0 {
            t.update(&[], 6.0, 20.0);
            lakes.update(&mut t, 6.0, 20.0, 0.0);
            steps += 20.0;
            assert!(steps < 3.0 * LAKE_LIFE as f32);
        }
        assert!(steps > 0.3 * LAKE_LIFE as f32, "{steps}");
        // The bed is clay now.
        assert!(t.hardness_at(60.0, t.valley_y(60.0)) > 0.5);
    }

    #[test]
    fn a_touched_lake_stays_full() {
        let (mut t, pts) = pit();
        let mut lakes = Lakes::default();
        lakes.add(&t, &pts);
        // The channel runs along one end.
        let end = pts[pts.len() - 1];
        let channel: Vec<[f32; 2]> = (0..20).map(|i| [end[0] + 2.0, end[1] - 20.0 + i as f32 * 2.0]).collect();
        t.update(&channel, 6.0, 1.0);
        lakes.update(&mut t, 6.0, 1.0, 0.0);
        let level = lakes.list[0].level;
        assert!(lakes.list[0].connected);
        // The channel migrates into the lake, reabsorbing its end.
        let mut at = channel.clone();
        for _ in 0..20 {
            at.iter_mut().for_each(|p| p[0] -= 0.5);
            t.update(&at, 6.0, 100.0);
            lakes.update(&mut t, 6.0, 100.0, 0.0);
        }
        assert!(lakes.list[0].connected && (lakes.list[0].level - level).abs() < 0.05, "{}", lakes.list[0].level - level);
        // It resumes drying once the channel leaves.
        let away: Vec<[f32; 2]> = at.iter().map(|p| [p[0], p[1] + 60.0]).collect();
        for _ in 0..30 {
            t.update(&away, 6.0, 100.0);
            lakes.update(&mut t, 6.0, 100.0, 0.0);
        }
        assert!(lakes.len() == 0 || lakes.list[0].level < level - 0.1);
    }
}
