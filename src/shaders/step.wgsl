// The Drossel–Schwabl update rule, one fragment per cell, plus a probe pass
// that reports which of the CPU-picked lightning strikes lit a tree. The
// river field (river.rs) decides which cells are water, makes trees grow
// faster on its banks, and lets embers jump it now and then. The terrain
// (the relief texture) slows fire spreading downhill and lets it reach uphill,
// dries the high ground and draws lightning to it; u.terrain is its strength.

struct Step {
  seed: u32,
  growth: f32,
  lightning: f32,
  moore: u32,
  wrap_edges: u32,
  strike_count: u32,
  // Chance per step that a bank tree catches from fire across the river.
  spotting: f32,
  // Channel width in cells.
  river_w: f32,
  // How much the terrain matters (0 = not at all), and the mean strike weight.
  terrain: f32,
  strike_mean: f32,
  // .xy of each; vec4 because uniform arrays have a 16-byte stride.
  strikes: array<vec4<i32>, MAX_STRIKES>,
}

@group(0) @binding(0) var state: texture_2d<u32>;
@group(1) @binding(0) var<uniform> u: Step;
@group(2) @binding(0) var field: texture_2d<f32>;
@group(2) @binding(2) var relief: texture_2d<f32>;

var<private> grid_size: vec2<i32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  return fullscreen(i);
}

fn struck(p: vec2<i32>) -> bool {
  for (var i = 0u; i < u.strike_count; i++) {
    if (all(u.strikes[i].xy == p)) { return true; }
  }
  return false;
}

fn rand(p: vec2<i32>, salt: u32) -> f32 {
  return f32(hash3(vec2<u32>(p), u.seed ^ salt)) / 4294967296.0;
}

fn burning(p0: vec2<i32>) -> bool {
  var p = p0;
  if (u.wrap_edges != 0u) {
    p = (p + grid_size) % grid_size;
  } else if (any(p < vec2<i32>(0)) || any(p >= grid_size)) {
    return false;
  }
  return textureLoad(state, p, 0).r == FIRE;
}

fn wrapped(p: vec2<i32>) -> vec2<i32> {
  if (u.wrap_edges != 0u) { return (p + grid_size) % grid_size; }
  return p;
}

// The river field texel at a cell's center.
fn field_at(p: vec2<i32>) -> vec4<f32> {
  let fd = vec2<i32>(textureDimensions(field));
  let t = vec2<i32>((vec2<f32>(wrapped(p)) + 0.5) * vec2<f32>(fd) / vec2<f32>(grid_size));
  return textureLoad(field, clamp(t, vec2<i32>(0), fd - 1), 0);
}

// Relief (cells above the floodplain) at a point in cells, bilinear between
// texel centers like Terrain::relief_at; wraps in x, clamps in y.
fn relief_at(c: vec2<f32>) -> f32 {
  let rd = vec2<i32>(textureDimensions(relief));
  let f = c / vec2<f32>(grid_size) * vec2<f32>(rd) - 0.5;
  let f0 = floor(f);
  let t = f - f0;
  let i0 = vec2<i32>(f0);
  let x0 = (i0.x % rd.x + rd.x) % rd.x;
  let x1 = (x0 + 1) % rd.x;
  let y0 = clamp(i0.y, 0, rd.y - 1);
  let y1 = clamp(i0.y + 1, 0, rd.y - 1);
  let bottom = mix(textureLoad(relief, vec2<i32>(x0, y0), 0).r, textureLoad(relief, vec2<i32>(x1, y0), 0).r, t.x);
  let top = mix(textureLoad(relief, vec2<i32>(x0, y1), 0).r, textureLoad(relief, vec2<i32>(x1, y1), 0).r, t.x);
  return mix(bottom, top, t.y);
}

// Relief at a cell's center.
fn relief_cell(p: vec2<i32>) -> f32 {
  return relief_at(vec2<f32>(p) + 0.5);
}

// Lightning's weight at a relief: STRIKE_FLOOR on the floor, 1 on high ground,
// pulled toward 1 (everywhere) as the strength drops. Matches terrain.rs.
fn strike_weight(rel: f32) -> f32 {
  let w = STRIKE_FLOOR + (1.0 - STRIKE_FLOOR) * smoothstep(0.0, RELIEF_SCALE, rel);
  return clamp(1.0 - u.terrain * (1.0 - w), 0.0, 1.0);
}

// Growth's moisture factor: wet on the floor, dry on high ground.
fn moisture(rel: f32) -> f32 {
  return 1.0 + u.terrain * (mix(MOIST_WET, MOIST_DRY, smoothstep(0.0, RELIEF_SCALE, rel)) - 1.0);
}

