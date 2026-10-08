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
  /** Animated sprites instead of flat colors. */
  sprites: boolean;
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
  sprites: true,
  paused: false,
  panelHidden: false,
};

const KEY = 'tinderbox:params';

export function load(): Params {
  try {
    return { ...defaults, ...JSON.parse(localStorage.getItem(KEY) ?? '{}'), paused: false };
  } catch {
    return { ...defaults };
  }
}

export function save(p: Params) {
  localStorage.setItem(KEY, JSON.stringify(p));
}
