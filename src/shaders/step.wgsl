// The Drossel–Schwabl update rule, one fragment per cell, plus a probe pass
// that reports which of the CPU-picked lightning strikes lit a tree.

struct Step {
  seed: u32,
  growth: f32,
  lightning: f32,
  moore: u32,
  wrap_edges: u32,
  strike_count: u32,
  _pad0: u32,
  _pad1: u32,
  // .xy of each; vec4 because uniform arrays have a 16-byte stride.
  strikes: array<vec4<i32>, MAX_STRIKES>,
}

@group(0) @binding(0) var state: texture_2d<u32>;
@group(1) @binding(0) var<uniform> u: Step;

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

@fragment
fn fs_step(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<u32> {
  grid_size = vec2<i32>(textureDimensions(state));
  let p = vec2<i32>(pos.xy);
  let cur = textureLoad(state, p, 0);
  let s = cur.r;
  var next = s;
  var cause = CAUSE_NONE;
  var a = 0u;

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
    for (var i = 0; i < n; i++) {
      if (burning(p + neighbors[i])) { a |= 1u << u32(i); }
    }
    if (a != 0u) {
      next = FIRE;
      cause = CAUSE_SPREAD;
    } else if (struck(p) || rand(p, 0x9e3779b9u) < u.lightning) {
      next = FIRE;
      cause = CAUSE_LIGHTNING;
    } else if (cur.b == CAUSE_GROW) {
      // A second step of growing, still keeping the ground's age.
      cause = CAUSE_MATURE;
      a = cur.a;
    }
  } else if (rand(p, 0x85ebca6bu) < u.growth) {
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
