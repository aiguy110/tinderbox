// What the scene and flat renderers share: the view uniforms, which place
// this render target within the global layout (all monitors together), and
// cell lookup, noise and the fire front's timing.

struct View {
  // Global physical pixels (y up) of the target's bottom-left corner.
  origin: vec2<f32>,
  // Physical pixels per target pixel; above 1 when rendering at reduced
  // resolution.
  scale: vec2<f32>,
  target_h: f32,
  // Physical pixels per cell.
  cell_px: f32,
  // Steps for burnt ground to green back over.
  regrow: f32,
  // How far through the current step, 0 to 1. Drives transitions.
  progress: f32,
  // Seconds. Drives flicker.
  time: f32,
  grid_lines: u32,
  wrap_edges: u32,
  // Scene renderer: trees per cell side, already capped for small cells.
  trees: i32,
  night: u32,
  // Scene renderer: average steps a snag stands after its cell clears.
  snag: f32,
  // River half-width, in cells.
  river_hw: f32,
  _pad: u32,
}

@group(0) @binding(0) var state: texture_2d<u32>;
@group(1) @binding(0) var<uniform> view: View;
@group(2) @binding(0) var field: texture_2d<f32>;
@group(2) @binding(1) var field_sampler: sampler;

var<private> grid_size: vec2<i32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  return fullscreen(i);
}

// A fragment's position in cells across the whole layout (y up).
fn world(pos: vec4<f32>) -> vec2<f32> {
  return (view.origin + vec2<f32>(pos.x, view.target_h - pos.y) * view.scale) / view.cell_px;
}

fn cell_at(c0: vec2<i32>) -> vec4<u32> {
  var c = c0;
  if (view.wrap_edges != 0u) {
    c = ((c % grid_size) + grid_size) % grid_size;
  } else if (any(c < vec2<i32>(0)) || any(c >= grid_size)) {
    return vec4<u32>(EMPTY, EMPTY, CAUSE_NONE, SETTLED);
  }
  return textureLoad(state, c, 0);
}

// The river field at a point (see river.rs): r = signed distance to the
// water's edge in cells, gb = flow, a = fresh sand.
fn river_at(p: vec2<f32>) -> vec4<f32> {
  return textureSampleLevel(field, field_sampler, p / vec2<f32>(grid_size), 0.0);
}

// Steps since the ground was cleared by fire, at progress s through the step:
// 0 when the CLEAR step starts, continuous through a sapling's two growing
// steps. Large for ground that never burned and for grown forest.
fn ground_age(t: vec4<u32>, s: f32) -> f32 {
  if (t.a == SETTLED) { return 1e4; }
  if (t.r == EMPTY) { return f32(t.a) + s; }
  if (t.r == TREE && t.b == CAUSE_GROW) { return f32(t.a) + 1.0 + s; }
  if (t.r == TREE && t.b == CAUSE_MATURE) { return f32(t.a) + 2.0 + s; }
  return 1e4;
}

// How far the ground has greened back, 0 (bare dirt) to 1 (grass). Grass
// starts after the CLEAR step and takes view.regrow steps.
fn regrowth(t: vec4<u32>, s: f32) -> f32 {
  return clamp((ground_age(t, s) - 1.0) / view.regrow, 0.0, 1.0);
}

fn hashf(i: vec2<i32>, salt: u32) -> f32 {
  return f32(hash3(vec2<u32>(i), salt) & 0xffffffu) / 16777216.0;
}

fn vnoise(p: vec2<f32>, salt: u32) -> f32 {
  let i = vec2<i32>(floor(p));
  let f = fract(p);
  let u = f * f * (3.0 - 2.0 * f);
  return mix(mix(hashf(i, salt), hashf(i + vec2<i32>(1, 0), salt), u.x),
             mix(hashf(i + vec2<i32>(0, 1), salt), hashf(i + vec2<i32>(1, 1), salt), u.x), u.y);
}

fn fbm(p: vec2<f32>, salt: u32) -> f32 {
  return 0.5 * vnoise(p, salt) + 0.3 * vnoise(p * 2.03 + 17.1, salt) + 0.2 * vnoise(p * 4.07 + 31.7, salt);
}

// Grass coverage of ground that has regrown by r: it comes back in patches
// that spread and join.
fn grass_at(p: vec2<f32>, r: f32) -> f32 {
  if (r >= 1.0) { return 1.0; }
  if (r <= 0.0) { return 0.0; }
  let n = 0.75 * fbm(p * 2.2, 4u) + 0.25 * vnoise(p * 11.0, 16u);
  return smoothstep(n - 0.06, n + 0.06, r * 1.4 - 0.2);
}

// How far through the catching step (0 to 1) the fire front reaches a point
// q in the cell, coming from the burning neighbors in the sources mask, or
// spreading out from a random spot for lightning (an empty mask). The mask
// is kept through the burning step.
fn arrival(q: vec2<f32>, t: vec4<u32>, c: vec2<i32>) -> f32 {
  let m = t.a;
  if (m == 0u) {
    let o = 0.3 + 0.4 * vec2<f32>(hashf(c, 11u), hashf(c, 12u));
    return length(q - o) / 0.95;
  }
  var a = 2.0;
  if ((m & 1u) != 0u) { a = min(a, q.x); }
  if ((m & 2u) != 0u) { a = min(a, 1.0 - q.y); }
  if ((m & 4u) != 0u) { a = min(a, 1.0 - q.x); }
  if ((m & 8u) != 0u) { a = min(a, q.y); }
  if ((m & 16u) != 0u) { a = min(a, length(q) * 0.7071); }
  if ((m & 32u) != 0u) { a = min(a, length(q - vec2<f32>(0.0, 1.0)) * 0.7071); }
  if ((m & 64u) != 0u) { a = min(a, length(q - vec2<f32>(1.0)) * 0.7071); }
  if ((m & 128u) != 0u) { a = min(a, length(q - vec2<f32>(1.0, 0.0)) * 0.7071); }
  return a;
}

fn grid_line(p: vec2<f32>, px: f32) -> f32 {
  if (view.grid_lines == 0u) { return 1.0; }
  let g = fract(p);
  let edge = step(g.x, px) + step(g.y, px);
  return 1.0 - 0.35 * min(edge, 1.0);
}
