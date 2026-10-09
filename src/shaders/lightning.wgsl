// Lightning bolts. Segments are drawn as glowing capsules into an offscreen
// target with MAX blending, so joints and overlapping glow don't double up,
// and that target is added onto the scene along with a faint sky flash.
// Bolt geometry is in global physical pixels (y up), shared by every view.

struct Bolts {
  // Global physical pixels of the target's bottom-left corner, and its size.
  origin: vec2<f32>,
  size: vec2<f32>,
  sky: f32,
  _pad0: f32,
  _pad1: f32,
  _pad2: f32,
}

@group(0) @binding(0) var<uniform> b: Bolts;

struct SegOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) world: vec2<f32>,
  @location(1) @interpolate(flat) seg: vec4<f32>,
  @location(2) @interpolate(flat) style: vec2<f32>,
}

@vertex
fn vs_seg(@location(0) at: vec2<f32>, @location(1) seg: vec4<f32>, @location(2) style: vec2<f32>) -> SegOut {
  var o: SegOut;
  o.pos = vec4<f32>((at - b.origin) / b.size * 2.0 - 1.0, 0.0, 1.0);
  o.world = at;
  o.seg = seg;
  o.style = style;
  return o;
}

// Distance to the segment gives a white-hot core and a blue glow.
@fragment
fn fs_seg(i: SegOut) -> @location(0) vec4<f32> {
  let a = i.seg.xy;
  let ba = i.seg.zw - a;
  let pa = i.world - a;
  let d = length(pa - ba * clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0));
  let w = i.style.x;
  let k = i.style.y;
  let core = exp(-pow(d / w, 2.0));
  let glow = exp(-d / (w * 4.0));
  let c = core * mix(vec3<f32>(0.55, 0.65, 1.0), vec3<f32>(1.0), clamp(k, 0.0, 1.0)) + glow * 0.4 * vec3<f32>(0.45, 0.55, 1.0);
  return vec4<f32>(c * min(k, 1.0), 1.0);
}

@group(1) @binding(0) var bolts: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  return fullscreen(i);
}

@fragment
fn fs_composite(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let c = textureLoad(bolts, vec2<i32>(pos.xy), 0).rgb;
  return vec4<f32>(c + b.sky * vec3<f32>(0.5, 0.55, 0.7), 1.0);
}
