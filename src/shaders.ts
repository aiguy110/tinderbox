// Cell state lives in an RGBA8UI texture, one texel per cell:
//   R = current state, G = previous state, B = cause of the last transition,
//   A = for FIRE, a bitmask of the neighbors that were burning when it caught
//       (see Neighbor); for EMPTY, and for a tree in its first two steps
//       (causes GROW then MATURE), steps since the ground was cleared by fire
//       (saturating at 255).
// A fire lasts two steps (catching, then burning), leaves a burnt stand for
// one step, and then clears to bare dirt, which greens back over a few steps.
// Keeping (prev, cause, A) alongside the state means a renderer can pick the
// right transition animation per cell without any extra bookkeeping.

/** Lightning strikes per step whose cells are picked on the CPU (see sim.ts). */
export const MAX_STRIKES = 16;

export const State = { EMPTY: 0, TREE: 1, FIRE: 2, BURNT: 3 } as const;
export const Cause = { NONE: 0, GROW: 1, SPREAD: 2, LIGHTNING: 3, BURNOUT: 4, BURNING: 5, CLEAR: 6, MATURE: 7 } as const;
/** Ground age of grass that has never burned. */
export const SETTLED = 255;

// Neighbor bits for the sources mask (y up).
export const Neighbor = { W: 0, N: 1, E: 2, S: 3, SW: 4, NW: 5, NE: 6, SE: 7 } as const;
const neighborOffsets = ['ivec2(-1, 0)', 'ivec2(0, 1)', 'ivec2(1, 0)', 'ivec2(0, -1)', 'ivec2(-1, -1)', 'ivec2(-1, 1)', 'ivec2(1, 1)', 'ivec2(1, -1)'];

// Looks in the stills atlas (tools/atlas/stills.py LOOKS). A sim state maps
// onto one look, except EMPTY, which blends from dirt to grass as it regrows.
export const LOOKS = ['grass', 'forest', 'fire', 'burnt', 'dirt'] as const;

export const common = /* glsl */ `#version 300 es
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
const uint CAUSE_MATURE = ${Cause.MATURE}u;
const uint SETTLED = ${SETTLED}u;

uint hash(uint x) {
  x ^= x >> 16; x *= 0x7feb352du;
  x ^= x >> 15; x *= 0x846ca68bu;
  x ^= x >> 16;
  return x;
}

uint hash3(uvec2 c, uint salt) {
  return hash(c.x ^ hash(c.y ^ hash(salt)));
}

// Steps since the ground was cleared by fire, at progress s through the step:
// 0 when the CLEAR step starts, continuous through a sapling's two growing
// steps. Large for ground that never burned and for grown forest.
float groundAge(uvec4 t, float s) {
  if (t.a == SETTLED) return 1e4;
  if (t.r == EMPTY) return float(t.a) + s;
  if (t.r == TREE && t.b == CAUSE_GROW) return float(t.a) + 1.0 + s;
  if (t.r == TREE && t.b == CAUSE_MATURE) return float(t.a) + 2.0 + s;
  return 1e4;
}

// How far the ground has greened back, 0 (bare dirt) to 1 (grass). Grass
// starts after the CLEAR step and takes uRegrow steps.
uniform float uRegrow;
float regrowth(uvec4 t, float s) {
  return clamp((groundAge(t, s) - 1.0) / uRegrow, 0.0, 1.0);
}
`;

// The same hash as the shaders' hash3/hashf, so the CPU can find where the
// dual renderer starts a lightning fire.
function hash(x: number): number {
  x ^= x >>> 16;
  x = Math.imul(x, 0x7feb352d);
  x ^= x >>> 15;
  x = Math.imul(x, 0x846ca68b);
  x ^= x >>> 16;
  return x >>> 0;
}

export function hashf(cx: number, cy: number, salt: number): number {
  return ((hash(cx ^ hash(cy ^ hash(salt))) & 0xffffff) >>> 0) / 16777216;
}

// Value noise and fbm, shared by the dual and scene renderers.
export const noise = /* glsl */ `
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

// Grass coverage of ground that has regrown by r: it comes back in patches
// that spread and join.
float grassAt(vec2 p, float r) {
  if (r >= 1.0) return 1.0;
  if (r <= 0.0) return 0.0;
  float n = 0.75 * fbm(p * 2.2, 4u) + 0.25 * vnoise(p * 11.0, 16u);
  return smoothstep(n - 0.06, n + 0.06, r * 1.4 - 0.2);
}
`;

