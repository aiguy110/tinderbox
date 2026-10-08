// Cell state lives in an RGBA8UI texture, one texel per cell:
//   R = current state, G = previous state, B = cause of the last transition.
// Keeping (prev, cause) alongside the state means a renderer can pick the
// right transition animation per cell without any extra bookkeeping.

export const State = { EMPTY: 0, TREE: 1, FIRE: 2 } as const;
export const Cause = { NONE: 0, GROW: 1, SPREAD: 2, LIGHTNING: 3, BURNOUT: 4 } as const;

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

  if (s == FIRE) {
    next = EMPTY;
    cause = CAUSE_BURNOUT;
  } else if (s == TREE) {
    bool caught =
      burning(p + ivec2(1, 0)) || burning(p + ivec2(-1, 0)) ||
      burning(p + ivec2(0, 1)) || burning(p + ivec2(0, -1));
    if (!caught && uMoore) {
      caught =
        burning(p + ivec2(1, 1)) || burning(p + ivec2(-1, 1)) ||
        burning(p + ivec2(1, -1)) || burning(p + ivec2(-1, -1));
    }
    if (caught) {
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

  outState = uvec4(next, s, cause, 0u);
}
`;

// Phase-one renderer: flat colors per state, with a few transition accents.
// Phase two replaces this with per-transition animation sprites keyed on
// (prev, state, cause) and neighborhood.
export const renderFrag = /* glsl */ `${common}
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
