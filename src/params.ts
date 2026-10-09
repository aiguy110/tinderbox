import type { Renderer } from './sim';

export interface Params {
  /** Cell edge length in CSS pixels. */
  cellSize: number;
  /** p: probability per step that an empty cell grows a tree. */
  growth: number;
  /** f: probability per step that a tree is struck by lightning. */
  lightning: number;
  /** Simulation steps per second. */
  stepsPerSecond: number;
  /** Tree density used when (re)seeding. */
  initialDensity: number;
  /** 8-neighbor (Moore) fire spread instead of 4-neighbor (von Neumann). */
  moore: boolean;
  /** Toroidal edges. */
  wrap: boolean;
  gridLines: boolean;
  /** Steps for burnt ground to green back over. */
  regrow: number;
  /** Procedural scene, dual-grid tiles or flat colors (docs/PLAN.md). */
  renderer: Renderer;
  /** Scene renderer: trees per cell side. */
  trees: number;
  /** Scene renderer: moonlight instead of daylight. */
  night: boolean;
  paused: boolean;
  panelHidden: boolean;
}

export const defaults: Params = {
  cellSize: 3,
  growth: 0.01,
  lightning: 0.00001,
  stepsPerSecond: 60,
  initialDensity: 0.5,
  moore: false,
  wrap: true,
  gridLines: true,
  regrow: 10,
  renderer: 'scene',
  trees: 4,
  night: false,
  paused: false,
  panelHidden: false,
};

const KEY = 'tinderbox:params';

export function load(): Params {
  try {
    const p: Params = { ...defaults, ...JSON.parse(localStorage.getItem(KEY) ?? '{}'), paused: false };
    // Saved settings may name a renderer that has since been removed or
    // renamed ('dual' is now 'tiles', and the scene replaced it as default).
    if (p.renderer !== 'flat' && p.renderer !== 'tiles') p.renderer = 'scene';
    return p;
  } catch {
    return { ...defaults };
  }
}

export function save(p: Params) {
  localStorage.setItem(KEY, JSON.stringify(p));
}
