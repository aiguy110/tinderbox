import { LOOKS } from './shaders';

/** Upload an atlas as a mipmapped RGBA8 texture array. */
export function uploadAtlas(gl: WebGL2RenderingContext, atlas: { tile: number; layers: number; pixels: Uint8Array }): WebGLTexture {
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

function rng(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let x = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    x ^= x + Math.imul(x ^ (x >>> 7), 61 | x);
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

// --- Dual-grid stills ---------------------------------------------------------
//
// `atlas/stills.json` + `stills.png` (written by tools/atlas/stills.py): one
// vertex tile per (SW, SE, NW, NE) combination of the looks, laid out
// row-major in a K²×K² grid so that grid index = SW + K·SE + K²·NW + K³·NE.
// See docs/PLAN.md.

export interface StillsManifest {
  version: 1;
  tile: number;
  looks: string[];
}

export interface TileAtlas {
  tile: number;
  layers: number;
  pixels: Uint8Array;
  source: 'generated' | 'placeholder';
}

const K = LOOKS.length;
const CONFIGS = K ** 4;
const [GRASS, FIRE, BURNT, DIRT] = [0, 2, 3, 4];

export async function loadTiles(): Promise<TileAtlas> {
  const base = `${import.meta.env.BASE_URL}atlas/`;
  try {
    const res = await fetch(`${base}stills.json`);
    if (!res.ok) throw new Error(res.statusText);
    const { tile, looks }: StillsManifest = await res.json();
    if (looks.join() !== LOOKS.join()) throw new Error(`atlas looks ${looks} don't match the renderer's`);
    const img = new Image();
    img.src = `${base}stills.png`;
    await img.decode();
    const ctx = new OffscreenCanvas(img.width, img.height).getContext('2d')!;
    ctx.drawImage(img, 0, 0);
    const pixels = new Uint8Array(CONFIGS * tile * tile * 4);
    for (let i = 0; i < CONFIGS; i++) {
      const data = ctx.getImageData((i % (K * K)) * tile, Math.floor(i / (K * K)) * tile, tile, tile).data;
      pixels.set(data, i * tile * tile * 4);
    }
    return { tile, layers: CONFIGS, pixels, source: 'generated' };
  } catch (err) {
    console.warn('stills atlas unavailable, using placeholder:', err);
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
  const pixels = new Uint8Array(CONFIGS * T * T * 4);
  // Corners in tile space (y down): SW, SE, NW, NE.
  const corners: [number, number][] = [
    [0, T],
    [T, T],
    [0, 0],
    [T, 0],
  ];
  for (let config = 0; config < CONFIGS; config++) {
    const looks = [0, 1, 2, 3].map((i) => Math.floor(config / K ** i) % K);
    const each = (fn: (x: number, y: number, s: number) => void) => corners.forEach(([x, y], i) => fn(x, y, looks[i]));
    tileGround(g, T);
    // Fixed draw order by layer, never by cell, so overlaps resolve the same
    // way in every tile.
    each((x, y, s) => (s === DIRT || s === BURNT) && tileDirt(g, x, y, T));
    each((x, y, s) => s === FIRE && tileGlow(g, x, y, T));
    each((x, y, s) => s !== GRASS && s !== DIRT && tileCanopy(g, x, y, T, s === BURNT));
    each((x, y, s) => s === FIRE && tileFlames(g, x, y, T));
    pixels.set(g.getImageData(0, 0, T, T).data, config * T * T * 4);
  }
  return { tile: T, layers: CONFIGS, pixels, source: 'placeholder' };
}

function tileGround(g: OffscreenCanvasRenderingContext2D, T: number) {
  g.globalCompositeOperation = 'source-over';
  g.fillStyle = '#4e7a2a';
  g.fillRect(0, 0, T, T);
  // Speckles drawn with wraparound, so the ground tiles with itself.
  const r = rng(5);
  for (let i = 0; i < 30; i++) {
    const x = r() * T;
    const y = r() * T;
    g.fillStyle = r() < 0.5 ? '#5a8a30' : '#3f6a22';
    for (const dx of [-T, 0, T]) for (const dy of [-T, 0, T]) g.fillRect(x + dx, y + dy, 2, 2);
  }
}

function tileDirt(g: OffscreenCanvasRenderingContext2D, cx: number, cy: number, T: number) {
  // The cell's own quarter of the tile.
  g.fillStyle = '#33261a';
  g.fillRect(cx - T / 2, cy - T / 2, T, T);
  const r = rng(17);
  g.fillStyle = 'rgba(90,84,78,0.8)';
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
