// Cell state lives in an Rgba8Uint texture, one texel per cell:
//   r = current state, g = previous state, b = cause of the last transition,
//   a = for FIRE, a bitmask of the neighbors that were burning when it caught
//       (see Neighbor in shaders.rs); for EMPTY, and for a tree in its first
//       two steps (causes GROW then MATURE), steps since the ground was
//       cleared by fire (saturating at 255).
// A fire lasts two steps (catching, then burning), leaves a burnt stand for
// one step, and then clears to bare dirt, which greens back over a few steps.
// Keeping (prev, cause, a) alongside the state means a renderer can pick the
// right transition animation per cell without any extra bookkeeping.
//
// Texel rows run up the screen (y up), as in the WebGL original. The state
// constants (EMPTY, CAUSE_GROW, SETTLED, ...) are prepended from shaders.rs.

fn hash(x0: u32) -> u32 {
  var x = x0;
  x ^= x >> 16u; x *= 0x7feb352du;
  x ^= x >> 15u; x *= 0x846ca68bu;
  x ^= x >> 16u;
  return x;
}

fn hash3(c: vec2<u32>, salt: u32) -> u32 {
  return hash(c.x ^ hash(c.y ^ hash(salt)));
}

// A fullscreen triangle; no vertex buffers needed.
fn fullscreen(i: u32) -> vec4<f32> {
  let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
