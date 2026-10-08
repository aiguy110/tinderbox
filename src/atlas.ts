import { CLIPS, type ClipName } from './shaders';

/**
 * `atlas/atlas.json` describes `atlas.png`: one row of `tile`-sized
 * frames per clip. Clips with `seconds` loop over that period; the rest play
 * once per step. Both are written by `tools/atlas` into the data dir outside
 * the repo, and served from there by `vite.config.ts`.
 */
export interface AtlasManifest {
  tile: number;
  clips: Record<ClipName, { row: number; frames: number; seconds?: number }>;
}

export interface Clip {
  /** First layer in the texture array. */
  first: number;
  frames: number;
  /** Loop period, or 0 for clips timed to the step. */
  seconds: number;
}

/** Frames as texture-array layers, `tile * tile * 4` RGBA bytes each. */
export interface Atlas {
  tile: number;
  layers: number;
  pixels: Uint8Array;
  clips: Clip[];
  source: 'generated' | 'placeholder';
}

/** Load the generated atlas, falling back to the procedural placeholder. */
export async function loadAtlas(): Promise<Atlas> {
  const base = `${import.meta.env.BASE_URL}atlas/`;
  try {
    const res = await fetch(`${base}atlas.json`);
    if (!res.ok) throw new Error(res.statusText);
    const manifest: AtlasManifest = await res.json();
    const img = new Image();
    img.src = `${base}atlas.png`;
    await img.decode();
    return fromImage(manifest, img);
  } catch {
    return placeholderAtlas();
  }
}

function fromImage(m: AtlasManifest, img: HTMLImageElement): Atlas {
  const ctx = new OffscreenCanvas(img.width, img.height).getContext('2d')!;
  ctx.drawImage(img, 0, 0);
  const src = ctx.getImageData(0, 0, img.width, img.height).data;
  const { tile } = m;
  const layers = CLIPS.reduce((n, c) => n + m.clips[c].frames, 0);
  const pixels = new Uint8Array(layers * tile * tile * 4);
  const clips: Clip[] = [];
  let layer = 0;
  for (const name of CLIPS) {
    const { row, frames, seconds = 0 } = m.clips[name];
    clips.push({ first: layer, frames, seconds });
    for (let f = 0; f < frames; f++, layer++) {
      for (let y = 0; y < tile; y++) {
        const from = ((row * tile + y) * img.width + f * tile) * 4;
        pixels.set(src.subarray(from, from + tile * 4), (layer * tile + y) * tile * 4);
      }
    }
  }
  return { tile, layers, pixels, clips, source: 'generated' };
}

/** Upload an atlas as a mipmapped RGBA8 texture array. */
export function uploadAtlas(gl: WebGL2RenderingContext, atlas: Pick<Atlas, 'tile' | 'layers' | 'pixels'>): WebGLTexture {
  const tex = gl.createTexture()!;
  gl.bindTexture(gl.TEXTURE_2D_ARRAY, tex);
  gl.texImage3D(gl.TEXTURE_2D_ARRAY, 0, gl.RGBA8, atlas.tile, atlas.tile, atlas.layers, 0, gl.RGBA, gl.UNSIGNED_BYTE, atlas.pixels);
  gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
  gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.generateMipmap(gl.TEXTURE_2D_ARRAY);
  return tex;
}

// --- Procedural placeholder -------------------------------------------------
//
// Hand-drawn stand-ins for the diffusion clips, top-down, in tile space with
// y down (west = left edge, south = bottom edge). Transition clips end on the
// frame the next state's clip starts with, so cells don't pop between steps.

const TILE = 64;
const FRAMES = 12;
const LOOP_SECONDS = 2.5;

type Draw = (g: OffscreenCanvasRenderingContext2D, t: number) => void;

