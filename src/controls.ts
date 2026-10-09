import { defaults, type Params } from './params';
import type { Renderer } from './sim';

export interface ControlHandlers {
  onChange(key: keyof Params): void;
  onStep(): void;
  onReseed(): void;
  onFullscreen(): void;
}

export interface Controls {
  /** Re-sync inputs after params were changed outside the panel. */
  refresh(): void;
  setStats(text: string): void;
  toggleHidden(): void;
}

interface SliderSpec {
  key: 'cellSize' | 'trees' | 'regrow' | 'growth' | 'lightning' | 'stepsPerSecond' | 'initialDensity';
  label: string;
  min: number;
  max: number;
  step: number;
  /** Slider position is log10(value). */
  log?: boolean;
  format(v: number): string;
}

const sliders: SliderSpec[] = [
  { key: 'cellSize', label: 'Cell size', min: 1, max: 128, step: 1, format: (v) => `${v}px` },
  { key: 'growth', label: 'Growth p', min: -5, max: 0, step: 0.05, log: true, format: sci },
  { key: 'lightning', label: 'Lightning f', min: -8, max: -1, step: 0.05, log: true, format: sci },
  { key: 'stepsPerSecond', label: 'Speed', min: -1, max: Math.log10(480), step: 0.01, log: true, format: speed },
  { key: 'trees', label: 'Trees per side', min: 1, max: 8, step: 1, format: (v) => `${v * v} per cell` },
  { key: 'regrow', label: 'Regrowth', min: 1, max: 30, step: 1, format: (v) => `${v} steps` },
  { key: 'initialDensity', label: 'Seed density', min: 0, max: 1, step: 0.01, format: (v) => `${Math.round(v * 100)}%` },
];

const renderers: { value: Renderer; label: string }[] = [
  { value: 'scene', label: 'Procedural scene' },
  { value: 'tiles', label: 'Dual-grid tiles' },
  { value: 'flat', label: 'Flat colors' },
];

const toggles: { key: 'moore' | 'wrap' | 'gridLines' | 'night' | 'paused'; label: string }[] = [
  { key: 'paused', label: 'Paused' },
  { key: 'moore', label: '8-neighbor spread' },
  { key: 'wrap', label: 'Wrap edges' },
  { key: 'gridLines', label: 'Grid lines' },
  { key: 'night', label: 'Night' },
];

export function buildControls(root: HTMLElement, params: Params, h: ControlHandlers): Controls {
  root.innerHTML = `
    <header>
      <h1>tinderbox</h1>
      <button class="hide" title="Hide panel (H)" aria-label="Hide panel">×</button>
      <span class="hint">H hides · F fullscreen · Space pause · click to ignite</span>
    </header>
    <div class="stats"></div>`;
  const stats = root.querySelector<HTMLElement>('.stats')!;

  const show = document.createElement('button');
  show.className = 'show-panel';
  show.title = 'Show panel (H)';
  show.setAttribute('aria-label', 'Show panel');
  show.textContent = '☰';
  root.after(show);

  const setHidden = (hidden: boolean) => {
    params.panelHidden = hidden;
    root.classList.toggle('hidden', hidden);
    show.classList.toggle('visible', hidden);
    h.onChange('panelHidden');
  };
  root.querySelector('.hide')!.addEventListener('click', () => setHidden(true));
  show.addEventListener('click', () => setHidden(false));
  root.classList.toggle('hidden', params.panelHidden);
  show.classList.toggle('visible', params.panelHidden);
  const syncers: (() => void)[] = [];

  for (const s of sliders) {
    const row = document.createElement('label');
    row.className = 'row';
    row.innerHTML = `<span>${s.label}</span><output></output><input type="range" min="${s.min}" max="${s.max}" step="${s.step}">`;
    const input = row.querySelector('input')!;
    const out = row.querySelector('output')!;
    const sync = () => {
      input.value = String(s.log ? Math.log10(params[s.key]) : params[s.key]);
      out.textContent = s.format(params[s.key]);
    };
    input.addEventListener('input', () => {
      const raw = Number(input.value);
      params[s.key] = s.log ? 10 ** raw : raw;
      out.textContent = s.format(params[s.key]);
      h.onChange(s.key);
    });
    syncers.push(sync);
    root.appendChild(row);
  }

  const ratio = document.createElement('div');
  ratio.className = 'ratio';
  root.appendChild(ratio);
  const syncRatio = () => (ratio.textContent = `p/f ≈ ${Math.round(params.growth / params.lightning).toLocaleString()}`);
  syncers.push(syncRatio);
  root.addEventListener('input', syncRatio);

  const renderRow = document.createElement('label');
  renderRow.className = 'row select';
  renderRow.innerHTML = `<span>Renderer</span><select>${renderers.map((r) => `<option value="${r.value}">${r.label}</option>`).join('')}</select>`;
  const select = renderRow.querySelector('select')!;
  syncers.push(() => (select.value = params.renderer));
  select.addEventListener('change', () => {
    params.renderer = select.value as Renderer;
    h.onChange('renderer');
  });
  root.appendChild(renderRow);

  const checks = document.createElement('div');
  checks.className = 'checks';
  for (const t of toggles) {
    const row = document.createElement('label');
    row.innerHTML = `<input type="checkbox"> ${t.label}`;
    const input = row.querySelector('input')!;
    syncers.push(() => (input.checked = params[t.key]));
    input.addEventListener('change', () => {
      params[t.key] = input.checked;
      h.onChange(t.key);
    });
    checks.appendChild(row);
  }
  root.appendChild(checks);

  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const button = (label: string, fn: () => void) => {
    const b = document.createElement('button');
    b.textContent = label;
    b.addEventListener('click', fn);
    buttons.appendChild(b);
  };
  button('Step', h.onStep);
  button('Reseed', h.onReseed);
  button('Defaults', () => {
    const { paused, panelHidden } = params;
    Object.assign(params, defaults, { paused, panelHidden });
    refresh();
    h.onChange('cellSize');
  });
  button('Fullscreen', h.onFullscreen);
  root.appendChild(buttons);

  const refresh = () => syncers.forEach((s) => s());
  refresh();

  return {
    refresh,
    setStats: (text) => (stats.textContent = text),
    toggleHidden: () => setHidden(!params.panelHidden),
  };
}

function speed(v: number): string {
  return v >= 1 ? `${Math.round(v)} steps/s` : `${(1 / v).toFixed(1)} s/step`;
}

function sci(v: number): string {
  return v >= 0.01 ? v.toPrecision(2) : v.toExponential(1);
}
