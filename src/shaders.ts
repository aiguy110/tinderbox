// Cell state lives in an RGBA8UI texture, one texel per cell:
//   R = current state, G = previous state, B = cause of the last transition,
//   A = for SPREAD, a bitmask of the neighbors that were burning (see Neighbor).
// A fire lasts two steps (catching, then burning), leaves a burnt stand for
// one step, and then clears to empty ground.
// Keeping (prev, cause, sources) alongside the state means a renderer can pick
// the right transition animation per cell without any extra bookkeeping.

export const State = { EMPTY: 0, TREE: 1, FIRE: 2, BURNT: 3 } as const;
export const Cause = { NONE: 0, GROW: 1, SPREAD: 2, LIGHTNING: 3, BURNOUT: 4, BURNING: 5, CLEAR: 6 } as const;

// Neighbor bits for the sources mask (y up).
export const Neighbor = { W: 0, N: 1, E: 2, S: 3, SW: 4, NW: 5, NE: 6, SE: 7 } as const;
const neighborOffsets = ['ivec2(-1, 0)', 'ivec2(0, 1)', 'ivec2(1, 0)', 'ivec2(0, -1)', 'ivec2(-1, -1)', 'ivec2(-1, 1)', 'ivec2(1, 1)', 'ivec2(1, -1)'];

const common = /* glsl */ `#version 300 es
precision highp float;
precision highp int;
precision highp usampler2D;

const uint EMPTY = ${State.EMPTY}u;
const uint TREE = ${State.TREE}u;
const uint FIRE = ${State.FIRE}u;
const uint BURNT = ${State.BURNT}u;

const uint CAUSE_NONE = ${Cause.NONE}u;
const uint CAUSE_GROW = ${Cause.GROW}u;
const uint CAUSE_SPREAD = ${Cause.SPREAD}u;
const uint CAUSE_LIGHTNING = ${Cause.LIGHTNING}u;
const uint CAUSE_BURNOUT = ${Cause.BURNOUT}u;
const uint CAUSE_BURNING = ${Cause.BURNING}u;
const uint CAUSE_CLEAR = ${Cause.CLEAR}u;

uint hash(uint x) {
  x ^= x >> 16; x *= 0x7feb352du;
  x ^= x >> 15; x *= 0x846ca68bu;
  x ^= x >> 16;
  return x;
}

uint hash3(uvec2 c, uint salt) {
  return hash(c.x ^ hash(c.y ^ hash(salt)));
}
`;

// Fullscreen triangle; no vertex buffers needed.
export const fullscreenVert = /* glsl */ `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
`;

export const stepFrag = /* glsl */ `${common}
uniform usampler2D uState;
uniform uint uSeed;
uniform float uGrowth;
uniform float uLightning;
uniform bool uMoore;
uniform bool uWrap;

out uvec4 outState;

ivec2 gridSize;

const ivec2 NEIGHBORS[8] = ivec2[8](${neighborOffsets.join(', ')});

float rand(ivec2 p, uint salt) {
  return float(hash3(uvec2(p), uSeed ^ salt)) / 4294967296.0;
}

bool burning(ivec2 p) {
  if (uWrap) {
    p = (p + gridSize) % gridSize;
  } else if (any(lessThan(p, ivec2(0))) || any(greaterThanEqual(p, gridSize))) {
    return false;
  }
  return texelFetch(uState, p, 0).r == FIRE;
}

void main() {
  gridSize = textureSize(uState, 0);
  ivec2 p = ivec2(gl_FragCoord.xy);
  uvec4 cur = texelFetch(uState, p, 0);
  uint s = cur.r;
  uint next = s;
  uint cause = CAUSE_NONE;
  uint sources = 0u;

  if (s == FIRE && cur.g == TREE) {
    // Caught last step: burn for a step, keeping where the fire came from.
    cause = CAUSE_BURNING;
    sources = cur.a;
  } else if (s == FIRE) {
    next = BURNT;
    cause = CAUSE_BURNOUT;
  } else if (s == BURNT) {
    next = EMPTY;
    cause = CAUSE_CLEAR;
  } else if (s == TREE) {
    for (int i = 0; i < (uMoore ? 8 : 4); i++) {
      if (burning(p + NEIGHBORS[i])) sources |= 1u << i;
    }
    if (sources != 0u) {
      next = FIRE;
      cause = CAUSE_SPREAD;
    } else if (rand(p, 0x9e3779b9u) < uLightning) {
      next = FIRE;
      cause = CAUSE_LIGHTNING;
    }
  } else if (s == EMPTY && rand(p, 0x85ebca6bu) < uGrowth) {
    next = TREE;
    cause = CAUSE_GROW;
  }

  outState = uvec4(next, s, cause, sources);
}
`;

