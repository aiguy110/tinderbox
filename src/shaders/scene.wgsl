// Procedural scene renderer: everything is drawn here, with no art assets.
//
// The view is oblique: the ground is seen from above, and pines stand up on
// it, drawn in profile and overlapping the trees behind them. Each cell owns
// an n×n jittered grid of tree slots (view.trees per side). Trees are
// evaluated per pixel from a fixed window of nearby slots, back to front,
// and every tree's shape, shadow and flames stay inside that window, so the
// picture is a continuous function of world position and the cell states.
// That makes it seamless with no tiles, and across monitors.
//
// A tree's timeline comes from its cell (see fire_age):
//   grow     two steps (causes GROW, MATURE), from a sprout to full size
//   catch    each tree ignites when the front reaches it, so a stand catches
//            in order from the side the fire came from (arrival())
//   burn     flames climb the crown, needles scorch and burn away
//   burnt    flames die out tree by tree, leaving a charred snag with embers
//   cleared  snags stay standing for a few steps, then tip and crumble
// Ground is a blend of grass, dirt, needle litter and ash, interpolated
// between cell centers and thresholded by noise, so it changes in patches.
// Smoke, sparks and fire light come from a bilinear heat field over cells.

var<private> s: f32;      // progress through the step
var<private> px: f32;     // one target pixel, in cells
var<private> wind: f32;   // to the right; gusts slowly
var<private> time: f32;

// Steps since the cell caught fire, including progress through this step:
// 0-1 catching, 1-2 burning, 2-3 burnt, then 3 + ground age. -1 if it never
// burned.
fn fire_age(t: vec4<u32>) -> f32 {
  if (t.r == FIRE) { return select(1.0, 0.0, t.g == TREE) + s; }
  if (t.r == BURNT) { return 2.0 + s; }
  let ga = ground_age(t, s);
  return select(-1.0, 3.0 + ga, ga < 1000.0);
}

// How grown a cell's trees are: 0 to 1 over the two steps after sprouting.
fn growth(t: vec4<u32>) -> f32 {
  if (t.r != TREE) { return select(1.0, 0.0, t.r == EMPTY); }
  if (t.b == CAUSE_GROW) { return 0.5 * s; }
  if (t.b == CAUSE_MATURE) { return 0.5 + 0.5 * s; }
  return 1.0;
}

// How brightly a cell burns overall, and how much it smokes.
fn heat(t: vec4<u32>) -> vec2<f32> {
  let f = fire_age(t);
  if (f < 0.0) { return vec2<f32>(0.0); }
  return vec2<f32>(smoothstep(0.0, 0.8, f) * (1.0 - smoothstep(2.0, 2.9, f)),
                   smoothstep(0.1, 0.9, f) * (1.0 - smoothstep(2.6, 4.5, f)));
}

fn heat_at(p: vec2<f32>) -> vec2<f32> {
  let q = p - 0.5;
  let v = vec2<i32>(floor(q));
  var u = q - vec2<f32>(v);
  u = u * u * (3.0 - 2.0 * u);
  return mix(mix(heat(cell_at(v)), heat(cell_at(v + vec2<i32>(1, 0))), u.x),
             mix(heat(cell_at(v + vec2<i32>(0, 1))), heat(cell_at(v + vec2<i32>(1, 1))), u.x), u.y);
}

// ---- Ground ----

// Continuous amounts, interpolated between cells before being thresholded.
struct Ground {
  grass: f32,   // regrowth, 0 to 1
  litter: f32,  // needle litter under grown forest
  ash: f32,
  ember: f32,
}

// Ash washes into the dirt in patches over a few steps after the clear.
fn ash_left(p: vec2<f32>, f: f32) -> f32 {
  let n = fbm(p * 3.0, 3u);
  return 1.0 - smoothstep(n - 0.1, n + 0.1, (f - 3.0) / 3.0 * 1.3 - 0.15);
}

fn ground_of(t: vec4<u32>, c: vec2<i32>, p: vec2<f32>) -> Ground {
  let f = fire_age(t);
  let r = regrowth(t, s);
  var ember = 0.0;
  var ash = 0.0;
  if (f >= 0.0) {
    ember = 1.0 - smoothstep(2.2, 3.8, f);
    ash = ash_left(p, f);
  }
  if (t.r == TREE) { return Ground(r, growth(t), ash, 0.0); }
  if (t.r == EMPTY) { return Ground(r, 0.0, ash, ember); }
  if (t.r == FIRE && t.g == TREE) {
    // Litter burns to ash behind the front.
    let a = arrival(clamp(p - vec2<f32>(c), vec2<f32>(0.0), vec2<f32>(1.0)), t, c) + (fbm(p * 2.5, 2u) - 0.5) * 0.3;
    let b = smoothstep(a, a + 0.25, s * 1.45 - 0.05);
    return Ground(1.0, 1.0 - b, b, b);
  }
  return Ground(1.0, 0.0, 1.0, ember);
}