// Embers carried across the river: a tree on the bank may catch from a
// burning cell straight across the water.
fn spotted(p: vec2<i32>, f: vec4<f32>) -> bool {
  let l = length(f.gb);
  // Rivers only (lakes have weak flow), and only trees near the bank.
  if (u.spotting <= 0.0 || l < 0.5 || f.r > 1.5) { return false; }
  let t = f.gb / l;
  var n = vec2<f32>(-t.y, t.x);
  if (field_at(p + vec2<i32>(round(n * 2.0))).r > f.r) { n = -n; }
  let q = p + vec2<i32>(round(n * (u.river_w + 1.5 + f.r)));
  let side = vec2<i32>(round(t));
  return (burning(q) || burning(q + side) || burning(q - side)) && rand(p, 0x5bd1e995u) < u.spotting;
}

@fragment
fn fs_step(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<u32> {
  grid_size = vec2<i32>(textureDimensions(state));
  let p = vec2<i32>(pos.xy);
  let cur = textureLoad(state, p, 0);
  let s = cur.r;
  let f = field_at(p);
  if (f.r < 0.0) {
    if (s == WATER) { return vec4<u32>(WATER, WATER, CAUSE_NONE, SETTLED); }
    return vec4<u32>(WATER, s, CAUSE_ERODE, SETTLED);
  }
  if (s == WATER) { return vec4<u32>(EMPTY, WATER, CAUSE_DEPOSIT, SETTLED); }
  var next = s;
  var cause = CAUSE_NONE;
  var a = 0u;
  // Zero (and not read from the texture) when the terrain is off.
  var rel = 0.0;
  if (u.terrain > 0.0) { rel = relief_cell(p); }

  if (s == FIRE && cur.g == TREE) {
    // Caught last step: burn for a step, keeping where the fire came from.
    cause = CAUSE_BURNING;
    a = cur.a;
  } else if (s == FIRE) {
    next = BURNT;
    cause = CAUSE_BURNOUT;
  } else if (s == BURNT) {
    next = EMPTY;
    cause = CAUSE_CLEAR;
  } else if (s == TREE) {
    let n = select(4, 8, u.moore != 0u);
    var neighbors = NEIGHBORS;
    // A burning neighbor uphill of this tree spreads with less than certainty.
    for (var i = 0; i < n; i++) {
      let q = p + neighbors[i];
      if (!burning(q)) { continue; }
      var chance = 1.0;
      if (u.terrain > 0.0) {
        let slope = (rel - relief_cell(q)) / length(vec2<f32>(neighbors[i]));
        chance = clamp(1.0 + SLOPE_K * u.terrain * slope, SLOPE_MIN, 1.0);
      }
      if (chance >= 1.0 || rand(p, 0x27d4eb2fu + u32(i) * 0x9e3779b1u) < chance) { a |= 1u << u32(i); }
    }
    // Fire reaches a tree two cells uphill of it.
    var jumped = false;
    if (a == 0u && u.terrain > 0.0) {
      for (var i = 0; i < n; i++) {
        let q = p - 2 * neighbors[i];
        if (!burning(q)) { continue; }
        let slope = (rel - relief_cell(q)) / (2.0 * length(vec2<f32>(neighbors[i])));
        let chance = min(JUMP_K * u.terrain * max(slope - JUMP_DEADZONE, 0.0), JUMP_MAX);
        if (rand(p, 0x165667b1u + u32(i) * 0x9e3779b1u) < chance) { jumped = true; }
      }
    }
    var strike = u.lightning;
    if (u.terrain > 0.0) { strike *= strike_weight(rel) / u.strike_mean; }
    if (a != 0u) {
      next = FIRE;
      cause = CAUSE_SPREAD;
    } else if (struck(p) || rand(p, 0x9e3779b9u) < strike) {
      next = FIRE;
      cause = CAUSE_LIGHTNING;
    } else if (jumped || spotted(p, f)) {
      next = FIRE;
      cause = CAUSE_SPOT;
    } else if (cur.b == CAUSE_GROW) {
      // A second step of growing, still keeping the ground's age.
      cause = CAUSE_MATURE;
      a = cur.a;
    }
  } else if (rand(p, 0x85ebca6bu) < u.growth * moisture(rel) * (1.0 + RIPARIAN * (1.0 - smoothstep(0.0, max(u.river_w, 1.0), f.r)))) {
    // A sapling keeps the age of the ground it grows from, to draw it.
    next = TREE;
    cause = CAUSE_GROW;
    a = cur.a;
  } else {
    a = select(min(cur.a + 1u, SETTLED - 1u), SETTLED, cur.a == SETTLED);
  }

  return vec4<u32>(next, s, cause, a);
}

// Drawn into a MAX_STRIKES×1 target after a step, reading the new state:
// white where strike i set a tree alight, black otherwise.
@fragment
fn fs_probe(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let i = u32(pos.x);
  if (i >= u.strike_count) { return vec4<f32>(0.0); }
  let t = textureLoad(state, u.strikes[i].xy, 0);
  return select(vec4<f32>(0.0), vec4<f32>(1.0), t.r == FIRE && t.b == CAUSE_LIGHTNING);
}
