// Lightning bolts: a procedural, animated strike from the top of the screen
// down to a cell that lightning set alight.
//
// Each bolt is a jagged polyline (midpoint displacement) with a few forks.
// Its segments are drawn as glowing capsules into an offscreen buffer with MAX
// blending, so joints and overlapping glow don't double up, and that buffer is
// added onto the scene along with a faint sky flash.
//
// Timeline: a dim leader feels its way down, the return stroke flashes the
// whole channel, then it decays with a restrike or two.

import { link, uniforms } from './sim';

/** Bolts on screen at once; more strikes than this are dropped. */
const MAX_BOLTS = 12;
/** Seconds for the leader to reach the ground. */
const LEADER = 0.07;
const LIFE = 0.75;

const segVert = /* glsl */ `#version 300 es
in vec2 aPos;
in vec4 aSeg;
in vec2 aStyle;
uniform vec2 uSize;
out vec2 vPos;
flat out vec4 vSeg;
flat out vec2 vStyle;
void main() {
  vPos = aPos;
  vSeg = aSeg;
  vStyle = aStyle;
  gl_Position = vec4(aPos / uSize * 2.0 - 1.0, 0.0, 1.0);
}
`;

// Distance to the segment gives a white-hot core and a blue glow.
const segFrag = /* glsl */ `#version 300 es
precision highp float;
in vec2 vPos;
flat in vec4 vSeg;
flat in vec2 vStyle;
out vec4 outColor;
void main() {
  vec2 a = vSeg.xy, ba = vSeg.zw - a, pa = vPos - a;
  float d = length(pa - ba * clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0));
  float w = vStyle.x, k = vStyle.y;
  float core = exp(-pow(d / w, 2.0));
  float glow = exp(-d / (w * 4.0));
  vec3 c = core * mix(vec3(0.55, 0.65, 1.0), vec3(1.0), clamp(k, 0.0, 1.0)) + glow * 0.4 * vec3(0.45, 0.55, 1.0);
  outColor = vec4(c * min(k, 1.0), 1.0);
}
`;

const compositeVert = /* glsl */ `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
`;