// The fire front's timing within a cell, shared by the dual and scene renderers.
export const arrival = /* glsl */ `
// How far through the catching step (0 to 1) the fire front reaches a point
// q in the cell, coming from the burning neighbors in the sources mask, or
// spreading out from a random spot for lightning (an empty mask). The mask
// is kept through the burning step.
float arrival(vec2 q, uvec4 t, ivec2 c) {
  uint m = t.a;
  if (m == 0u) {
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
uniform ivec2 uStrikes[${MAX_STRIKES}];
uniform int uStrikeCount;

out uvec4 outState;

ivec2 gridSize;

bool struck(ivec2 p) {
  for (int i = 0; i < uStrikeCount; i++) {
    if (uStrikes[i] == p) return true;
  }
  return false;
}

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
  uint a = 0u;

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
    for (int i = 0; i < (uMoore ? 8 : 4); i++) {
      if (burning(p + NEIGHBORS[i])) a |= 1u << i;
    }
    if (a != 0u) {
      next = FIRE;
      cause = CAUSE_SPREAD;
    } else if (struck(p) || rand(p, 0x9e3779b9u) < uLightning) {
      next = FIRE;
      cause = CAUSE_LIGHTNING;
    } else if (cur.b == CAUSE_GROW) {
      // A second step of growing, still keeping the ground's age.
      cause = CAUSE_MATURE;
      a = cur.a;
    }
  } else if (rand(p, 0x85ebca6bu) < uGrowth) {
    // A sapling keeps the age of the ground it grows from, to draw it.
    next = TREE;
    cause = CAUSE_GROW;
    a = cur.a;
  } else {
    a = cur.a == SETTLED ? SETTLED : min(cur.a + 1u, SETTLED - 1u);
  }

  outState = uvec4(next, s, cause, a);
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
    c = mix(vec3(0.04, 0.20, 0.09), vec3(0.08, 0.30, 0.12), jitter);
    if (cause == CAUSE_GROW) c += vec3(0.08, 0.10, 0.02);
  } else if (s == FIRE) {
    c = mix(vec3(1.0, 0.35, 0.05), vec3(1.0, 0.75, 0.2), jitter);
    if (cause == CAUSE_LIGHTNING) c = vec3(0.85, 0.92, 1.0);
    if (cause == CAUSE_BURNING) c *= 0.7;
  } else if (s == BURNT) {
    c = mix(vec3(0.07, 0.06, 0.05), vec3(0.11, 0.09, 0.08), jitter);
  } else {
    vec3 dirt = mix(vec3(0.20, 0.14, 0.09), vec3(0.24, 0.17, 0.11), jitter);
    vec3 grass = mix(vec3(0.30, 0.52, 0.16), vec3(0.36, 0.58, 0.20), jitter);
    c = mix(dirt, grass, regrowth(t, 1.0));
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
// those 4 cells' looks and borders between tiles depend only on the cells
// they pass through.
//
// Transitions are drawn here rather than generated. At each pixel, each of
// the 4 cells shows a blend of up to 3 looks, with weights that are a
// function of the cell and world position: fire sweeps in from the neighbors
// that lit it, saplings grow out from the middle, flames die out and the
// burnt stand clears in patches, and grass creeps back over the dirt. The
// pixel mixes the tiles for every combination of its 4 cells' looks,
// weighted multilinearly. Because the weights depend only on the cell and
// the world position, both tiles on a shared border agree and the result
// stays seamless.
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

const uint GRASS = 0u;
const uint FOREST = 1u;
const uint FLAMES = 2u;
const uint CHARRED = 3u;
const uint DIRT = 4u;
const uint LOOKS = ${LOOKS.length}u;

ivec2 gridSize;
vec2 st, gx, gy;

uvec4 cellAt(ivec2 c) {
  if (uWrap) {
    c = (c + gridSize) % gridSize;
  } else if (any(lessThan(c, ivec2(0))) || any(greaterThanEqual(c, gridSize))) {
    return uvec4(EMPTY, EMPTY, CAUSE_NONE, SETTLED);
  }
  return texelFetch(uState, c, 0);
}

${noise}
vec4 corners(vec2 u) {
  return vec4((1.0 - u.x) * (1.0 - u.y), u.x * (1.0 - u.y), (1.0 - u.x) * u.y, u.x * u.y);
}

${arrival}
// How brightly a cell burns and how much it smokes over the step.
vec2 heat(uvec4 t, float s) {
  if (t.g == TREE && t.r == FIRE) return vec2(smoothstep(0.0, 0.8, s), 0.5 * s);
  if (t.r == FIRE) return vec2(1.0, 0.5);
  if (t.g == FIRE && t.r == BURNT) return vec2(1.0 - smoothstep(0.1, 0.9, s), 0.5 + 0.5 * sin(3.1416 * s));
  if (t.g == BURNT && t.r == EMPTY) return vec2(0.0, 0.5 * (1.0 - s));
  return vec2(0.0);
}

// One cell's look at this pixel, as up to 3 looks with weights, plus the
// effects drawn on top.
struct Corner {
  uvec3 look;
  vec3 w;
  float ember;
  float front;
  float flash;
};

Corner blend(uint a, uint b, float w) {
  return Corner(uvec3(a, b, a), vec3(1.0 - w, w, 0.0), 0.0, 0.0, 0.0);
}

Corner corner(uvec4 t, ivec2 c, vec2 p, float s) {
  vec2 q = p - vec2(c);
  vec2 qc = clamp(q, 0.0, 1.0);
  float inside = smoothstep(-0.02, 0.02, min(min(q.x, q.y), min(1.0 - q.x, 1.0 - q.y)));
  if (t.r == EMPTY && t.g == BURNT) {
    // The burnt stand falls into the ash in patches, and the embers go out.
    float n = fbm(p * 3.0, 3u);
    Corner k = blend(CHARRED, DIRT, smoothstep(n - 0.12, n + 0.12, s * 1.4 - 0.2));
    k.ember = 0.6 * (1.0 - smoothstep(0.0, 0.5, s));
    return k;
  }
  if (t.r == EMPTY) return blend(DIRT, GRASS, grassAt(p, regrowth(t, s)));
  if (t.r == TREE && t.g == EMPTY) {
    // Grows out from the middle of the cell, over whatever ground was there.
    float r = length(qc - 0.5) * 1.5 + (fbm(p * 3.0, 1u) - 0.5) * 0.4;
    float w = smoothstep(r - 0.12, r + 0.12, s * 1.4 - 0.1);
    float g = grassAt(p, regrowth(t, s));
    return Corner(uvec3(DIRT, GRASS, FOREST), vec3((1.0 - g) * (1.0 - w), g * (1.0 - w), w), 0.0, 0.0, 0.0);
  }
  if (t.r == TREE) return blend(FOREST, FOREST, 0.0);
  if (t.r == FIRE && t.g == TREE) {
    // The front crosses the cell over the step, ragged and glowing.
    float a = arrival(qc, t, c) + (fbm(p * 2.5, 2u) - 0.5) * 0.3;
    float f = s * 1.3 - 0.15;
    Corner k = blend(FOREST, FLAMES, smoothstep(a - 0.05, a + 0.05, f));
    // Flame tongues along the front, and a warm cast on the canopy ahead.
    float rag = fbm(p * 6.0 + vec2(0.0, -2.0) * uTime, 9u);
    float d = f - a;
    k.front = inside * (exp(-pow(d / (0.03 + 0.06 * rag), 2.0)) * (0.4 + rag) + 0.35 * smoothstep(-0.3, 0.0, d) * step(d, 0.0));
    if (t.b == CAUSE_LIGHTNING) k.flash = inside * max(0.0, 1.0 - s / 0.12);
    return k;
  }
  if (t.r == FIRE) return blend(FLAMES, FLAMES, 0.0);
  // BURNT: flames die out in patches, leaving charred snags and embers.
  float n = fbm(p * 3.0, 3u);
  Corner k = blend(FLAMES, CHARRED, smoothstep(n - 0.12, n + 0.12, s * 1.4 - 0.2));
  k.ember = smoothstep(0.2, 0.6, s);
  return k;
}

vec3 tile(uvec4 v) {
  uint i = v.x + LOOKS * (v.y + LOOKS * (v.z + LOOKS * v.w));
  return textureGrad(uTiles, vec3(st, float(i)), gx, gy).rgb;
}

void main() {
  gridSize = textureSize(uState, 0);
  vec2 p = gl_FragCoord.xy / uCellPx;
  ivec2 v = ivec2(floor(p + 0.5));
  vec2 uv = p + 0.5 - vec2(v);
  vec4 tent = corners(uv);

  // Atlas rows run top-down; gradients come from the continuous p so mip
  // selection doesn't spike at tile borders.
  st = vec2(uv.x, 1.0 - uv.y);
  gx = dFdx(p) * vec2(1.0, -1.0);
  gy = dFdy(p) * vec2(1.0, -1.0);
  float s = clamp(uProgress, 0.0, 1.0);

  const ivec2 offsets[4] = ivec2[4](ivec2(-1, -1), ivec2(0, -1), ivec2(-1, 0), ivec2(0, 0));
  uvec3 looks[4];
  vec3 ws[4];
  vec4 ember, fire, smokes;
  float front = 0.0, flash = 0.0;
  for (int i = 0; i < 4; i++) {
    ivec2 c = v + offsets[i];
    uvec4 t = cellAt(c);
    Corner k = corner(t, c, p, s);
    looks[i] = k.look;
    ws[i] = k.w;
    ember[i] = k.ember;
    front += k.front;
    flash += k.flash;
    vec2 h = heat(t, s);
    fire[i] = h.x;
    smokes[i] = h.y;
  }

  // Multilinear blend over every combination of the corners' looks.
  vec3 c = vec3(0.0);
  float total = 0.0;
  for (int m = 0; m < 81; m++) {
    int r = m;
    float wt = 1.0;
    uvec4 look;
    for (int i = 0; i < 4; i++) {
      int j = r % 3;
      r /= 3;
      wt *= ws[i][j];
      if (wt < 0.002) break;
      look[i] = looks[i][j];
    }
    if (wt < 0.002) continue;
    c += wt * tile(look);
    total += wt;
  }
  c /= total;

  // Flames in the tiles dance, and fire lights up what's around it. The front
  // burns brightest on canopy, and embers glow on charred wood.
  float canopy = 0.1 + 0.9 * smoothstep(0.0, 0.08, c.g - max(c.r, c.b));
  float charred = 1.0 - smoothstep(0.08, 0.2, dot(c, vec3(0.299, 0.587, 0.114)));
  float dance = fbm(p * 5.0 + vec2(0.7, -2.3) * uTime, 6u);
  float flames = smoothstep(0.3, 0.6, c.r - c.b) * smoothstep(0.45, 0.8, c.r);
  c *= 1.0 + flames * (dance - 0.45) * 1.2;
  float light = dot(tent, fire) * (0.6 + 0.8 * fbm(p * 1.5 + vec2(0.0, 0.8) * uTime, 5u));
  c += c * light * vec3(0.8, 0.38, 0.1) + light * vec3(0.06, 0.02, 0.0);
  front *= canopy;
  c += c * front * vec3(1.6, 0.6, 0.1) + front * front * vec3(0.9, 0.45, 0.1);
  // Smoke drifts off burning and smouldering cells.
  float smoke = dot(tent, smokes) * smoothstep(0.35, 0.8, fbm(p * 2.0 - vec2(0.35, 0.6) * uTime, 10u));
  c = mix(c, vec3(0.32, 0.31, 0.3), 0.55 * smoke);
  float sparks = smoothstep(0.72, 0.85, vnoise(p * 14.0, 7u)) * (0.3 + 0.7 * vnoise(p * 9.0 + vec2(1.3, -0.7) * uTime * 3.0, 8u));
  c += dot(tent, ember) * charred * sparks * vec3(1.0, 0.35, 0.05) * 1.2;
  c = mix(c, vec3(0.85, 0.9, 1.0), 0.8 * clamp(flash, 0.0, 1.0));

  if (uGridLines) {
    vec2 g = fract(p);
    float edge = step(g.x, 1.0 / uCellPx) + step(g.y, 1.0 / uCellPx);
    c *= 1.0 - 0.35 * min(edge, 1.0);
  }

  outColor = vec4(c, 1.0);
}
`;
