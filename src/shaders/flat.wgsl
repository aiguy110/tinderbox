// Flat renderer: one color per state, with a few transition accents.

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  grid_size = vec2<i32>(textureDimensions(state));
  let p = world(pos);
  let cell = vec2<i32>(floor(p));
  let t = cell_at(cell);
  let s = t.r;
  let cause = t.b;
  let jitter = f32(hash3(vec2<u32>(cell), 0x27d4eb2du) & 255u) / 255.0;

  var c: vec3<f32>;
  if (s == TREE) {
    c = mix(vec3<f32>(0.04, 0.20, 0.09), vec3<f32>(0.08, 0.30, 0.12), jitter);
    if (cause == CAUSE_GROW) { c += vec3<f32>(0.08, 0.10, 0.02); }
  } else if (s == FIRE) {
    c = mix(vec3<f32>(1.0, 0.35, 0.05), vec3<f32>(1.0, 0.75, 0.2), jitter);
    if (cause == CAUSE_LIGHTNING) { c = vec3<f32>(0.85, 0.92, 1.0); }
    if (cause == CAUSE_BURNING) { c *= 0.7; }
  } else if (s == WATER) {
    c = mix(vec3<f32>(0.08, 0.22, 0.32), vec3<f32>(0.10, 0.25, 0.35), jitter);
  } else if (s == BURNT) {
    c = mix(vec3<f32>(0.07, 0.06, 0.05), vec3<f32>(0.11, 0.09, 0.08), jitter);
  } else {
    let dirt = mix(vec3<f32>(0.20, 0.14, 0.09), vec3<f32>(0.24, 0.17, 0.11), jitter);
    let grass = mix(vec3<f32>(0.30, 0.52, 0.16), vec3<f32>(0.36, 0.58, 0.20), jitter);
    c = mix(dirt, grass, regrowth(t, 1.0));
    c = mix(c, vec3<f32>(0.62, 0.55, 0.40), river_at(vec2<f32>(cell) + 0.5).a);
  }

  c *= grid_line(p, view.scale.x / view.cell_px);
  return vec4<f32>(c, 1.0);
}
