// Procedural scene renderer: everything is drawn in GLSL, with no atlas.
//
// The view is oblique: the ground is seen from above, and pines stand up on
// it, drawn in profile and overlapping the trees behind them. Each cell owns
// an n×n jittered grid of tree slots (uTrees per side). Trees are evaluated
// per pixel from a fixed window of nearby slots, back to front, and every
// tree's shape, shadow and flames stay inside that window, so the picture is
// a continuous function of world position and the cell states. That makes it
// seamless with no tiles.
//
// A tree's timeline comes from its cell (see fireAge):
//   grow     two steps (causes GROW, MATURE), from a sprout to full size
//   catch    each tree ignites when the front reaches it, so a stand catches
//            in order from the side the fire came from (arrival())
//   burn     flames climb the crown, needles scorch and burn away
//   burnt    flames die out tree by tree, leaving a charred snag with embers
//   cleared  snags stay standing for a few steps, then tip and crumble
// Ground is a blend of grass, dirt, needle litter and ash, interpolated
// between cell centers and thresholded by noise, so it changes in patches.
// Smoke, sparks and fire light come from a bilinear heat field over cells.

import { arrival, common, noise } from './shaders';

export const sceneFrag = /* glsl */ `${common}
uniform usampler2D uState;
uniform float uCellPx;
uniform bool uGridLines;
uniform bool uWrap;
uniform float uProgress;
uniform float uTime;
uniform int uTrees;
uniform bool uNight;

out vec4 outColor;

ivec2 gridSize;
float s;     // progress through the step
float px;    // one screen pixel, in cells
float wind;  // to the right; gusts slowly

uvec4 cellAt(ivec2 c) {
  if (uWrap) {
    c = (c + gridSize) % gridSize;
  } else if (any(lessThan(c, ivec2(0))) || any(greaterThanEqual(c, gridSize))) {
    return uvec4(EMPTY, EMPTY, CAUSE_NONE, SETTLED);
  }
  return texelFetch(uState, c, 0);
}

${noise}
${arrival}

// Steps since the cell caught fire, including progress through this step:
// 0-1 catching, 1-2 burning, 2-3 burnt, then 3 + ground age. -1 if it never
// burned.
float fireAge(uvec4 t) {
  if (t.r == FIRE) return (t.g == TREE ? 0.0 : 1.0) + s;
  if (t.r == BURNT) return 2.0 + s;
  float ga = groundAge(t, s);
  return ga < 1000.0 ? 3.0 + ga : -1.0;
}

// How grown a cell's trees are: 0 to 1 over the two steps after sprouting.
float growth(uvec4 t) {
  if (t.r != TREE) return t.r == EMPTY ? 0.0 : 1.0;
  if (t.b == CAUSE_GROW) return 0.5 * s;
  if (t.b == CAUSE_MATURE) return 0.5 + 0.5 * s;
  return 1.0;
}

// How brightly a cell burns overall, and how much it smokes.
vec2 heat(uvec4 t) {
  float f = fireAge(t);
  if (f < 0.0) return vec2(0.0);
  return vec2(smoothstep(0.0, 0.8, f) * (1.0 - smoothstep(2.0, 2.9, f)),
              smoothstep(0.1, 0.9, f) * (1.0 - smoothstep(2.6, 4.5, f)));
}

vec2 heatAt(vec2 p) {
  vec2 q = p - 0.5;
  ivec2 v = ivec2(floor(q));
  vec2 u = q - vec2(v);
  u = u * u * (3.0 - 2.0 * u);
  return mix(mix(heat(cellAt(v)), heat(cellAt(v + ivec2(1, 0))), u.x),
             mix(heat(cellAt(v + ivec2(0, 1))), heat(cellAt(v + ivec2(1, 1))), u.x), u.y);
}

// ---- Ground ----

// Continuous amounts, interpolated between cells before being thresholded.
struct Ground {
  float grass;   // regrowth, 0 to 1
  float litter;  // needle litter under grown forest
  float ash;
  float ember;
};

// Ash washes into the dirt in patches over a few steps after the clear.
float ashLeft(vec2 p, float f) {
  float n = fbm(p * 3.0, 3u);
  return 1.0 - smoothstep(n - 0.1, n + 0.1, (f - 3.0) / 3.0 * 1.3 - 0.15);
}

Ground groundOf(uvec4 t, ivec2 c, vec2 p) {
  float f = fireAge(t);
  float r = regrowth(t, s);
  float ember = f < 0.0 ? 0.0 : 1.0 - smoothstep(2.2, 3.8, f);
  if (t.r == TREE) return Ground(r, growth(t), f < 0.0 ? 0.0 : ashLeft(p, f), 0.0);
  if (t.r == EMPTY) return Ground(r, 0.0, f < 0.0 ? 0.0 : ashLeft(p, f), ember);
  if (t.r == FIRE && t.g == TREE) {
    // Litter burns to ash behind the front.
    float a = arrival(clamp(p - vec2(c), 0.0, 1.0), t, c) + (fbm(p * 2.5, 2u) - 0.5) * 0.3;
    float b = smoothstep(a, a + 0.25, s * 1.45 - 0.05);
    return Ground(1.0, 1.0 - b, b, b);
  }
  return Ground(1.0, 0.0, 1.0, ember);
}

vec3 groundColor(Ground G, vec2 p, out vec3 glow) {
  float tex = vnoise(p * 34.0, 80u);
  vec3 dirt = mix(vec3(0.30, 0.22, 0.14), vec3(0.40, 0.30, 0.19), fbm(p * 3.0, 81u)) * (0.88 + 0.24 * tex);
  vec3 grass = mix(vec3(0.19, 0.36, 0.09), vec3(0.33, 0.52, 0.14), fbm(p * 2.0, 82u));
  grass *= 0.78 + 0.38 * vnoise(p * vec2(60.0, 30.0), 83u);
  vec3 litter = mix(vec3(0.13, 0.11, 0.06), vec3(0.22, 0.17, 0.09), vnoise(p * 14.0, 84u)) * (0.85 + 0.3 * tex);
  vec3 ash = mix(vec3(0.06, 0.055, 0.05), vec3(0.24, 0.23, 0.22), smoothstep(0.5, 0.75, fbm(p * 9.0, 85u)));
  float n = fbm(p * 2.5, 86u);
  vec3 c = mix(dirt, grass, grassAt(p, G.grass));
  c = mix(c, litter, smoothstep(n - 0.15, n + 0.15, G.litter * 1.3 - 0.15));
  float a = smoothstep(n - 0.12, n + 0.12, G.ash * 1.24 - 0.12);
  c = mix(c, ash, a);
  float spots = smoothstep(0.68, 0.85, vnoise(p * 14.0, 87u)) * (0.4 + 0.6 * vnoise(p * 9.0 + vec2(0.0, 1.5 * uTime), 88u));
  glow = G.ember * a * spots * vec3(1.0, 0.32, 0.05) * 1.5;
  return c;
}

// ---- Trees ----

struct Tree {
  vec2 base;    // foot of the trunk, in cells
  float h;      // full-grown height, in cells
  float seed;
  float grow;   // size of the live tree, 0 if there isn't one
  float char;   // how burnt it is
  float flame;
  float climb;  // flame height: flares up the crown, then dies back
  float ember;
  float fall;   // a snag left by an earlier fire: 0 standing to 1 gone; -1 if none
  float flash;  // lightning
};

Tree treeAt(ivec2 k, float n) {
  Tree T = Tree(vec2(0.0), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0);
  if (hashf(k, 24u) > 0.9) return T;
  ivec2 c = ivec2(floor((vec2(k) + 0.5) / n));
  T.seed = hashf(k, 20u);
  T.base = (vec2(k) + vec2(0.15 + 0.7 * hashf(k, 21u), 0.3 + 0.4 * hashf(k, 22u))) / n;
  T.h = (1.55 + 0.35 * hashf(k, 23u)) / n;
  uvec4 t = cellAt(c);
  float f = fireAge(t);
  T.grow = growth(t);
  if (f < 0.0) return T;
  // When the front reaches this tree. Past the catching step everything
  // that depends on it has saturated, so the burnt step can ignore it.
  float ign = 0.0;
  if (t.r == FIRE) {
    float arr = arrival(clamp(T.base - vec2(c), 0.0, 1.0), t, c);
    ign = clamp(0.85 * arr + 0.15 * hashf(k, 25u), 0.0, 1.0);
    if (t.g == TREE && t.a == 0u) T.flash = max(0.0, 1.0 - s / 0.15) * (1.0 - smoothstep(0.05, 0.25, arr));
  }
  float tau = f - ign;
  T.ember = smoothstep(0.4, 1.0, tau) * (1.0 - smoothstep(2.6, 4.2 + T.seed, f));
  if (t.r == FIRE || t.r == BURNT) {
    T.char = smoothstep(0.05, 1.0, tau);
    T.flame = (0.75 + 0.25 * hashf(k, 27u)) * smoothstep(0.0, 0.12, tau) * (1.0 - smoothstep(2.2 + 0.4 * T.seed, 2.6 + 0.35 * T.seed, f));
    T.climb = smoothstep(0.0, 0.35, tau) * (1.0 - 0.55 * smoothstep(0.5, 1.6, tau));
  } else {
    float F = 3.8 + 3.5 * hashf(k, 26u);
    T.fall = f < F ? smoothstep(F - 0.7, F, f) : -1.0;
  }
  return T;
}

float shadowOf(Tree T, vec2 p, float size) {
  float h = T.h * size;
  vec2 d = (p - T.base - vec2(0.18, -0.02) * h) / (vec2(0.32, 0.11) * h);
  return 1.0 - smoothstep(0.5, 1.0, length(d));
}

vec3 fireRamp(float x) {
  vec3 c = mix(vec3(0.7, 0.12, 0.0), vec3(1.0, 0.38, 0.03), smoothstep(0.0, 0.4, x));
  c = mix(c, vec3(1.0, 0.75, 0.25), smoothstep(0.35, 0.7, x));
  return mix(c, vec3(1.0, 0.97, 0.85), smoothstep(0.7, 1.0, x));
}

// Composite one pine over c: a live tree (fall 0) or a snag (char 1).
// Surfaces are lit by lit; flames and embers glow on their own.
void drawPine(inout vec3 c, Tree T, vec2 p, float grow, float char, float flame, float fall, vec3 lit) {
  float h = T.h * grow;
  if (h < 0.5 * px) return;
  vec2 q = (p - T.base) / h;
  if (q.y < -0.05 || q.y > 1.65 || abs(q.x) > 0.7) return;
  // Sway; tip over and sink as a snag falls.
  q.x -= (0.015 + 0.02 * wind) * sin(uTime * 1.3 + T.seed * 6.283 + T.base.x * 1.7) * q.y * q.y * (1.0 - 0.6 * char);
  if (fall > 0.0) {
    float ang = 0.35 * fall * (T.seed > 0.5 ? 1.0 : -1.0);
    q = mat2(cos(ang), -sin(ang), sin(ang), cos(ang)) * q;
    q.y += 0.2 * fall;
  }
  float u = q.x, v = q.y;
  float aa = 1.2 * px / h;

  float tiers = 4.0 + floor(T.seed * 3.0);
  float vv = (v - 0.12) / 0.88;
  float env = 0.29 * max(1.0 - vv, 0.0);
  float tt = fract(vv * tiers + abs(u) * 2.0);
  float w = env * (0.6 + 0.4 * (1.0 - tt)) * (0.85 + 0.3 * vnoise(q * 38.0 + T.seed * 91.0, 30u));
  float crown = step(0.0, vv) * (1.0 - smoothstep(w - aa, w + aa, abs(u)));
  // Needles burn away, leaving the snag's trunk and branches.
  float needles = vnoise(q * 16.0 + T.seed * 37.0, 31u);
  crown *= smoothstep(char * 1.15 - 0.15, char * 1.15 - 0.05, needles);
  float top = mix(0.3, 0.95, char);
  float tw = 0.032 * (1.0 - 0.75 * v / top) + 0.004;
  float trunk = (1.0 - smoothstep(tw - aa, tw + aa, abs(u))) * step(0.0, v) * (1.0 - smoothstep(top - aa, top + aa, v));
  float tb = fract(vv * tiers + abs(u) * 2.6);
  float bd = min(tb, 1.0 - tb) * 0.88 / tiers;
  float branch = (1.0 - smoothstep(0.008, 0.008 + aa, bd)) * (1.0 - smoothstep(env * 0.8, env * 0.85 + 1e-4, abs(u))) * step(0.0, vv) * smoothstep(0.3, 0.9, char);
  float wood = max(trunk, branch);
  if (fall > 0.0) {
    float bits = smoothstep(fall * 1.3 - 0.3, fall * 1.3 - 0.2, vnoise(q * 9.0 + T.seed * 7.0, 32u));
    wood *= bits;
    crown *= bits;
  }

  vec3 charred = vec3(0.045, 0.038, 0.032);
  vec3 bark = mix(vec3(0.24, 0.15, 0.08), charred, smoothstep(0.2, 0.7, char));
  c = mix(c, bark * lit * (u < 0.0 ? 1.0 : 0.7), wood);

  vec3 green = mix(vec3(0.05, 0.16, 0.07), vec3(0.10, 0.25, 0.09), T.seed);
  green = mix(vec3(0.20, 0.40, 0.11), green, smoothstep(0.3, 1.0, grow));
  float side = clamp(-u / max(env, 1e-3), -1.0, 1.0);
  float shade = (0.8 + 0.28 * side) * (0.7 + 0.45 * (1.0 - smoothstep(0.3, 1.0, tt))) * (0.75 + 0.25 * smoothstep(0.0, 0.5, v));
  shade *= 0.8 + 0.4 * vnoise(q * 70.0 + T.seed * 13.0, 33u);
  vec3 leaf = mix(green, vec3(0.40, 0.19, 0.05), smoothstep(0.0, 0.45, char));
  leaf = mix(leaf, charred, smoothstep(0.35, 0.8, char));
  c = mix(c, leaf * shade * lit, crown);

  float body = max(wood, crown);
  float spots = smoothstep(0.62, 0.85, vnoise(q * 20.0 + T.seed * 50.0, 34u)) * (0.4 + 0.6 * vnoise(q * 7.0 + vec2(0.0, 1.3 * uTime), 35u));
  c += T.ember * char * spots * body * vec3(1.0, 0.3, 0.04) * 1.6;
  c = mix(c, vec3(0.9, 0.95, 1.0), T.flash * (1.0 - fall) * body);

  if (flame > 0.0) {
    // Flames lick up the crown in tongues, leaning with the wind and
    // pulsing. They stay within 1.6 tree heights (the slot window).
    float fh = (0.35 + 0.85 * T.climb) * (0.85 + 0.3 * vnoise(vec2(uTime * 1.7, T.seed * 50.0), 42u));
    float y = v / fh;
    if (y > -0.1 && y < 1.15) {
      float x = u - (0.06 + 0.08 * wind) * y * y * fh;
      float n = fbm(vec2(x * 7.0 + T.seed * 17.0, v * 3.5 - uTime * 2.6), 40u);
      float tongues = vnoise(vec2(x * 9.0 + T.seed * 3.0, v * 2.0 - uTime * 4.0), 41u);
      float wf = 0.3 * pow(max(1.0 - 0.8 * y, 0.0), 0.7) + 0.015;
      float d = abs(x) / wf + (n - 0.5) * 1.2 + (tongues - 0.5) * 0.9 * y;
      float core = clamp(1.0 - d, 0.0, 1.0) * (1.0 - smoothstep(0.5, 1.3, y + (n - 0.5) * 0.7)) * (1.0 - smoothstep(0.8, 1.15, y)) * smoothstep(-0.1, 0.15, y + (n - 0.5) * 0.4);
      float alpha = smoothstep(0.03, 0.35, core) * flame;
      vec3 fc = fireRamp(core * (1.4 - 0.7 * y) * (0.6 + 0.4 * flame));
      c = c * (1.0 - 0.75 * alpha) + fc * 1.25 * alpha;
    }
  }
}

// ---- Sky effects ----

// Sparks: a lattice of particles drifting up with the wind. Each lights up
// if there was fire where it came from, and fades as it rises.
vec3 sparks(vec2 p) {
  vec3 acc = vec3(0.0);
  for (int L = 0; L < 2; L++) {
    float fl = float(L);
    float sz = 0.3 + 0.15 * fl;
    vec2 drift = vec2(0.35 * wind, 0.8 + 0.3 * fl);
    vec2 q = (p - drift * uTime) / sz;
    ivec2 id = ivec2(floor(q));
    uint salt = 90u + 8u * uint(L);
    if (hashf(id, salt) > 0.5) continue;
    float hs = hashf(id, salt + 1u);
    vec2 o = 0.25 + 0.5 * vec2(hashf(id, salt + 2u), hashf(id, salt + 3u));
    float period = 1.4 + hs;
    float life = fract(uTime / period + hs * 7.0);
    vec2 here = (vec2(id) + o) * sz + drift * uTime;
    here.x += 0.05 * sz * sin(uTime * 5.0 + hs * 30.0);
    float b = heatAt(here - drift * life * period).x * (1.0 - life) * (0.6 + 0.4 * sin(uTime * 17.0 + hs * 40.0));
    float r = max(1.2 * px, 0.02);
    acc += b * (1.0 - smoothstep(0.0, r, length(p - here))) * vec3(1.0, 0.55, 0.15) * 2.0;
  }
  return acc;
}

void main() {
  gridSize = textureSize(uState, 0);
  vec2 p = gl_FragCoord.xy / uCellPx;
  px = 1.0 / uCellPx;
  s = clamp(uProgress, 0.0, 1.0);
  wind = 0.6 + 0.3 * sin(uTime * 0.11) + 0.15 * sin(uTime * 0.37 + 1.0);
  float n = float(uTrees);

  // Light: daylight or moonlight, plus fire nearby.
  vec2 h0 = heatAt(p);
  float glowAmt = 0.6 * h0.x + 0.2 * (heatAt(p + vec2(0.9, 0.3)).x + heatAt(p - vec2(0.9, -0.3)).x);
  glowAmt *= 0.85 + 0.3 * vnoise(p * 1.3 + vec2(0.0, 2.0 * uTime), 50u);
  vec3 fireLight = vec3(1.0, 0.5, 0.18) * glowAmt * (uNight ? 2.2 : 0.8);
  vec3 lit = (uNight ? vec3(0.13, 0.16, 0.27) : vec3(1.0)) + fireLight;

  // Trees in a 3×4 window of slots: theirs are the only shapes, shadows
  // and flames that can reach this pixel. Rows run back to front.
  ivec2 kp = ivec2(floor(p * n));
  Tree trees[12];
  float shadow = 0.0;
  for (int i = 0; i < 12; i++) {
    Tree T = treeAt(kp + ivec2(i % 3 - 1, -(i / 3)), n);
    trees[i] = T;
    if (T.grow > 0.0) shadow = max(shadow, shadowOf(T, p, T.grow) * (1.0 - 0.75 * T.char));
    if (T.fall >= 0.0) shadow = max(shadow, 0.25 * (1.0 - T.fall) * shadowOf(T, p, 1.0));
  }

  // Ground, interpolated between cell centers through a warped lattice.
  vec2 pw = p + (vec2(fbm(p * 1.7, 70u), fbm(p * 1.7, 71u)) - 0.5) * 0.5;
  ivec2 v = ivec2(floor(pw - 0.5));
  vec2 f = pw - 0.5 - vec2(v);
  vec4 wt = vec4((1.0 - f.x) * (1.0 - f.y), f.x * (1.0 - f.y), (1.0 - f.x) * f.y, f.x * f.y);
  Ground G = Ground(0.0, 0.0, 0.0, 0.0);
  for (int i = 0; i < 4; i++) {
    ivec2 c = v + ivec2(i & 1, i >> 1);
    Ground g = groundOf(cellAt(c), c, p);
    G.grass += wt[i] * g.grass;
    G.litter += wt[i] * g.litter;
    G.ash += wt[i] * g.ash;
    G.ember += wt[i] * g.ember;
  }
  vec3 emit;
  vec3 c = groundColor(G, p, emit) * (1.0 - (uNight ? 0.15 : 0.45) * shadow) * lit + emit;

  for (int i = 0; i < 12; i++) {
    Tree T = trees[i];
    if (T.fall >= 0.0) drawPine(c, T, p, 1.0, 1.0, 0.0, T.fall, lit);
    if (T.grow > 0.0) drawPine(c, T, p, T.grow, T.char, T.flame, 0.0, lit);
  }

  // Smoke rises from fire below and drifts with the wind.
  float sm = 0.0;
  for (int k = 0; k < 4; k++) {
    vec2 hk = k == 0 ? h0 : heatAt(p - vec2(0.25 * wind, 0.5) * float(k));
    sm += hk.y * (1.0 - 0.18 * float(k));
  }
  sm = clamp(0.35 * sm, 0.0, 1.0);
  float dens = sm * smoothstep(0.3, 0.8, fbm(p * 1.4 - vec2(0.3 * wind, 0.6) * uTime, 60u) + 0.25 * sm);
  vec3 smokeC = (uNight ? vec3(0.08, 0.08, 0.1) : vec3(0.42, 0.41, 0.40)) + 0.3 * fireLight;
  c = mix(c, smokeC, 0.65 * dens);
  c += sparks(p);

  if (uGridLines) {
    vec2 g = fract(p);
    float edge = step(g.x, px) + step(g.y, px);
    c *= 1.0 - 0.35 * min(edge, 1.0);
  }
  outColor = vec4(c, 1.0);
}
`;
