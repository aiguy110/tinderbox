// Scales the scene (rendered at the adaptive resolution) up to the screen.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct BlitOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> BlitOut {
  var o: BlitOut;
  o.pos = fullscreen(i);
  o.uv = vec2<f32>(o.pos.x * 0.5 + 0.5, 0.5 - o.pos.y * 0.5);
  return o;
}

@fragment
fn fs(i: BlitOut) -> @location(0) vec4<f32> {
  return vec4<f32>(textureSample(src, smp, i.uv).rgb, 1.0);
}