// Flat renderer: one color per state, with a few transition accents.
export const flatFrag = /* glsl */ `${common}
uniform usampler2D uState;
uniform float uCellPx;
uniform bool uGridLines;

out vec4 outColor;

void main() {
  vec2 cellF = gl_FragCoord.xy / uCellPx;
  ivec2 cell = ivec2(cellF);
  uvec4 t = texelFetch(uState, cell, 0);
  uint s = t.r;
  uint cause = t.b;
  float jitter = float(hash3(uvec2(cell), 0x27d4eb2du) & 255u) / 255.0;

  vec3 c;
  if (s == TREE) {
    c = mix(vec3(0.07, 0.30, 0.10), vec3(0.16, 0.45, 0.14), jitter);
    if (cause == CAUSE_GROW) c += vec3(0.08, 0.10, 0.02);
  } else if (s == FIRE) {
    c = mix(vec3(1.0, 0.35, 0.05), vec3(1.0, 0.75, 0.2), jitter);
    if (cause == CAUSE_LIGHTNING) c = vec3(0.85, 0.92, 1.0);
    if (cause == CAUSE_BURNING) c *= 0.7;
  } else if (s == BURNT) {
    c = mix(vec3(0.14, 0.12, 0.11), vec3(0.20, 0.17, 0.15), jitter);
  } else {
    c = mix(vec3(0.06, 0.05, 0.04), vec3(0.09, 0.07, 0.05), jitter);
    if (cause == CAUSE_BURNOUT) c = vec3(0.30, 0.12, 0.06);
  }

  if (uGridLines) {
    vec2 f = fract(cellF);
    float edge = step(f.x, 1.0 / uCellPx) + step(f.y, 1.0 / uCellPx);
    c *= 1.0 - 0.35 * min(edge, 1.0);
  }

  outColor = vec4(c, 1.0);
}
`;