const compositeFrag = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D uBolts;
uniform float uSky;
out vec4 outColor;
void main() {
  vec3 c = texelFetch(uBolts, ivec2(gl_FragCoord.xy), 0).rgb;
  outColor = vec4(c + uSky * vec3(0.5, 0.55, 0.7), 1.0);
}
`;

interface Seg {
  ax: number;
  ay: number;
  bx: number;
  by: number;
  /** Core width in pixels. */
  w: number;
  /** When the leader reaches the segment's far end, 0 (sky) to 1 (ground). */
  reach: number;
  fork: boolean;
}

interface Bolt {
  born: number;
  segs: Seg[];
  restrikes: number[];
}

type Point = [number, number];

/** Jag a straight line by midpoint displacement until segments are short. */
function jag(a: Point, b: Point, rough: number, minLen: number): Point[] {
  let pts: Point[] = [a, b];
  for (;;) {
    const next: Point[] = [pts[0]];
    let split = false;
    for (let i = 1; i < pts.length; i++) {
      const [ax, ay] = pts[i - 1];
      const [bx, by] = pts[i];
      const dx = bx - ax;
      const dy = by - ay;
      if (Math.hypot(dx, dy) > minLen) {
        const t = 0.35 + 0.3 * Math.random();
        const off = (Math.random() - 0.5) * rough;
        next.push([ax + dx * t - dy * off, ay + dy * t + dx * off]);
        split = true;
      }
      next.push(pts[i]);
    }
    pts = next;
    if (!split) return pts;
  }
}

function addPath(segs: Seg[], pts: Point[], w: number, reach0: number, reach1: number, fork: boolean) {
  for (let i = 1; i < pts.length; i++) {
    const [ax, ay] = pts[i - 1];
    const [bx, by] = pts[i];
    segs.push({ ax, ay, bx, by, w, reach: reach0 + ((reach1 - reach0) * i) / (pts.length - 1), fork });
  }
}

function buildBolt(x: number, y: number, top: number, scale: number): Seg[] {
  const segs: Seg[] = [];
  const h = top - y;
  const start: Point = [x + (Math.random() - 0.5) * 0.5 * h, top + 8 * scale];
  // A few large kinks first so the channel wanders, then fine jaggedness.
  const coarse = jag(start, [x, y], 0.5, h / 5);
  const main: Point[] = [coarse[0]];
  for (let i = 1; i < coarse.length; i++) main.push(...jag(coarse[i - 1], coarse[i], 0.45, 5 * scale).slice(1));
  const w = 1.4 * scale;
  addPath(segs, main, w, 0, 1, false);

  // Forks peel off the upper part of the channel, heading down and away.
  const forks = 2 + Math.floor(Math.random() * 4);
  for (let f = 0; f < forks; f++) {
    const i = Math.floor(Math.random() * main.length * 0.75);
    const [px, py] = main[i];
    const at = i / (main.length - 1);
    const len = (0.12 + 0.25 * Math.random()) * h * (1 - at);
    const ang = -Math.PI / 2 + (Math.random() < 0.5 ? -1 : 1) * (0.3 + 0.6 * Math.random());
    const end: Point = [px + Math.cos(ang) * len, py + Math.sin(ang) * len];
    const pts = jag([px, py], end, 0.6, 5 * scale);
    addPath(segs, pts, w * 0.55, at, Math.min(1, at + len / h), true);
  }
  return segs;
}

export class Lightning {
  private bolts: Bolt[] = [];
  private segProg: WebGLProgram;
  private compProg: WebGLProgram;
  private segU;
  private compU;
  private vao: WebGLVertexArrayObject;
  private emptyVao: WebGLVertexArrayObject;
  private vbo: WebGLBuffer;
  private target: { tex: WebGLTexture; fbo: WebGLFramebuffer; w: number; h: number } | null = null;

  constructor(private gl: WebGL2RenderingContext) {
    this.segProg = link(gl, segVert, segFrag);
    this.compProg = link(gl, compositeVert, compositeFrag);
    this.segU = uniforms(gl, this.segProg, ['uSize']);
    this.compU = uniforms(gl, this.compProg, ['uBolts', 'uSky']);
    this.vbo = gl.createBuffer()!;
    this.vao = gl.createVertexArray()!;
    this.emptyVao = gl.createVertexArray()!;
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vbo);
    const stride = 8 * 4;
    for (const [name, size, offset] of [['aPos', 2, 0], ['aSeg', 4, 2], ['aStyle', 2, 6]] as const) {
      const loc = gl.getAttribLocation(this.segProg, name);
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, size, gl.FLOAT, false, stride, offset * 4);
    }
    gl.bindVertexArray(null);
  }

  /** Strike at a point in canvas pixels (y up), timed from `now` in ms. */
  strike(x: number, y: number, now: number, pixelRatio: number) {
    if (this.bolts.length >= MAX_BOLTS) return;
    const gl = this.gl;
    const scale = pixelRatio;
    const restrikes = [LEADER + 0.1 + 0.1 * Math.random()];
    if (Math.random() < 0.6) restrikes.push(restrikes[0] + 0.08 + 0.12 * Math.random());
    this.bolts.push({ born: now / 1000, segs: buildBolt(x, y, gl.drawingBufferHeight, scale), restrikes });
  }

  /** Draw live bolts over whatever is in the default framebuffer. */
  render(now: number) {
    const t = now / 1000;
    this.bolts = this.bolts.filter((b) => t - b.born < LIFE);
    if (!this.bolts.length) return;
    const gl = this.gl;
    const W = gl.drawingBufferWidth;
    const H = gl.drawingBufferHeight;

    const verts: number[] = [];
    let sky = 0;
    for (const b of this.bolts) {
      const age = t - b.born;
      let stroke = 0;
      let reach = 1;
      if (age < LEADER) {
        reach = age / LEADER;
      } else {
        stroke = Math.exp(-(age - LEADER) / 0.06);
        for (const r of b.restrikes) if (age >= r) stroke = Math.max(stroke, 0.85 * Math.exp(-(age - r) / 0.05));
      }
      const fade = 1 - Math.max(0, (age - LIFE + 0.2) / 0.2);
      const main = age < LEADER ? 0.4 : Math.max(stroke * 1.5, 0.2 * Math.exp(-(age - LEADER) / 0.2)) * fade;
      const fork = age < LEADER ? 0.3 : stroke * 0.7 * fade;
      sky += 0.2 * stroke;
      for (const s of b.segs) {
        if (s.reach > reach + 0.02) continue;
        const k = s.fork ? fork : main;
        if (k < 0.01) continue;
        // A quad around the segment, wide enough for the glow.
        const r = s.w * 14;
        const dx = s.bx - s.ax;
        const dy = s.by - s.ay;
        const len = Math.hypot(dx, dy) || 1;
        const ux = (dx / len) * r;
        const uy = (dy / len) * r;
        const corners = [
          [s.ax - ux - uy, s.ay - uy + ux],
          [s.ax - ux + uy, s.ay - uy - ux],
          [s.bx + ux + uy, s.by + uy - ux],
          [s.bx + ux - uy, s.by + uy + ux],
        ];
        for (const i of [0, 1, 2, 0, 2, 3]) verts.push(corners[i][0], corners[i][1], s.ax, s.ay, s.bx, s.by, s.w, k);
      }
    }

    const target = this.ensureTarget(W, H);
    gl.bindFramebuffer(gl.FRAMEBUFFER, target.fbo);
    gl.viewport(0, 0, W, H);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendEquation(gl.MAX);
    gl.useProgram(this.segProg);
    gl.uniform2f(this.segU.uSize, W, H);
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vbo);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.STREAM_DRAW);
    gl.drawArrays(gl.TRIANGLES, 0, verts.length / 8);

    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.blendEquation(gl.FUNC_ADD);
    gl.blendFunc(gl.ONE, gl.ONE);
    gl.useProgram(this.compProg);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, target.tex);
    gl.uniform1i(this.compU.uBolts, 0);
    gl.uniform1f(this.compU.uSky, Math.min(sky, 0.35));
    gl.bindVertexArray(this.emptyVao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.disable(gl.BLEND);
  }

  private ensureTarget(w: number, h: number) {
    if (this.target && this.target.w === w && this.target.h === h) return this.target;
    const gl = this.gl;
    if (this.target) {
      gl.deleteTexture(this.target.tex);
      gl.deleteFramebuffer(this.target.fbo);
    }
    const tex = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    const fbo = gl.createFramebuffer()!;
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    this.target = { tex, fbo, w, h };
    return this.target;
  }
}