function placeholderAtlas(): Atlas {
  const draws: Record<ClipName, Draw> = {
    empty: (g, t) => ground(g, t),
    tree: (g, t) => {
      ground(g, 0);
      tree(g, 1, Math.sin(t * 2 * Math.PI), 0);
    },
    grow: (g, t) => {
      ground(g, 0);
      tree(g, ease(t), 0, 0);
    },
    'spread-edge': (g, t) => burning(g, t, (x) => x),
    'spread-corner': (g, t) => burning(g, t, (x, y) => (x + (1 - y)) / 2),
    lightning: (g, t) => {
      const flash = Math.max(0, 1 - t / 0.35);
      burning(g, Math.max(0, (t - 0.15) / 0.85), (x, y) => Math.hypot(x - 0.5, y - 0.5) * 1.4);
      if (flash > 0) bolt(g, flash);
    },
    burnout: (g, t) => {
      ground(g, 0);
      ash(g, t);
      tree(g, 1 - 0.35 * t, 0, 1, 1 - t);
      flames(g, 1 - ease(t), () => true, t);
    },
  };

  const g = new OffscreenCanvas(TILE, TILE).getContext('2d', { willReadFrequently: true })!;
  const pixels = new Uint8Array(CLIPS.length * FRAMES * TILE * TILE * 4);
  const clips: Clip[] = [];
  let layer = 0;
  for (const name of CLIPS) {
    const loop = name === 'empty' || name === 'tree';
    clips.push({ first: layer, frames: FRAMES, seconds: loop ? LOOP_SECONDS : 0 });
    for (let f = 0; f < FRAMES; f++, layer++) {
      g.save();
      g.clearRect(0, 0, TILE, TILE);
      draws[name](g, loop ? f / FRAMES : f / (FRAMES - 1));
      g.restore();
      pixels.set(g.getImageData(0, 0, TILE, TILE).data, layer * TILE * TILE * 4);
    }
  }
  return { tile: TILE, layers: layer, pixels, clips, source: 'placeholder' };
}

const ease = (t: number) => t * t * (3 - 2 * t);

