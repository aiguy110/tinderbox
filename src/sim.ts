import { type TileAtlas, uploadAtlas } from './atlas';
import { sceneFrag } from './scene';
import { Cause, MAX_STRIKES, SETTLED, hashf, State, dualFrag, flatFrag, fullscreenVert, stepFrag } from './shaders';

export type Renderer = 'scene' | 'tiles' | 'flat';

export interface RenderOptions {
  gridLines: boolean;
  /** Tiles fall back to flat until the atlas has loaded. */
  renderer: Renderer;
  /** Scene renderer: trees per cell side, before the cap for small cells. */
  trees: number;
  /** Scene renderer: moonlight instead of daylight. */
  night: boolean;
  wrap: boolean;
  /** Steps for burnt ground to green back over. */
  regrow: number;
  /** How far through the current step, 0 to 1. Drives transitions. */
  progress: number;
  /** Seconds. Drives flicker. */
  time: number;
}

export interface StepParams {
  growth: number;
  lightning: number;
  moore: boolean;
  wrap: boolean;
  stepsPerSecond: number;
}

/** A cell that lightning set alight. */
export interface Strike {
  x: number;
  y: number;
}

interface Buffer {
  tex: WebGLTexture;
  fbo: WebGLFramebuffer;
}

/**
 * Lightning strikes per second that get a bolt. Strikes are split between a
 * short list picked here (so we know where they land) and the rest, which the
 * step shader rolls per cell; together they're the same Poisson process.
 */
const BOLTS_PER_SECOND = 3;

type Uniforms = Record<string, WebGLUniformLocation | null>;

/** GPU ping-pong Drossel–Schwabl forest-fire simulation. */
export class ForestFire {
  readonly gl: WebGL2RenderingContext;
  width = 0;
  height = 0;
  cellPx = 1;
  generation = 0;

