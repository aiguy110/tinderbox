import { Cause, State, fullscreenVert, renderFrag, stepFrag } from './shaders';

export interface StepParams {
  growth: number;
  lightning: number;
  moore: boolean;
  wrap: boolean;
}

interface Buffer {
  tex: WebGLTexture;
  fbo: WebGLFramebuffer;
}

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
  private renderProg: WebGLProgram;
  private stepU: Uniforms;
  private renderU: Uniforms;
  private vao: WebGLVertexArrayObject;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext('webgl2', { antialias: false, alpha: false });
    if (!gl) throw new Error('WebGL2 is not supported in this browser');
    this.gl = gl;
    this.stepProg = link(gl, fullscreenVert, stepFrag);
    this.renderProg = link(gl, fullscreenVert, renderFrag);
    this.stepU = uniforms(gl, this.stepProg, ['uState', 'uSeed', 'uGrowth', 'uLightning', 'uMoore', 'uWrap']);
    this.renderU = uniforms(gl, this.renderProg, ['uState', 'uCellPx', 'uGridLines']);
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

  step(p: StepParams) {
    if (!this.buffers) return;
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
    gl.uniform1f(this.stepU.uLightning, p.lightning);
    gl.uniform1i(this.stepU.uMoore, p.moore ? 1 : 0);
    gl.uniform1i(this.stepU.uWrap, p.wrap ? 1 : 0);
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.current = 1 - this.current;
    this.generation++;
  }

  render(gridLines: boolean) {
    if (!this.buffers) return;
    const gl = this.gl;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.useProgram(this.renderProg);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.buffers[this.current].tex);
    gl.uniform1i(this.renderU.uState, 0);
    gl.uniform1f(this.renderU.uCellPx, this.cellPx);
    gl.uniform1i(this.renderU.uGridLines, gridLines && this.cellPx >= 6 ? 1 : 0);
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  /** Strike lightning at a canvas-relative CSS pixel position. */
  ignite(cssX: number, cssY: number) {
    if (!this.buffers) return;
    const dpr = window.devicePixelRatio || 1;
    const x = Math.floor((cssX * dpr) / this.cellPx);
    const y = Math.floor((this.canvas.height - cssY * dpr) / this.cellPx);
    if (x < 0 || y < 0 || x >= this.width || y >= this.height) return;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.buffers[this.current].tex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, 1, 1, gl.RGBA_INTEGER, gl.UNSIGNED_BYTE, new Uint8Array([State.FIRE, State.TREE, Cause.LIGHTNING, 0]));
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

function seedData(w: number, h: number, density: number): Uint8Array {
  const data = new Uint8Array(w * h * 4);
  for (let i = 0; i < w * h; i++) {
    if (Math.random() < density) data[i * 4] = data[i * 4 + 1] = State.TREE;
  }
  return data;
}

function link(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram {
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

function uniforms(gl: WebGL2RenderingContext, prog: WebGLProgram, names: string[]): Uniforms {
  return Object.fromEntries(names.map((n) => [n, gl.getUniformLocation(prog, n)]));
}
