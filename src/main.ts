import './style.css';
import { loadTiles } from './atlas';
import { buildControls } from './controls';
import { Lightning } from './lightning';
import { load, save } from './params';
import { ForestFire, MAX_PIXEL_RATIO, type Strike } from './sim';

const MAX_STEPS_PER_FRAME = 16;
/** While paused, a manual step's transitions play at most this fast. */
const PAUSED_STEPS_PER_SECOND = 2;

const canvas = document.querySelector<HTMLCanvasElement>('#stage')!;
const panel = document.querySelector<HTMLElement>('#panel')!;
const params = load();

let sim: ForestFire;
let lightning: Lightning;
try {
  sim = new ForestFire(canvas);
  lightning = new Lightning(sim.gl);
} catch (err) {
  document.body.textContent = String(err);
  throw err;
}

// Mobile GPUs reset a context that takes too long per frame; without this
// the page just freezes.
canvas.addEventListener('webglcontextlost', (e) => e.preventDefault());
canvas.addEventListener('webglcontextrestored', () => location.reload());

const controls = buildControls(panel, params, {
  onChange(key) {
    if (key === 'cellSize') sim.resize(params.cellSize, params.initialDensity);
    if (key === 'renderer') resetResolution();
    save(params);
  },
  onStep() {
    params.paused = true;
    controls.refresh();
    stepOnce();
  },
  onReseed: () => sim.reseed(params.initialDensity),
  onFullscreen: toggleFullscreen,
});

function toggleFullscreen() {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.documentElement.requestFullscreen();
}

let atlasSource = 'loading';
loadTiles().then((tiles) => {
  sim.setTiles(tiles);
  atlasSource = tiles.source;
});

new ResizeObserver(() => sim.resize(params.cellSize, params.initialDensity)).observe(canvas);
sim.resize(params.cellSize, params.initialDensity);

window.addEventListener('keydown', (e) => {
  if (e.key === 'h' || e.key === 'H') controls.toggleHidden();
  else if (e.key === 'f' || e.key === 'F') toggleFullscreen();
  else if (e.key === ' ') {
    e.preventDefault();
    params.paused = !params.paused;
    controls.refresh();
  } else if (e.key === '.') stepOnce();
});

// Click or drag on the canvas to ignite cells. Dragging throws fewer bolts.
const DRAG_BOLT_MS = 150;
let igniting = false;
let boltAt = 0;
const igniteAt = (e: PointerEvent) => {
  const r = canvas.getBoundingClientRect();
  const hit = sim.ignite(e.clientX - r.left, e.clientY - r.top);
  if (hit && e.timeStamp - boltAt > DRAG_BOLT_MS) {
    boltAt = e.timeStamp;
    strike([hit]);
  }
};
canvas.addEventListener('pointerdown', (e) => {
  igniting = true;
  canvas.setPointerCapture(e.pointerId);
  igniteAt(e);
});
canvas.addEventListener('pointermove', (e) => igniting && igniteAt(e));
canvas.addEventListener('pointerup', () => (igniting = false));

let last = performance.now();
// Fractional steps owed. After stepping, it's also how far we are through the
// current step, which is what transitions are timed by.
let debt = 0;
let fps = 0;
let statsAt = 0;

function strike(hits: Strike[]) {
  for (const h of hits) lightning.strike(...sim.strikePoint(h), performance.now(), sim.pixelRatio);
}

// Lower the render resolution while frames are slow, and creep back up after a
// spell of full frame rate, but never back to a ratio that was too slow.
const MIN_PIXEL_RATIO = 0.5;
const SLOW_FPS = 40;
const FAST_FPS = 57;
const fullRatio = () => Math.min(window.devicePixelRatio || 1, MAX_PIXEL_RATIO);
let ratioCeiling = fullRatio();
let windowAt = 0;
let windowFrames = 0;
let fastWindows = 0;
// The last drop, so we can undo it if it didn't help (a frame rate capped by
// the browser, like iOS Low Power Mode's 30 fps, isn't the GPU's fault).
let dropped: { ratio: number; rate: number } | null = null;
let gaveUp = false;
// A background tab stops frames; don't count that as slow.
document.addEventListener('visibilitychange', () => (windowAt = 0));

function resetResolution() {
  ratioCeiling = fullRatio();
  setPixelRatio(ratioCeiling);
  fastWindows = 0;
  dropped = null;
  gaveUp = false;
}

function setPixelRatio(r: number) {
  if (Math.abs(r - sim.pixelRatio) < 0.01) return;
  sim.pixelRatio = r;
  sim.resize(params.cellSize, params.initialDensity);
}

function adaptResolution(now: number) {
  if (!windowAt) {
    windowAt = now;
    windowFrames = 0;
    return;
  }
  windowFrames++;
  if (now - windowAt < 1000) return;
  const rate = (windowFrames * 1000) / (now - windowAt);
  windowAt = now;
  windowFrames = 0;
  if (dropped) {
    const before = dropped;
    dropped = null;
    if (rate < before.rate * 1.15) {
      setPixelRatio(before.ratio);
      gaveUp = true;
      return;
    }
  }
  if (rate < SLOW_FPS && sim.pixelRatio > MIN_PIXEL_RATIO && !gaveUp) {
    dropped = { ratio: sim.pixelRatio, rate };
    ratioCeiling = sim.pixelRatio * 0.95;
    // Pixel cost goes with the square of the ratio.
    setPixelRatio(Math.max(MIN_PIXEL_RATIO, sim.pixelRatio * Math.sqrt(rate / FAST_FPS)));
    fastWindows = 0;
  } else if (rate >= FAST_FPS && ++fastWindows >= 4 && sim.pixelRatio < ratioCeiling) {
    setPixelRatio(Math.min(ratioCeiling, sim.pixelRatio * 1.15));
    fastWindows = 0;
  }
}

function stepOnce() {
  strike(sim.step(params));
  debt = 0;
}

// For poking at transitions from the console in dev: step, then set how far
// through the step to show (with the sim paused and Speed turned right down).
if (import.meta.env.DEV) {
  Object.assign(window, { tinderbox: { sim, params, step: stepOnce, setProgress: (x: number) => (debt = x) } });
}

function frame(now: number) {
  const dt = Math.min(0.25, (now - last) / 1000);
  last = now;
  fps = fps * 0.95 + (dt > 0 ? 1 / dt : 0) * 0.05;

  if (!params.paused) {
    debt += dt * params.stepsPerSecond;
    const n = Math.min(MAX_STEPS_PER_FRAME, Math.floor(debt));
    for (let i = 0; i < n; i++) strike(sim.step(params));
    debt = n === MAX_STEPS_PER_FRAME ? 0 : debt - n;
  } else {
    debt = Math.min(1, debt + dt * Math.min(params.stepsPerSecond, PAUSED_STEPS_PER_SECOND));
  }
  sim.render({ gridLines: params.gridLines, renderer: params.renderer, regrow: params.regrow, trees: params.trees, night: params.night, wrap: params.wrap, progress: debt, time: now / 1000 });
  lightning.render(now);
  adaptResolution(now);

  if (now - statsAt > 250) {
    statsAt = now;
    controls.setStats(
      `${sim.width}×${sim.height} cells · gen ${sim.generation.toLocaleString()} · ${fps.toFixed(0)} fps · ${sim.pixelRatio.toFixed(2)}× res${params.renderer === 'tiles' ? ` · ${atlasSource} atlas` : ''}`,
    );
  }
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