  private buffers: [Buffer, Buffer] | null = null;
  private current = 0;
  private stepProg: WebGLProgram;
  private flatProg: WebGLProgram;
  private dualProg: WebGLProgram;
  private sceneProg: WebGLProgram;
  private stepU: Uniforms;
  private flatU: Uniforms;
  private dualU: Uniforms;
  private sceneU: Uniforms;
  private tiles: WebGLTexture | null = null;
  private vao: WebGLVertexArrayObject;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext('webgl2', { antialias: false, alpha: false });
    if (!gl) throw new Error('WebGL2 is not supported in this browser');
    this.gl = gl;
    this.stepProg = link(gl, fullscreenVert, stepFrag);
    this.flatProg = link(gl, fullscreenVert, flatFrag);
    this.dualProg = link(gl, fullscreenVert, dualFrag);
    this.sceneProg = link(gl, fullscreenVert, sceneFrag);
    this.stepU = uniforms(gl, this.stepProg, ['uState', 'uSeed', 'uGrowth', 'uLightning', 'uMoore', 'uWrap', 'uStrikes', 'uStrikeCount']);
    this.flatU = uniforms(gl, this.flatProg, ['uState', 'uCellPx', 'uGridLines', 'uRegrow']);
    this.dualU = uniforms(gl, this.dualProg, ['uState', 'uTiles', 'uCellPx', 'uGridLines', 'uRegrow', 'uWrap', 'uProgress', 'uTime']);
    this.sceneU = uniforms(gl, this.sceneProg, ['uState', 'uCellPx', 'uGridLines', 'uRegrow', 'uWrap', 'uProgress', 'uTime', 'uTrees', 'uNight']);
    this.vao = gl.createVertexArray()!;
  }

  /**
   * Fit the grid to the canvas. Cells overlapping the old grid are kept;
   * new cells are seeded at `density`.
   */
  resize(cellCssPx: number, density: number) {
    const dpr = window.devicePixelRatio || 1;
    const pxW = Math.max(1, Math.round(this.canvas.clientWidth * dpr));
    const pxH = Math.max(1, Math.round(this.canvas.clientHeight * dpr));
    this.canvas.width = pxW;
    this.canvas.height = pxH;

    const cellPx = Math.max(1, cellCssPx * dpr);
    const w = Math.ceil(pxW / cellPx);
    const h = Math.ceil(pxH / cellPx);
    const cellChanged = cellPx !== this.cellPx;
    this.cellPx = cellPx;
    if (w === this.width && h === this.height && this.buffers) return;

    const old = this.buffers ? { buf: this.buffers[this.current], w: this.width, h: this.height } : null;
    const next = this.createBuffers(w, h, density);

    if (old && !cellChanged) {
      const gl = this.gl;
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, old.buf.fbo);
      gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, next[0].fbo);
      const cw = Math.min(w, old.w);
      const ch = Math.min(h, old.h);
      gl.blitFramebuffer(0, 0, cw, ch, 0, 0, cw, ch, gl.COLOR_BUFFER_BIT, gl.NEAREST);
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
      gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
    }
    if (!old || cellChanged) this.generation = 0;

    this.destroyBuffers();
    this.buffers = next;
    this.current = 0;
    this.width = w;
    this.height = h;
  }

  reseed(density: number) {
    if (!this.buffers) return;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.buffers[this.current].tex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, this.width, this.height, gl.RGBA_INTEGER, gl.UNSIGNED_BYTE, seedData(this.width, this.height, density));
    this.generation = 0;
  }

  /** Advance one step. Returns the listed lightning strikes that lit a tree. */
  step(p: StepParams): Strike[] {
    if (!this.buffers) return [];
    const gl = this.gl;
    const src = this.buffers[this.current];
    const dst = this.buffers[1 - this.current];
    gl.bindFramebuffer(gl.FRAMEBUFFER, dst.fbo);
    gl.viewport(0, 0, this.width, this.height);
    gl.useProgram(this.stepProg);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, src.tex);
    gl.uniform1i(this.stepU.uState, 0);
    gl.uniform1ui(this.stepU.uSeed, (Math.random() * 0xffffffff) >>> 0);
    gl.uniform1f(this.stepU.uGrowth, p.growth);
    const cells = this.width * this.height;
    const expected = p.lightning * cells;
    const listedRate = Math.min(expected, BOLTS_PER_SECOND / p.stepsPerSecond, MAX_STRIKES / 4);
    const n = Math.min(MAX_STRIKES, poisson(listedRate));
    const strikes = new Int32Array(2 * MAX_STRIKES);
    for (let i = 0; i < n; i++) {
      strikes[2 * i] = Math.floor(Math.random() * this.width);
      strikes[2 * i + 1] = Math.floor(Math.random() * this.height);
    }
    gl.uniform1f(this.stepU.uLightning, (expected - listedRate) / cells);
    gl.uniform2iv(this.stepU.uStrikes, strikes);
    gl.uniform1i(this.stepU.uStrikeCount, n);
    gl.uniform1i(this.stepU.uMoore, p.moore ? 1 : 0);
    gl.uniform1i(this.stepU.uWrap, p.wrap ? 1 : 0);
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.current = 1 - this.current;
    this.generation++;

    // Strikes are rare, so a synchronous read of just those cells is cheap.
    const hits: Strike[] = [];
    const px = new Uint32Array(4);
    for (let i = 0; i < n; i++) {
      const x = strikes[2 * i];
      const y = strikes[2 * i + 1];
      gl.readPixels(x, y, 1, 1, gl.RGBA_INTEGER, gl.UNSIGNED_INT, px);
      if (px[0] === State.FIRE && px[2] === Cause.LIGHTNING) hits.push({ x, y });
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    return hits;
  }

  /**
   * Where in a struck cell the fire starts, in canvas pixels (y up). Matches
   * the lightning origin in the renderers' arrival().
   */
  strikePoint(s: Strike): [number, number] {
    return [(s.x + 0.3 + 0.4 * hashf(s.x, s.y, 11)) * this.cellPx, (s.y + 0.3 + 0.4 * hashf(s.x, s.y, 12)) * this.cellPx];
  }

  setTiles(data: TileAtlas) {
    if (this.tiles) this.gl.deleteTexture(this.tiles);
    this.tiles = uploadAtlas(this.gl, data);
  }

  render(o: RenderOptions) {
    if (!this.buffers) return;
    const gl = this.gl;
    const dual = o.renderer === 'tiles' && this.tiles;
    const scene = o.renderer === 'scene';
    const [prog, u] = scene ? [this.sceneProg, this.sceneU] : dual ? [this.dualProg, this.dualU] : [this.flatProg, this.flatU];
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.useProgram(prog);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.buffers[this.current].tex);
    gl.uniform1i(u.uState, 0);
    gl.uniform1f(u.uCellPx, this.cellPx);
    gl.uniform1i(u.uGridLines, o.gridLines && this.cellPx >= 6 ? 1 : 0);
    gl.uniform1f(u.uRegrow, Math.max(1, o.regrow));
    if (dual || scene) {
      gl.uniform1i(u.uWrap, o.wrap ? 1 : 0);
      gl.uniform1f(u.uProgress, o.progress);
      gl.uniform1f(u.uTime, o.time);
    }
    if (dual) {
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D_ARRAY, dual);
      gl.uniform1i(u.uTiles, 1);
    }
    if (scene) {
      // Keep trees at least 4 px apart.
      gl.uniform1i(u.uTrees, Math.max(1, Math.min(Math.round(o.trees), Math.floor(this.cellPx / 4))));
      gl.uniform1i(u.uNight, o.night ? 1 : 0);
    }
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  /** Strike lightning at a canvas-relative CSS pixel position. */
  ignite(cssX: number, cssY: number): Strike | null {
    if (!this.buffers) return null;
    const dpr = window.devicePixelRatio || 1;
    const x = Math.floor((cssX * dpr) / this.cellPx);
    const y = Math.floor((this.canvas.height - cssY * dpr) / this.cellPx);
    if (x < 0 || y < 0 || x >= this.width || y >= this.height) return null;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.buffers[this.current].tex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, 1, 1, gl.RGBA_INTEGER, gl.UNSIGNED_BYTE, new Uint8Array([State.FIRE, State.TREE, Cause.LIGHTNING, 0]));
    return { x, y };
  }

  private createBuffers(w: number, h: number, density: number): [Buffer, Buffer] {
    const gl = this.gl;
    const data = seedData(w, h, density);
    const make = (init: Uint8Array | null): Buffer => {
      const tex = gl.createTexture()!;
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8UI, w, h, 0, gl.RGBA_INTEGER, gl.UNSIGNED_BYTE, init);
      const fbo = gl.createFramebuffer()!;
      gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      return { tex, fbo };
    };
    return [make(data), make(null)];
  }

  private destroyBuffers() {
    if (!this.buffers) return;
    for (const b of this.buffers) {
      this.gl.deleteTexture(b.tex);
      this.gl.deleteFramebuffer(b.fbo);
    }
    this.buffers = null;
  }
}

function poisson(mean: number): number {
  const l = Math.exp(-mean);
  let k = 0;
  for (let p = Math.random(); p > l; p *= Math.random()) k++;
  return k;
}

function seedData(w: number, h: number, density: number): Uint8Array {
  const data = new Uint8Array(w * h * 4);
  for (let i = 0; i < w * h; i++) {
    if (Math.random() < density) data[i * 4] = data[i * 4 + 1] = State.TREE;
    else data[i * 4 + 3] = SETTLED;
  }
  return data;
}

export function link(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram {
  const prog = gl.createProgram()!;
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]] as const) {
    const sh = gl.createShader(type)!;
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
      throw new Error(`Shader compile failed:\n${gl.getShaderInfoLog(sh)}`);
    }
    gl.attachShader(prog, sh);
  }
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
    throw new Error(`Program link failed:\n${gl.getProgramInfoLog(prog)}`);
  }
  return prog;
}

export function uniforms(gl: WebGL2RenderingContext, prog: WebGLProgram, names: string[]): Uniforms {
  return Object.fromEntries(names.map((n) => [n, gl.getUniformLocation(prog, n)]));
}