// Dual-grid renderer (docs/PLAN.md). Tiles sit on grid vertices and each
// covers a quarter of the 4 cells around it, so a tile is chosen purely by
// those 4 states and borders between tiles depend only on the cells they
// pass through.
//
// Transitions are drawn here rather than generated. Over a step, each cell
// blends per pixel from its previous look to its current one, with a weight
// that is a function of world position: fire sweeps in from the neighbors
// that lit it, saplings grow out from the middle, flames die out and ash
// clears in noisy patches. A pixel mixes the tiles for every combination of
// its 4 cells' looks, weighted multilinearly. Because the weights depend only
// on the cell and the world position, both tiles on a shared border agree
// and the result stays seamless.
//
// The stills only cover {EMPTY, TREE, FIRE}; BURNT is drawn by charring the
// trees in the TREE tile.
export const dualFrag = /* glsl */ `${common}
precision highp sampler2DArray;

uniform usampler2D uState;
uniform sampler2DArray uTiles;
uniform float uCellPx;
uniform bool uGridLines;
uniform bool uWrap;
uniform float uProgress;
uniform float uTime;

out vec4 outColor;

ivec2 gridSize;
vec2 st, gx, gy;
// Bilinear weights of the 4 corner cells, and a sharpened version that is
// close to each cell's own quarter.
vec4 tent, own;

uvec4 cellAt(ivec2 c) {
  if (uWrap) {
    c = (c + gridSize) % gridSize;
  } else if (any(lessThan(c, ivec2(0))) || any(greaterThanEqual(c, gridSize))) {
    return uvec4(EMPTY, EMPTY, CAUSE_NONE, 0u);
  }
  return texelFetch(uState, c, 0);
}

float hashf(ivec2 i, uint salt) {
  return float(hash3(uvec2(i), salt) & 0xffffffu) / 16777216.0;
}

float vnoise(vec2 p, uint salt) {
  ivec2 i = ivec2(floor(p));
  vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(mix(hashf(i, salt), hashf(i + ivec2(1, 0), salt), u.x),
             mix(hashf(i + ivec2(0, 1), salt), hashf(i + ivec2(1, 1), salt), u.x), u.y);
}

float fbm(vec2 p, uint salt) {
  return 0.5 * vnoise(p, salt) + 0.3 * vnoise(p * 2.03 + 17.1, salt) + 0.2 * vnoise(p * 4.07 + 31.7, salt);
}

vec4 corners(vec2 u) {
  return vec4((1.0 - u.x) * (1.0 - u.y), u.x * (1.0 - u.y), (1.0 - u.x) * u.y, u.x * u.y);
}

// How far through the catching step (0 to 1) the fire front reaches a point
// q in the cell, coming from the burning neighbors in the sources mask, or
// spreading out from a random spot for lightning.
float arrival(vec2 q, uvec4 t, ivec2 c) {
  uint m = t.a;
  if (t.b != CAUSE_SPREAD || m == 0u) {
    vec2 o = 0.3 + 0.4 * vec2(hashf(c, 11u), hashf(c, 12u));
    return length(q - o) / 0.95;
  }
  float a = 2.0;
  if ((m & 1u) != 0u) a = min(a, q.x);
  if ((m & 2u) != 0u) a = min(a, 1.0 - q.y);
  if ((m & 4u) != 0u) a = min(a, 1.0 - q.x);
  if ((m & 8u) != 0u) a = min(a, q.y);
  if ((m & 16u) != 0u) a = min(a, length(q) * 0.7071);
  if ((m & 32u) != 0u) a = min(a, length(q - vec2(0.0, 1.0)) * 0.7071);
  if ((m & 64u) != 0u) a = min(a, length(q - vec2(1.0)) * 0.7071);
  if ((m & 128u) != 0u) a = min(a, length(q - vec2(1.0, 0.0)) * 0.7071);
  return a;
}

// One cell's look at this pixel: blending from \`from\` to \`to\` by w, plus
// how much it lights its surroundings and the effects drawn on top.
struct Corner {
  uint from;
  uint to;
  float w;
  float fire;
  float ember;
  float front;
  float flash;
};

Corner corner(uvec4 t, ivec2 c, vec2 p, float s) {
  Corner k = Corner(t.r, t.r, 0.0, 0.0, 0.0, 0.0, 0.0);
  vec2 q = p - vec2(c);
  vec2 qc = clamp(q, 0.0, 1.0);
  float inside = smoothstep(-0.02, 0.02, min(min(q.x, q.y), min(1.0 - q.x, 1.0 - q.y)));
  if (t.g == EMPTY && t.r == TREE) {
    // Grows out from the middle of the cell.
    float r = length(qc - 0.5) * 1.5 + (fbm(p * 3.0, 1u) - 0.5) * 0.4;
    k.from = EMPTY;
    k.w = smoothstep(r - 0.12, r + 0.12, s * 1.4 - 0.1);
  } else if (t.g == TREE && t.r == FIRE) {
    // The front crosses the cell over the step, ragged and glowing.
    float a = arrival(qc, t, c) + (fbm(p * 2.5, 2u) - 0.5) * 0.3;
    float f = s * 1.3 - 0.15;
    k.from = TREE;
    k.w = smoothstep(a - 0.05, a + 0.05, f);
    k.fire = smoothstep(0.0, 0.8, s);
    k.front = inside * exp(-pow((f - a) / 0.05, 2.0));
    if (t.b == CAUSE_LIGHTNING) k.flash = inside * max(0.0, 1.0 - s / 0.12);
  } else if (t.r == FIRE) {
    k.fire = 1.0;
  } else if (t.g == FIRE && t.r == BURNT) {
    // Flames die out in patches, leaving charred trees and embers.
    float n = fbm(p * 3.0, 3u);
    k.from = FIRE;
    k.w = smoothstep(n - 0.12, n + 0.12, s * 1.4 - 0.2);
    k.fire = 1.0 - smoothstep(0.1, 0.9, s);
    k.ember = smoothstep(0.2, 0.6, s);
  } else if (t.g == BURNT && t.r == EMPTY) {
    // Ash settles and grass comes back through it.
    float n = fbm(p * 3.0, 4u);
    k.from = BURNT;
    k.w = smoothstep(n - 0.12, n + 0.12, s * 1.4 - 0.2);
    k.ember = 0.6 * (1.0 - smoothstep(0.0, 0.5, s));
  }
  return k;
}

vec3 tile(uvec4 v) {
  uint i = v.x + 3u * v.y + 9u * v.z + 27u * v.w;
  return textureGrad(uTiles, vec3(st, float(i)), gx, gy).rgb;
}

// A tile for any 4 looks. BURNT corners take the TREE tile's trees, charred,
// over the EMPTY tile's ground, scorched.
vec3 shade(uvec4 v) {
  bvec4 burnt = equal(v, uvec4(BURNT));
  if (!any(burnt)) return tile(v);
  uvec4 trees = v, bare = v;
  for (int i = 0; i < 4; i++) {
    if (burnt[i]) {
      trees[i] = TREE;
      bare[i] = EMPTY;
    }
  }
  vec3 t = tile(trees);
  vec3 g = tile(bare);
  float canopy = smoothstep(0.06, 0.2, length(t - g));
  float lum = dot(t, vec3(0.299, 0.587, 0.114));
  vec3 charred = lum * vec3(0.42, 0.36, 0.32);
  g *= 1.0 - 0.45 * dot(own, vec4(burnt));
  return mix(g, charred, canopy);
}

void main() {
  gridSize = textureSize(uState, 0);
  vec2 p = gl_FragCoord.xy / uCellPx;
  ivec2 v = ivec2(floor(p + 0.5));
  vec2 uv = p + 0.5 - vec2(v);
  tent = corners(uv);
  own = corners(smoothstep(0.3, 0.7, uv));

  // Atlas rows run top-down; gradients come from the continuous p so mip
  // selection doesn't spike at tile borders.
  st = vec2(uv.x, 1.0 - uv.y);
  gx = dFdx(p) * vec2(1.0, -1.0);
  gy = dFdy(p) * vec2(1.0, -1.0);
  float s = clamp(uProgress, 0.0, 1.0);

  const ivec2 offsets[4] = ivec2[4](ivec2(-1, -1), ivec2(0, -1), ivec2(-1, 0), ivec2(0, 0));
  uvec4 from, to;
  vec4 w, fire, ember;
  float front = 0.0, flash = 0.0;
  for (int i = 0; i < 4; i++) {
    ivec2 c = v + offsets[i];
    Corner k = corner(cellAt(c), c, p, s);
    from[i] = k.from;
    to[i] = k.to;
    w[i] = k.w;
    fire[i] = k.fire;
    ember[i] = k.ember;
    front += k.front;
    flash += k.flash;
  }

  // Multilinear blend over the corners that are mid-transition.
  vec3 c = vec3(0.0);
  float total = 0.0;
  for (int m = 0; m < 16; m++) {
    float wt = 1.0;
    uvec4 look;
    for (int i = 0; i < 4; i++) {
      bool hi = ((m >> i) & 1) != 0;
      bool moving = from[i] != to[i];
      if (hi && !moving) {
        wt = 0.0;
        break;
      }
      look[i] = hi ? to[i] : from[i];
      if (moving) wt *= hi ? w[i] : 1.0 - w[i];
    }
    if (wt < 0.002) continue;
    c += wt * shade(look);
    total += wt;
  }
  c /= total;

  // Flames in the tiles dance, and fire lights up what's around it.
  float dance = fbm(p * 5.0 + vec2(0.7, -2.3) * uTime, 6u);
  float flames = smoothstep(0.3, 0.6, c.r - c.b) * smoothstep(0.45, 0.8, c.r);
  c *= 1.0 + flames * (dance - 0.45) * 1.2;
  float light = dot(tent, fire) * (0.6 + 0.8 * fbm(p * 1.5 + vec2(0.0, 0.8) * uTime, 5u));
  c += c * light * vec3(0.8, 0.38, 0.1) + light * vec3(0.06, 0.02, 0.0);
  c += front * vec3(1.0, 0.6, 0.2) * (0.7 + 0.6 * dance) * 1.3;
  float sparks = smoothstep(0.72, 0.85, vnoise(p * 14.0, 7u)) * (0.3 + 0.7 * vnoise(p * 9.0 + vec2(1.3, -0.7) * uTime * 3.0, 8u));
  c += dot(own, ember) * sparks * vec3(1.0, 0.35, 0.05) * 1.5;
  c = mix(c, vec3(0.85, 0.9, 1.0), 0.8 * clamp(flash, 0.0, 1.0));

  if (uGridLines) {
    vec2 g = fract(p);
    float edge = step(g.x, 1.0 / uCellPx) + step(g.y, 1.0 / uCellPx);
    c *= 1.0 - 0.35 * min(edge, 1.0);
  }

  outColor = vec4(c, 1.0);
}
`;
