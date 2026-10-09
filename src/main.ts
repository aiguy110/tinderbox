import './style.css';
import { loadTiles } from './atlas';
import { buildControls } from './controls';
import { Lightning } from './lightning';
import { load, save } from './params';
import { ForestFire, type Strike } from './sim';

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

const controls = buildControls(panel, params, {
  onChange(key) {
    if (key === 'cellSize') sim.resize(params.cellSize, params.initialDensity);
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
  for (const h of hits) lightning.strike(...sim.strikePoint(h), performance.now());
}

function stepOnce() {
  strike(sim.step(params));
  debt = 0;
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
  sim.render({ gridLines: params.gridLines, renderer: params.renderer, regrow: params.regrow, wrap: params.wrap, progress: debt, time: now / 1000 });
  lightning.render(now);

  if (now - statsAt > 250) {
    statsAt = now;
    controls.setStats(
      `${sim.width}×${sim.height} cells · gen ${sim.generation.toLocaleString()} · ${fps.toFixed(0)} fps${params.renderer === 'flat' ? '' : ` · ${atlasSource} atlas`}`,
    );
  }
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
