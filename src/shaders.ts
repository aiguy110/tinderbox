// Cell state lives in an RGBA8UI texture, one texel per cell:
//   R = current state, G = previous state, B = cause of the last transition,
//   A = for SPREAD, a bitmask of the neighbors that were burning (see Neighbor).
// Keeping (prev, cause, sources) alongside the state means a renderer can pick
// the right transition animation per cell without any extra bookkeeping.

export const State = { EMPTY: 0, TREE: 1, FIRE: 2 } as const;
export const Cause = { NONE: 0, GROW: 1, SPREAD: 2, LIGHTNING: 3, BURNOUT: 4 } as const;

// Neighbor bits, ordered so that `bit & 3` is the number of quarter turns
// (counterclockwise, y up) that map the canonical source onto that neighbor.
// Edge clips are drawn with the fire arriving from the west, corner clips
// from the south-west.
export const Neighbor = { W: 0, N: 1, E: 2, S: 3, SW: 4, NW: 5, NE: 6, SE: 7 } as const;
const neighborOffsets = ['ivec2(-1, 0)', 'ivec2(0, 1)', 'ivec2(1, 0)', 'ivec2(0, -1)', 'ivec2(-1, -1)', 'ivec2(-1, 1)', 'ivec2(1, 1)', 'ivec2(1, -1)'];

// Animation clips, in the order the renderer indexes them.
export const CLIPS = ['empty', 'tree', 'grow', 'spread-edge', 'spread-corner', 'lightning', 'burnout'] as const;
export type ClipName = (typeof CLIPS)[number];
const clipConsts = CLIPS.map((c, i) => `const int CLIP_${c.toUpperCase().replace('-', '_')} = ${i};`).join('\n');

const common = /* glsl */ `#version 300 es
precision highp float;
precision highp int;
precision highp usampler2D;

const uint EMPTY = ${State.EMPTY}u;
const uint TREE = ${State.TREE}u;
const uint FIRE = ${State.FIRE}u;

const uint CAUSE_NONE = ${Cause.NONE}u;
const uint CAUSE_GROW = ${Cause.GROW}u;
const uint CAUSE_SPREAD = ${Cause.SPREAD}u;
const uint CAUSE_LIGHTNING = ${Cause.LIGHTNING}u;
const uint CAUSE_BURNOUT = ${Cause.BURNOUT}u;

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
  uint s = texelFetch(uState, p, 0).r;
  uint next = s;
  uint cause = CAUSE_NONE;
  uint sources = 0u;

  if (s == FIRE) {
    next = EMPTY;
    cause = CAUSE_BURNOUT;
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
  } else if (rand(p, 0x85ebca6bu) < uGrowth) {
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

// Sprite renderer: each cell plays an animation clip from a texture-array
// atlas. Transition clips (grow, spread, lightning, burnout) run once over the
// step that produced them; idle clips (empty, tree) loop on wall-clock time
// with a per-cell phase offset so neighbors don't move in lockstep.
export const spriteFrag = /* glsl */ `${common}
precision highp sampler2DArray;
${clipConsts}

uniform usampler2D uState;
uniform sampler2DArray uAtlas;
uniform float uCellPx;
uniform bool uGridLines;
uniform float uProgress;
uniform float uTime;
// Per clip: first atlas layer, frame count, and loop period in seconds
// (0 for clips timed to the step).
uniform ivec2 uClipFrames[${CLIPS.length}];
uniform float uClipSeconds[${CLIPS.length}];

out vec4 outColor;

// Rotate by r quarter turns counterclockwise.
vec2 turn(vec2 v, uint r) {
  if (r == 1u) return vec2(-v.y, v.x);
  if (r == 2u) return -v;
  if (r == 3u) return vec2(v.y, -v.x);
  return v;
}

// Pick one set bit of a 4-bit mask, starting the search at a per-cell offset.
uint pickBit(uint mask, uint start) {
  for (uint i = 0u; i < 4u; i++) {
    uint b = (start + i) & 3u;
    if (((mask >> b) & 1u) != 0u) return b;
  }
  return start & 3u;
}

void main() {
  vec2 cellF = gl_FragCoord.xy / uCellPx;
  ivec2 cell = ivec2(cellF);
  uvec4 t = texelFetch(uState, cell, 0);
  uint s = t.r;
  uint cause = t.b;
  uint h = hash3(uvec2(cell), 0x27d4eb2du);

  int clip;
  uint r = 0u;
  bool mirror = (h & 0x100u) != 0u;
  if (s == TREE) {
    clip = cause == CAUSE_GROW ? CLIP_GROW : CLIP_TREE;
  } else if (s == FIRE) {
    if (cause == CAUSE_SPREAD && t.a != 0u) {
      // Orient the clip so the fire arrives from an actual burning neighbor.
      mirror = false;
      uint edges = t.a & 15u;
      clip = edges != 0u ? CLIP_SPREAD_EDGE : CLIP_SPREAD_CORNER;
      r = pickBit(edges != 0u ? edges : t.a >> 4, h);
    } else {
      clip = CLIP_LIGHTNING;
    }
  } else {
    clip = cause == CAUSE_BURNOUT ? CLIP_BURNOUT : CLIP_EMPTY;
  }

  ivec2 frames = uClipFrames[clip];
  float period = uClipSeconds[clip];
  float pos;
  if (period > 0.0) {
    float phase = float((h >> 12) & 1023u) / 1024.0;
    pos = fract(uTime / period + phase) * float(frames.y);
  } else {
    pos = clamp(uProgress, 0.0, 1.0) * float(frames.y - 1);
  }
  int f0 = int(pos);
  int f1 = period > 0.0 ? (f0 + 1) % frames.y : min(f0 + 1, frames.y - 1);

  // Map the cell into clip space. Gradients come from the continuous cellF so
  // mip selection doesn't spike at cell borders.
  vec2 uv = turn(fract(cellF) - 0.5, r);
  vec2 gx = turn(dFdx(cellF), r);
  vec2 gy = turn(dFdy(cellF), r);
  vec2 flip = vec2(mirror ? -1.0 : 1.0, -1.0); // atlas rows run top-down
  uv = uv * flip + 0.5;
  gx *= flip;
  gy *= flip;

  vec4 a = textureGrad(uAtlas, vec3(uv, float(frames.x + f0)), gx, gy);
  vec4 b = textureGrad(uAtlas, vec3(uv, float(frames.x + f1)), gx, gy);
  vec3 c = mix(a.rgb, b.rgb, fract(pos));

  if (uGridLines) {
    vec2 g = fract(cellF);
    float edge = step(g.x, 1.0 / uCellPx) + step(g.y, 1.0 / uCellPx);
    c *= 1.0 - 0.35 * min(edge, 1.0);
  }

  outColor = vec4(c, 1.0);
}
`;