struct Shade {
  color: vec3<f32>,
  glow: vec3<f32>,
}

fn ground_color(g: Ground, p: vec2<f32>) -> Shade {
  let tex = vnoise(p * 34.0, 80u);
  let dirt = mix(vec3<f32>(0.30, 0.22, 0.14), vec3<f32>(0.40, 0.30, 0.19), fbm(p * 3.0, 81u)) * (0.88 + 0.24 * tex);
  var grass = mix(vec3<f32>(0.19, 0.36, 0.09), vec3<f32>(0.33, 0.52, 0.14), fbm(p * 2.0, 82u));
  grass *= 0.78 + 0.38 * vnoise(p * vec2<f32>(60.0, 30.0), 83u);
  let litter = mix(vec3<f32>(0.13, 0.11, 0.06), vec3<f32>(0.22, 0.17, 0.09), vnoise(p * 14.0, 84u)) * (0.85 + 0.3 * tex);
  let ash = mix(vec3<f32>(0.06, 0.055, 0.05), vec3<f32>(0.24, 0.23, 0.22), smoothstep(0.5, 0.75, fbm(p * 9.0, 85u)));
  let n = fbm(p * 2.5, 86u);
  var c = mix(dirt, grass, grass_at(p, g.grass));
  c = mix(c, litter, smoothstep(n - 0.15, n + 0.15, g.litter * 1.3 - 0.15));
  let a = smoothstep(n - 0.12, n + 0.12, g.ash * 1.24 - 0.12);
  c = mix(c, ash, a);
  let spots = smoothstep(0.68, 0.85, vnoise(p * 14.0, 87u)) * (0.4 + 0.6 * vnoise(p * 9.0 + vec2<f32>(0.0, 1.5 * time), 88u));
  return Shade(c, g.ember * a * spots * vec3<f32>(1.0, 0.32, 0.05) * 1.5);
}

// ---- Trees ----

struct Tree {
  base: vec2<f32>,  // foot of the trunk, in cells
  h: f32,           // full-grown height, in cells
  seed: f32,
  grow: f32,        // size of the live tree, 0 if there isn't one
  burnt: f32,       // how burnt it is
  flame: f32,
  climb: f32,       // flame height: flares up the crown, then dies back
  ember: f32,
  fall: f32,        // a snag left by an earlier fire: 0 standing to 1 gone; -1 if none
  flash: f32,       // lightning
}

fn tree_at(k: vec2<i32>, n: f32) -> Tree {
  var tr = Tree(vec2<f32>(0.0), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0);
  if (hashf(k, 24u) > 0.9) { return tr; }
  let c = vec2<i32>(floor((vec2<f32>(k) + 0.5) / n));
  tr.seed = hashf(k, 20u);
  tr.base = (vec2<f32>(k) + vec2<f32>(0.15 + 0.7 * hashf(k, 21u), 0.3 + 0.4 * hashf(k, 22u))) / n;
  tr.h = (1.55 + 0.35 * hashf(k, 23u)) / n;
  let t = cell_at(c);
  let f = fire_age(t);
  tr.grow = growth(t);
  if (f < 0.0) { return tr; }
  // When the front reaches this tree. Past the catching step everything
  // that depends on it has saturated, so the burnt step can ignore it.
  var ign = 0.0;
  if (t.r == FIRE) {
    let arr = arrival(clamp(tr.base - vec2<f32>(c), vec2<f32>(0.0), vec2<f32>(1.0)), t, c);
    ign = clamp(0.85 * arr + 0.15 * hashf(k, 25u), 0.0, 1.0);
    if (t.g == TREE && t.a == 0u) {
      tr.flash = max(0.0, 1.0 - s / 0.15) * (1.0 - smoothstep(0.05, 0.25, arr));
    }
  }
  let tau = f - ign;
  tr.ember = smoothstep(0.4, 1.0, tau) * (1.0 - smoothstep(2.6, 4.2 + tr.seed, f));
  if (t.r == FIRE || t.r == BURNT) {
    tr.burnt = smoothstep(0.05, 1.0, tau);
    tr.flame = (0.75 + 0.25 * hashf(k, 27u)) * smoothstep(0.0, 0.12, tau)
      * (1.0 - smoothstep(2.2 + 0.4 * tr.seed, 2.6 + 0.35 * tr.seed, f));
    tr.climb = smoothstep(0.0, 0.35, tau) * (1.0 - 0.55 * smoothstep(0.5, 1.6, tau));
  } else {
    let fl = 4.6 + 7.0 * hashf(k, 26u);
    tr.fall = select(-1.0, smoothstep(fl - 0.7, fl, f), f < fl);
  }
  return tr;
}