function rng(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let x = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    x ^= x + Math.imul(x ^ (x >>> 7), 61 | x);
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

function ground(g: OffscreenCanvasRenderingContext2D, t: number) {
  g.fillStyle = '#2a2116';
  g.fillRect(0, 0, TILE, TILE);
  const r = rng(7);
  for (let i = 0; i < 40; i++) {
    const x = r() * TILE;
    const y = r() * TILE;
    g.fillStyle = r() < 0.5 ? '#3a2e1e' : '#1f1910';
    g.fillRect(x, y, 2, 2);
  }
  // Dry grass tufts that sway a little.
  g.strokeStyle = '#5d5a2a';
  g.lineWidth = 1;
  for (let i = 0; i < 9; i++) {
    const x = r() * TILE;
    const y = r() * TILE;
    const sway = Math.sin(t * 2 * Math.PI + i) * 1.5;
    g.beginPath();
    g.moveTo(x, y);
    g.lineTo(x + sway, y - 4);
    g.stroke();
  }
}

/** A canopy seen from above. `char` blends it toward scorched black. */
function tree(g: OffscreenCanvasRenderingContext2D, scale: number, sway: number, char: number, alpha = 1) {
  if (scale <= 0 || alpha <= 0) return;
  const r = rng(11);
  const cx = TILE / 2 + sway * 1.2;
  const cy = TILE / 2;
  g.globalAlpha = alpha;
  g.fillStyle = 'rgba(0,0,0,0.35)';
  g.beginPath();
  g.arc(cx + 3 * scale, cy + 4 * scale, 22 * scale, 0, 2 * Math.PI);
  g.fill();
  const shades = [
    [24, 78, 30],
    [34, 104, 38],
    [52, 128, 46],
  ];
  for (let layer = 0; layer < 3; layer++) {
    for (let i = 0; i < 7; i++) {
      const a = r() * 2 * Math.PI;
      const d = (1 - layer * 0.35) * 11 * r() * scale;
      const rad = (11 - layer * 2.5) * scale * (0.8 + 0.4 * r());
      const [cr, cg, cb] = shades[layer];
      const k = 1 - 0.85 * char;
      g.fillStyle = `rgb(${cr * k + 20 * char},${cg * k + 12 * char},${cb * k + 8 * char})`;
      g.beginPath();
      g.arc(cx + Math.cos(a) * d + sway * layer, cy + Math.sin(a) * d, rad, 0, 2 * Math.PI);
      g.fill();
    }
  }
  g.globalAlpha = 1;
}

/**
 * A tree catching fire along a front. `front(x, y)` gives, for tile
 * coordinates in [0, 1], how far through the clip that spot ignites.
 */
function burning(g: OffscreenCanvasRenderingContext2D, t: number, front: (x: number, y: number) => number) {
  ground(g, 0);
  tree(g, 1, 0, ease(t));
  flames(g, 1, (x, y) => front(x, y) < t * 1.25 - 0.1, t);
}

function flames(g: OffscreenCanvasRenderingContext2D, intensity: number, lit: (x: number, y: number) => boolean, t: number) {
  if (intensity <= 0) return;
  const r = rng(23);
  g.globalCompositeOperation = 'lighter';
  for (let i = 0; i < 60; i++) {
    const a = r() * 2 * Math.PI;
    const d = Math.sqrt(r()) * 24;
    const x = TILE / 2 + Math.cos(a) * d;
    const y = TILE / 2 + Math.sin(a) * d;
    if (!lit(x / TILE, y / TILE)) continue;
    const flicker = 0.6 + 0.4 * Math.sin(t * 40 + i * 1.7);
    const rad = (3 + 5 * r()) * intensity * flicker;
    const grad = g.createRadialGradient(x, y, 0, x, y, rad);
    grad.addColorStop(0, `rgba(255,230,140,${0.9 * intensity})`);
    grad.addColorStop(0.5, `rgba(255,120,20,${0.6 * intensity})`);
    grad.addColorStop(1, 'rgba(160,30,0,0)');
    g.fillStyle = grad;
    g.beginPath();
    g.arc(x, y, rad, 0, 2 * Math.PI);
    g.fill();
  }
  g.globalCompositeOperation = 'source-over';
}

function ash(g: OffscreenCanvasRenderingContext2D, t: number) {
  // Ash and embers that fade back into bare ground by the end of the clip.
  const fade = t < 0.2 ? t / 0.2 : 1 - ease((t - 0.2) / 0.8);
  const r = rng(31);
  for (let i = 0; i < 30; i++) {
    const a = r() * 2 * Math.PI;
    const d = Math.sqrt(r()) * 22;
    const ember = r() < 0.3;
    g.fillStyle = ember ? `rgba(255,110,30,${fade * (0.5 + 0.5 * Math.sin(t * 30 + i))})` : `rgba(90,85,80,${fade})`;
    g.fillRect(TILE / 2 + Math.cos(a) * d, TILE / 2 + Math.sin(a) * d, 2, 2);
  }
}

function bolt(g: OffscreenCanvasRenderingContext2D, flash: number) {
  g.fillStyle = `rgba(220,235,255,${0.6 * flash})`;
  g.fillRect(0, 0, TILE, TILE);
  g.strokeStyle = `rgba(255,255,255,${flash})`;
  g.lineWidth = 2;
  g.beginPath();
  g.moveTo(TILE * 0.6, 0);
  g.lineTo(TILE * 0.42, TILE * 0.3);
  g.lineTo(TILE * 0.58, TILE * 0.36);
  g.lineTo(TILE * 0.5, TILE * 0.5);
  g.stroke();
}

// --- Dual-grid stills ---------------------------------------------------------
//
// `atlas/tiles.json` + `tiles.png` (written by tools/atlas): 81 vertex tiles,
// one per (SW, SE, NW, NE) combination of {EMPTY, TREE, FIRE}, laid out
// row-major in a 9x9 grid so that grid index = SW + 3·SE + 9·NW + 27·NE.
// See docs/PLAN.md.

export interface TilesManifest {
  version: 1;
  tile: number;
}

export interface TileAtlas {
  tile: number;
  layers: number;
  pixels: Uint8Array;
  source: 'generated' | 'placeholder';
}

const DUAL_CONFIGS = 81;

export async function loadTiles(): Promise<TileAtlas> {
  const base = `${import.meta.env.BASE_URL}atlas/`;
  try {
    const res = await fetch(`${base}tiles.json`);
    if (!res.ok) throw new Error(res.statusText);
    const { tile }: TilesManifest = await res.json();
    const img = new Image();
    img.src = `${base}tiles.png`;
    await img.decode();
    const ctx = new OffscreenCanvas(img.width, img.height).getContext('2d')!;
    ctx.drawImage(img, 0, 0);
    const pixels = new Uint8Array(DUAL_CONFIGS * tile * tile * 4);
    for (let i = 0; i < DUAL_CONFIGS; i++) {
      const data = ctx.getImageData((i % 9) * tile, Math.floor(i / 9) * tile, tile, tile).data;
      pixels.set(data, i * tile * tile * 4);
    }
    return { tile, layers: DUAL_CONFIGS, pixels, source: 'generated' };
  } catch {
    return placeholderTiles();
  }
}

/**
 * Each cell is drawn centered on a tile corner, and nothing a cell draws
 * reaches further than one cell from its center. The four tiles sharing a
 * cell therefore each draw the same pixels for it, so any arrangement is
 * seamless. Fire glow spills into neighboring quarters, which shows the
 * neighbor dependence that rotated per-cell sprites couldn't.
 */
function placeholderTiles(): TileAtlas {
  const T = 64;
  const g = new OffscreenCanvas(T, T).getContext('2d', { willReadFrequently: true })!;
  const pixels = new Uint8Array(DUAL_CONFIGS * T * T * 4);
  // Corners in tile space (y down): SW, SE, NW, NE.
  const corners: [number, number][] = [
    [0, T],
    [T, T],
    [0, 0],
    [T, 0],
  ];
  for (let config = 0; config < DUAL_CONFIGS; config++) {
    const states = [config % 3, Math.floor(config / 3) % 3, Math.floor(config / 9) % 3, Math.floor(config / 27)];
    const each = (fn: (x: number, y: number, s: number) => void) => corners.forEach(([x, y], i) => fn(x, y, states[i]));
    tileGround(g, T);
    // Fixed draw order by layer, never by cell, so overlaps resolve the same
    // way in every tile.
    each((x, y, s) => s === 0 && tileAsh(g, x, y, T));
    each((x, y, s) => s === 2 && tileGlow(g, x, y, T));
    each((x, y, s) => s !== 0 && tileCanopy(g, x, y, T, s === 2));
    each((x, y, s) => s === 2 && tileFlames(g, x, y, T));
    pixels.set(g.getImageData(0, 0, T, T).data, config * T * T * 4);
  }
  return { tile: T, layers: DUAL_CONFIGS, pixels, source: 'placeholder' };
}

function tileGround(g: OffscreenCanvasRenderingContext2D, T: number) {
  g.globalCompositeOperation = 'source-over';
  g.fillStyle = '#2b2217';
  g.fillRect(0, 0, T, T);
  // Speckles drawn with wraparound, so the ground tiles with itself.
  const r = rng(5);
  for (let i = 0; i < 30; i++) {
    const x = r() * T;
    const y = r() * T;
    g.fillStyle = r() < 0.5 ? '#372b1c' : '#211a11';
    for (const dx of [-T, 0, T]) for (const dy of [-T, 0, T]) g.fillRect(x + dx, y + dy, 2, 2);
  }
}

function tileAsh(g: OffscreenCanvasRenderingContext2D, cx: number, cy: number, T: number) {
  const r = rng(17);
  g.fillStyle = 'rgba(70,64,58,0.8)';
  for (let i = 0; i < 18; i++) {
    const a = r() * 2 * Math.PI;
    const d = Math.sqrt(r()) * T * 0.35;
    g.fillRect(cx + Math.cos(a) * d, cy + Math.sin(a) * d, 2, 2);
  }
}

function tileGlow(g: OffscreenCanvasRenderingContext2D, cx: number, cy: number, T: number) {
  g.globalCompositeOperation = 'lighter';
  const grad = g.createRadialGradient(cx, cy, 0, cx, cy, T);
  grad.addColorStop(0, 'rgba(255,110,20,0.55)');
  grad.addColorStop(1, 'rgba(255,60,0,0)');
  g.fillStyle = grad;
  g.fillRect(cx - T, cy - T, 2 * T, 2 * T);
  g.globalCompositeOperation = 'source-over';
}

function tileCanopy(g: OffscreenCanvasRenderingContext2D, cx: number, cy: number, T: number, charred: boolean) {
  const r = rng(11);
  g.fillStyle = 'rgba(0,0,0,0.35)';
  g.beginPath();
  g.arc(cx + T * 0.05, cy + T * 0.07, T * 0.36, 0, 2 * Math.PI);
  g.fill();
  const shades = charred ? ['#1d140e', '#2a1a10', '#3a2312'] : ['#184e1e', '#226826', '#34802e'];
  for (let layer = 0; layer < 3; layer++) {
    g.fillStyle = shades[layer];
    for (let i = 0; i < 7; i++) {
      const a = r() * 2 * Math.PI;
      const d = (1 - layer * 0.35) * T * 0.17 * r();
      const rad = T * (0.17 - layer * 0.04) * (0.8 + 0.4 * r());
      g.beginPath();
      g.arc(cx + Math.cos(a) * d, cy + Math.sin(a) * d, rad, 0, 2 * Math.PI);
      g.fill();
    }
  }
}

function tileFlames(g: OffscreenCanvasRenderingContext2D, cx: number, cy: number, T: number) {
  const r = rng(23);
  g.globalCompositeOperation = 'lighter';
  for (let i = 0; i < 40; i++) {
    const a = r() * 2 * Math.PI;
    const d = Math.sqrt(r()) * T * 0.34;
    const x = cx + Math.cos(a) * d;
    const y = cy + Math.sin(a) * d;
    const rad = T * (0.04 + 0.07 * r());
    const grad = g.createRadialGradient(x, y, 0, x, y, rad);
    grad.addColorStop(0, 'rgba(255,230,140,0.9)');
    grad.addColorStop(0.5, 'rgba(255,120,20,0.6)');
    grad.addColorStop(1, 'rgba(160,30,0,0)');
    g.fillStyle = grad;
    g.beginPath();
    g.arc(x, y, rad, 0, 2 * Math.PI);
    g.fill();
  }
  g.globalCompositeOperation = 'source-over';
}

// --- Dual-grid clips ----------------------------------------------------------
//
// `atlas/clips.json` + `clips-NN.png` (tools/atlas/clips.py): one page per
// frame, each a `columns`-wide grid of the 625 vertex clips, where a cell's
// transition over the step is one of `transitions` and
// index = SW + 5·SE + 25·NW + 125·NE. Each tile is padded by `gutter` px of
// its own edge pixels so filtering never reads a neighbor on the page.

export interface ClipsManifest {
  version: 1;
  tile: number;
  gutter: number;
  frames: number;
  columns: number;
  transitions: string[];
}

export interface ClipPages {
  /** Page side in px. */
  size: number;
  layers: number;
  pixels: Uint8Array;
  manifest: ClipsManifest;
}

export async function loadClips(): Promise<ClipPages | null> {
  const base = `${import.meta.env.BASE_URL}atlas/`;
  try {
    const res = await fetch(`${base}clips.json`);
    if (!res.ok) return null;
    const manifest: ClipsManifest = await res.json();
    const pages = await Promise.all(
      Array.from({ length: manifest.frames }, async (_, f) => {
        const img = new Image();
        img.src = `${base}clips-${String(f).padStart(2, '0')}.png`;
        await img.decode();
        return img;
      }),
    );
    const size = pages[0].width;
    const ctx = new OffscreenCanvas(size, size).getContext('2d', { willReadFrequently: true })!;
    const pixels = new Uint8Array(manifest.frames * size * size * 4);
    pages.forEach((img, f) => {
      ctx.drawImage(img, 0, 0);
      pixels.set(ctx.getImageData(0, 0, size, size).data, f * size * size * 4);
    });
    return { size, layers: manifest.frames, pixels, manifest };
  } catch {
    return null;
  }
}
