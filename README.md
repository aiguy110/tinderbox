# tinderbox

A full-screen WebGL2 [Drossel–Schwabl forest-fire model](https://en.wikipedia.org/wiki/Forest-fire_model).

Each step, every cell updates at the same time:

- **Tree → burning** if any neighbor is burning, or with probability **f** (lightning)
- **Burning** lasts two steps (catching, then burning), then the cell is **burnt** for one step and **empty** after that
- **Empty → tree** with probability **p** (growth)

The interesting regime is f ≪ p ≪ 1. Fires then follow a power-law size distribution (self-organized criticality).

## Run

```sh
npm install
npm run dev
```

**Controls:** use the panel to set cell size, p, f, speed, seed density, neighborhood, and edge wrap. Keys:

- `H` or the × / ☰ buttons hide and show the panel
- `F` toggles fullscreen
- `Space` pauses
- `.` steps once

Click or drag on the canvas to start fires. Settings, including whether the panel is hidden, are saved in localStorage.

## Layout

- `src/shaders.ts`: GLSL for the update rule and the flat and dual-grid renderers, plus the state and cause encoding
- `src/atlas.ts`: loading the dual-grid tile atlas, and the procedural placeholder used when there isn't one
- `src/sim.ts`: ping-pong framebuffer simulation, resizing, and ignition
- `src/controls.ts`, `src/params.ts`: the parameter panel and its saved settings
- `src/main.ts`: the frame loop, input handling, and fullscreen

## State encoding

One `RGBA8UI` texel per cell holds:

- `R`: the current state (`EMPTY`, `TREE`, `FIRE`, `BURNT`)
- `G`: the previous state
- `B`: the cause of the last transition (`GROW`, `SPREAD`, `LIGHTNING`, `BURNING`, `BURNOUT`, `CLEAR`)
- `A`: for `SPREAD` (and the `BURNING` step after it), a bitmask of the neighbors that were burning

This gives the renderer the full temporal transition for each cell without extra passes.

## Dual-grid tiles

The default renderer draws generated tiles centered on grid vertices, so neighboring tiles always join. Transitions are drawn by the shader: fire sweeps in from the neighbors that lit it, burns, dies down to a charred stand and clears. **Trees per cell** draws each cell as a small stand. The design and the generator (`tools/atlas/tiles.py`) are described in [docs/PLAN.md](docs/PLAN.md).

If `atlas/tiles.{png,json}` exists in the data dir (`$TINDERBOX_DATA`, default `~/.local/share/tinderbox`) it's used; `vite.config.ts` serves it in dev and copies it into `dist/` on build. Otherwise a procedural placeholder is drawn at startup.

## Roadmap

- **Phase 1 (done):** GPU sim, flat-color renderer, adjustable parameters, fullscreen.
- **Phase 2 (in progress):** Diffusion-generated dual-grid tiles with shader-drawn transitions (see [Dual-grid tiles](#dual-grid-tiles)).
- Also planned: fire-size histogram and density plots (needs GPU reduction or async readback), and painting trees and firebreaks.