fn shadow_of(tr: Tree, p: vec2<f32>, size: f32) -> f32 {
  let h = tr.h * size;
  let d = (p - tr.base - vec2<f32>(0.18, -0.02) * h) / (vec2<f32>(0.32, 0.11) * h);
  return 1.0 - smoothstep(0.5, 1.0, length(d));
}

fn fire_ramp(x: f32) -> vec3<f32> {
  var c = mix(vec3<f32>(0.7, 0.12, 0.0), vec3<f32>(1.0, 0.38, 0.03), smoothstep(0.0, 0.4, x));
  c = mix(c, vec3<f32>(1.0, 0.75, 0.25), smoothstep(0.35, 0.7, x));
  return mix(c, vec3<f32>(1.0, 0.97, 0.85), smoothstep(0.7, 1.0, x));
}

// Composite one pine over c: a live tree (fall 0) or a snag (burnt 1).
// Surfaces are lit by lit; flames and embers glow on their own.
fn draw_pine(c0: vec3<f32>, tr: Tree, p: vec2<f32>, grow: f32, burnt: f32, flame: f32, fall: f32, lit: vec3<f32>) -> vec3<f32> {
  var c = c0;
  let h = tr.h * grow;
  if (h < 0.5 * px) { return c; }
  var q = (p - tr.base) / h;
  if (q.y < -0.05 || q.y > 1.65 || abs(q.x) > 0.7) { return c; }
  // Sway; tip over and sink as a snag falls.
  q.x -= (0.015 + 0.02 * wind) * sin(time * 1.3 + tr.seed * 6.283 + tr.base.x * 1.7) * q.y * q.y * (1.0 - 0.6 * burnt);
  if (fall > 0.0) {
    let ang = 0.35 * fall * select(-1.0, 1.0, tr.seed > 0.5);
    q = mat2x2<f32>(cos(ang), -sin(ang), sin(ang), cos(ang)) * q;
    q.y += 0.2 * fall;
  }
  let u = q.x;
  let v = q.y;
  let aa = 1.2 * px / h;

  let tiers = 4.0 + floor(tr.seed * 3.0);
  let vv = (v - 0.12) / 0.88;
  let env = 0.29 * max(1.0 - vv, 0.0);
  let tt = fract(vv * tiers + abs(u) * 2.0);
  let w = env * (0.6 + 0.4 * (1.0 - tt)) * (0.85 + 0.3 * vnoise(q * 38.0 + tr.seed * 91.0, 30u));
  var crown = step(0.0, vv) * (1.0 - smoothstep(w - aa, w + aa, abs(u)));
  // Needles burn away, leaving the snag's trunk and branches.
  let needles = vnoise(q * 16.0 + tr.seed * 37.0, 31u);
  crown *= smoothstep(burnt * 1.15 - 0.15, burnt * 1.15 - 0.05, needles);
  let top = mix(0.3, 0.95, burnt);
  let tw = 0.032 * (1.0 - 0.75 * v / top) + 0.004;
  let trunk = (1.0 - smoothstep(tw - aa, tw + aa, abs(u))) * step(0.0, v) * (1.0 - smoothstep(top - aa, top + aa, v));
  let tb = fract(vv * tiers + abs(u) * 2.6);
  let bd = min(tb, 1.0 - tb) * 0.88 / tiers;
  let branch = (1.0 - smoothstep(0.008, 0.008 + aa, bd)) * (1.0 - smoothstep(env * 0.8, env * 0.85 + 1e-4, abs(u)))
    * step(0.0, vv) * smoothstep(0.3, 0.9, burnt);
  var wood = max(trunk, branch);
  if (fall > 0.0) {
    let bits = smoothstep(fall * 1.3 - 0.3, fall * 1.3 - 0.2, vnoise(q * 9.0 + tr.seed * 7.0, 32u));
    wood *= bits;
    crown *= bits;
  }

  let charred = vec3<f32>(0.045, 0.038, 0.032);
  let bark = mix(vec3<f32>(0.24, 0.15, 0.08), charred, smoothstep(0.2, 0.7, burnt));
  c = mix(c, bark * lit * select(0.7, 1.0, u < 0.0), wood);

  var green = mix(vec3<f32>(0.05, 0.16, 0.07), vec3<f32>(0.10, 0.25, 0.09), tr.seed);
  green = mix(vec3<f32>(0.20, 0.40, 0.11), green, smoothstep(0.3, 1.0, grow));
  let side = clamp(-u / max(env, 1e-3), -1.0, 1.0);
  var shade = (0.8 + 0.28 * side) * (0.7 + 0.45 * (1.0 - smoothstep(0.3, 1.0, tt))) * (0.75 + 0.25 * smoothstep(0.0, 0.5, v));
  shade *= 0.8 + 0.4 * vnoise(q * 70.0 + tr.seed * 13.0, 33u);
  var leaf = mix(green, vec3<f32>(0.40, 0.19, 0.05), smoothstep(0.0, 0.45, burnt));
  leaf = mix(leaf, charred, smoothstep(0.35, 0.8, burnt));
  c = mix(c, leaf * shade * lit, crown);

  let body = max(wood, crown);
  let spots = smoothstep(0.62, 0.85, vnoise(q * 20.0 + tr.seed * 50.0, 34u)) * (0.4 + 0.6 * vnoise(q * 7.0 + vec2<f32>(0.0, 1.3 * time), 35u));
  c += tr.ember * burnt * spots * body * vec3<f32>(1.0, 0.3, 0.04) * 1.6;
  c = mix(c, vec3<f32>(0.9, 0.95, 1.0), tr.flash * (1.0 - fall) * body);

  if (flame > 0.0) {
    // Flames lick up the crown in tongues, leaning with the wind and
    // pulsing. They stay within 1.6 tree heights (the slot window).
    let fh = (0.35 + 0.85 * tr.climb) * (0.85 + 0.3 * vnoise(vec2<f32>(time * 1.7, tr.seed * 50.0), 42u));
    let y = v / fh;
    if (y > -0.1 && y < 1.15) {
      let x = u - (0.06 + 0.08 * wind) * y * y * fh;
      let n = fbm(vec2<f32>(x * 7.0 + tr.seed * 17.0, v * 3.5 - time * 2.6), 40u);
      let tongues = vnoise(vec2<f32>(x * 9.0 + tr.seed * 3.0, v * 2.0 - time * 4.0), 41u);
      let wf = 0.3 * pow(max(1.0 - 0.8 * y, 0.0), 0.7) + 0.015;
      let d = abs(x) / wf + (n - 0.5) * 1.2 + (tongues - 0.5) * 0.9 * y;
      let core = clamp(1.0 - d, 0.0, 1.0) * (1.0 - smoothstep(0.5, 1.3, y + (n - 0.5) * 0.7))
        * (1.0 - smoothstep(0.8, 1.15, y)) * smoothstep(-0.1, 0.15, y + (n - 0.5) * 0.4);
      let alpha = smoothstep(0.03, 0.35, core) * flame;
      let fc = fire_ramp(core * (1.4 - 0.7 * y) * (0.6 + 0.4 * flame));
      c = c * (1.0 - 0.75 * alpha) + fc * 1.25 * alpha;
    }
  }
  return c;
}

