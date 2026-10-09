# tinderbox

A full-screen WebGL2 [Drossel–Schwabl forest-fire model](https://en.wikipedia.org/wiki/Forest-fire_model).

Each step, every cell updates at the same time:

- **Tree → burning** if any neighbor is burning, or with probability **f** (lightning)
- **Burning** lasts two steps (catching, then burning), then the cell is **burnt** for one step and **empty** after that. Burnt ground starts as bare dirt and greens back to grass over **Regrowth** steps (drawing only; it doesn't affect the rule)
- **Empty → tree** with probability **p** (growth)

The interesting regime is f ≪ p ≪ 1. Fires then follow a power-law size distribution (self-organized criticality).

## Run

```sh
npm install
npm run dev
```

**Controls:** use the panel to set cell size, trees per cell, p, f, speed, regrowth time, seed density, neighborhood, edge wrap, and day or night. The scene renderer wants big, slow cells: try 48–96 px and 3–5 s per step. Keys:

- `H` or the × / ☰ buttons hide and show the panel
- `F` toggles fullscreen
- `Space` pauses
- `.` steps once

Click or drag on the canvas to start fires. Settings, including whether the panel is hidden, are saved in localStorage.

## Layout

- `src/shaders.ts`: GLSL for the update rule and the flat and dual-grid renderers, plus the state and cause encoding
- `src/scene.ts`: the procedural scene renderer (the default)
- `src/atlas.ts`: loading the dual-grid tile atlas, and the procedural placeholder used when there isn't one
- `src/sim.ts`: ping-pong framebuffer simulation, resizing, and ignition
- `src/controls.ts`, `src/params.ts`: the parameter panel and its saved settings
- `src/main.ts`: the frame loop, input handling, and fullscreen

## State encoding

One `RGBA8UI` texel per cell holds:

- `R`: the current state (`EMPTY`, `TREE`, `FIRE`, `BURNT`)
- `G`: the previous state
- `B`: the cause of the last transition (`GROW`, `MATURE`, `SPREAD`, `LIGHTNING`, `BURNING`, `BURNOUT`, `CLEAR`). `MATURE` marks a tree's second step, so saplings can grow over two steps
- `A`: for `SPREAD` (and the `BURNING` step after it), a bitmask of the neighbors that were burning. For `EMPTY`, and for a tree in its first two steps, the steps since the ground burned (255 = never burned)

This gives the renderer the full temporal transition for each cell without extra passes.

## Procedural scene

The default renderer draws everything in the fragment shader, with no art assets. Each cell is a stand of individual pines (Trees per side × Trees per side), seen at an oblique angle so they overlap and cast shadows. Over a cell's life:

- **Growing:** saplings sprout and grow to full size over two steps, over whatever ground was there.
- **Catching:** each tree ignites when the fire front reaches it, so a stand lights up in order from the side the fire came from (or outward from the strike, for lightning).
- **Burning:** flames flare up each crown, lean with the wind and flicker, then die back as the needles scorch and burn away. Sparks rise on the wind and smoke drifts off.
- **Burnt:** flames go out tree by tree, leaving charred snags with glowing embers on ash.
- **Afterwards:** snags stand for a few steps, then tip and crumble. The ash washes into the dirt, and grass grows back in patches over Regrowth steps.

**Night** switches to moonlight, so fires light up their surroundings. The design is in [docs/PLAN.md](docs/PLAN.md).

## Dual-grid tiles

The tiles renderer draws tiles centered on grid vertices, so neighboring tiles always join. Each tile is one combination of five looks (grass, pine forest, burning forest, burnt stand, dirt) at its four corners. Transitions are drawn by the shader: fire sweeps in from the neighbors that lit it, burns, dies down to a charred stand, clears to dirt, and grass creeps back. The design and the generator (`tools/atlas/stills.py`) are described in [docs/PLAN.md](docs/PLAN.md).

If `atlas/stills.{png,json}` exists in the data dir (`$TINDERBOX_DATA`, default `~/.local/share/tinderbox`) it's used; `vite.config.ts` serves it in dev and copies it into `dist/` on build. Otherwise a procedural placeholder is drawn at startup. **That's currently always the case:** generating the five-look atlas didn't work out (see the plan), and the next step is a fully procedural renderer.

## Roadmap

- **Phase 1 (done):** GPU sim, flat-color renderer, adjustable parameters, fullscreen.
- **Phase 2 (in progress):** a fully procedural scene (see [Procedural scene](#procedural-scene)). Before it: dual-grid tiles with shader-drawn transitions; diffusion-generated tiles are on hold.
- Also planned: fire-size histogram and density plots (needs GPU reduction or async readback), and painting trees and firebreaks.