// ---- Sky effects ----

// Sparks: a lattice of particles drifting up with the wind. Each lights up
// if there was fire where it came from, and fades as it rises.
fn sparks(p: vec2<f32>) -> vec3<f32> {
  var acc = vec3<f32>(0.0);
  for (var l = 0; l < 2; l++) {
    let fl = f32(l);
    let sz = 0.3 + 0.15 * fl;
    let drift = vec2<f32>(0.35 * wind, 0.8 + 0.3 * fl);
    let q = (p - drift * time) / sz;
    let id = vec2<i32>(floor(q));
    let salt = 90u + 8u * u32(l);
    if (hashf(id, salt) > 0.5) { continue; }
    let hs = hashf(id, salt + 1u);
    let o = 0.25 + 0.5 * vec2<f32>(hashf(id, salt + 2u), hashf(id, salt + 3u));
    let period = 1.4 + hs;
    let life = fract(time / period + hs * 7.0);
    var here = (vec2<f32>(id) + o) * sz + drift * time;
    here.x += 0.05 * sz * sin(time * 5.0 + hs * 30.0);
    let b = heat_at(here - drift * life * period).x * (1.0 - life) * (0.6 + 0.4 * sin(time * 17.0 + hs * 40.0));
    let r = max(1.2 * px, 0.02);
    acc += b * (1.0 - smoothstep(0.0, r, length(p - here))) * vec3<f32>(1.0, 0.55, 0.15) * 2.0;
  }
  return acc;
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  grid_size = vec2<i32>(textureDimensions(state));
  let p = world(pos);
  px = view.scale.x / view.cell_px;
  s = clamp(view.progress, 0.0, 1.0);
  time = view.time;
  wind = 0.6 + 0.3 * sin(time * 0.11) + 0.15 * sin(time * 0.37 + 1.0);
  let n = f32(view.trees);
  let night = view.night != 0u;

  // Light: daylight or moonlight, plus fire nearby.
  let h0 = heat_at(p);
  var glow_amt = 0.6 * h0.x + 0.2 * (heat_at(p + vec2<f32>(0.9, 0.3)).x + heat_at(p - vec2<f32>(0.9, -0.3)).x);
  glow_amt *= 0.85 + 0.3 * vnoise(p * 1.3 + vec2<f32>(0.0, 2.0 * time), 50u);
  let fire_light = vec3<f32>(1.0, 0.5, 0.18) * glow_amt * select(0.8, 2.2, night);
  let lit = select(vec3<f32>(1.0), vec3<f32>(0.13, 0.16, 0.27), night) + fire_light;

  // Trees in a 3×4 window of slots: theirs are the only shapes, shadows
  // and flames that can reach this pixel. Rows run back to front.
  let kp = vec2<i32>(floor(p * n));
  var trees: array<Tree, 12>;
  var shadow = 0.0;
  for (var i = 0; i < 12; i++) {
    let tr = tree_at(kp + vec2<i32>(i % 3 - 1, -(i / 3)), n);
    trees[i] = tr;
    if (tr.grow > 0.0) { shadow = max(shadow, shadow_of(tr, p, tr.grow) * (1.0 - 0.75 * tr.burnt)); }
    if (tr.fall >= 0.0) { shadow = max(shadow, 0.25 * (1.0 - tr.fall) * shadow_of(tr, p, 1.0)); }
  }

  // Ground, interpolated between cell centers through a warped lattice.
  let pw = p + (vec2<f32>(fbm(p * 1.7, 70u), fbm(p * 1.7, 71u)) - 0.5) * 0.5;
  let v = vec2<i32>(floor(pw - 0.5));
  let f = pw - 0.5 - vec2<f32>(v);
  let wt = vec4<f32>((1.0 - f.x) * (1.0 - f.y), f.x * (1.0 - f.y), (1.0 - f.x) * f.y, f.x * f.y);
  var g = Ground(0.0, 0.0, 0.0, 0.0);
  for (var i = 0; i < 4; i++) {
    let c = v + vec2<i32>(i & 1, i >> 1u);
    let gi = ground_of(cell_at(c), c, p);
    g.grass += wt[i] * gi.grass;
    g.litter += wt[i] * gi.litter;
    g.ash += wt[i] * gi.ash;
    g.ember += wt[i] * gi.ember;
  }
  let ground = ground_color(g, p);
  var c = ground.color * (1.0 - select(0.45, 0.15, night) * shadow) * lit + ground.glow;

  for (var i = 0; i < 12; i++) {
    let tr = trees[i];
    if (tr.fall >= 0.0) { c = draw_pine(c, tr, p, 1.0, 1.0, 0.0, tr.fall, lit); }
    if (tr.grow > 0.0) { c = draw_pine(c, tr, p, tr.grow, tr.burnt, tr.flame, 0.0, lit); }
  }

  // Smoke rises from fire below and drifts with the wind.
  var sm = 0.0;
  for (var k = 0; k < 4; k++) {
    var hk = h0;
    if (k > 0) { hk = heat_at(p - vec2<f32>(0.25 * wind, 0.5) * f32(k)); }
    sm += hk.y * (1.0 - 0.18 * f32(k));
  }
  sm = clamp(0.35 * sm, 0.0, 1.0);
  let dens = sm * smoothstep(0.3, 0.8, fbm(p * 1.4 - vec2<f32>(0.3 * wind, 0.6) * time, 60u) + 0.25 * sm);
  let smoke_c = select(vec3<f32>(0.42, 0.41, 0.40), vec3<f32>(0.08, 0.08, 0.1), night) + 0.3 * fire_light;
  c = mix(c, smoke_c, 0.65 * dens);
  c += sparks(p);

  c *= grid_line(p, px);
  return vec4<f32>(c, 1.0);
}
