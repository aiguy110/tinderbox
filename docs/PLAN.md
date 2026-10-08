# Phase 2 plan: seamless diffusion tiles

Living document. Update it as you go (see CLAUDE.md): tick milestones, record decisions and why, and keep **Next steps** accurate for whoever picks this up next.

## Goal

Render each cell with diffusion-generated animation that is seamless in **space**: adjacent tiles share their border pixels exactly. It must also be seamless in **time**: a tile's clip ends on the exact frame the next step's clip starts from. Orientation must come from the actual neighbor configuration. No rotating or mirroring a canonical sprite to force a match. Target playback is roughly 3–5 s per sim step.

## Status

- **v0, per-cell sprites (done, superseded).** One clip per cell transition, rotated toward a burning neighbor, generated with Wan2.1-VACE-1.3B first/last-frame interpolation between three keyframes. Time continuity works. Spatial seams are obvious: every tile has its own ground, and rotation flips flames and shadows. The code is still in the tree as the **Cell sprites** renderer (`spriteFrag`, `src/atlas.ts`, `tools/atlas/generate.py`). Delete it once v1 surpasses it.
- **v1, dual-grid tiles (in progress).** Described below.

## v1 design: dual grid

Tiles are centered on grid **vertices**, not cells. A tile covers one quarter of each of the 4 cells around its vertex, so its content depends only on those 4 cells:

```
 NW | NE        config = s_SW + K·s_SE + K²·s_NW + K³·s_NE
 ---+---        (cells at vertex v: SW=(v.x-1, v.y-1), SE=(v.x, v.y-1),
 SW | SE                            NW=(v.x-1, v.y),   NE=(v.x, v.y))
```

- In the renderer, for a pixel at cell-space position p: `v = floor(p + 0.5)` and `uv = p + 0.5 - v`. Cells outside the grid count as EMPTY when wrap is off.
- Tile borders run through **cell centers**. A tile's vertical edge depends only on the 2 vertically adjacent cells it passes through. A horizontal edge depends only on the 2 horizontally adjacent cells. Each tile corner is one cell center, so it depends on one cell.
- So if every edge strip is a fixed function of its cell pair, and every corner patch a fixed function of its cell, any two neighboring tiles match by construction. This is corner/Wang tiling.
- Direction comes from the configuration. For example, a burning SW cell next to a catching SE cell shows fire crossing that edge. There's no rotation.

### Vocabulary

- **Stills (step boundaries):** each cell is one of {EMPTY, TREE, FIRE}, so K=3 and there are 3⁴ = **81 stills**.
- **Clips (during a step):** each cell is one of {empty, tree, grow, catch-spread, catch-lightning, burnout}, so K=6 and there are 6⁴ = **1296 clips**.
  - A clip's first frame is the still of the previous states and its last frame is the still of the current states, which gives time continuity.
  - Idle-only configs (every cell empty or tree) loop instead of playing once.

### Generation: hierarchical, borders frozen

Each level is inpainted with the level below it frozen (VACE spatial masks; SD inpainting is a fallback). Borders therefore match exactly instead of approximately.

1. **World/style pass.** Generate one image of a small toroidal world in which every 2×2 configuration appears, ideally a 9×9 de Bruijn torus over 3 states. Condition it on a rough layout render, either as a VACE control video or via img2img from the placeholder. This fixes the art style. Crop the corner patches from it.
2. **Corners:** 3 patches for stills (6 for clips), one per cell state, each centered on a cell center.
3. **Edges:** 9 vertical and 9 horizontal strips for stills (36 + 36 animated strips for clips). Each strip straddles a tile seam, and is inpainted with its two end corners frozen.
4. **Tiles:** 81 stills or 1296 clips, with the interior inpainted and all four edge strips frozen. For clips, the first and last frames are also frozen to the matching stills.

### Placeholder rule (also the rule generated tiles must obey)

A cell's content is drawn centered on a tile corner and never reaches more than one cell from its center. Overlapping effects are drawn in a fixed order by layer, never by cell. With that, the 4 tiles sharing a cell all produce identical pixels for it.

### Atlas constraints

- WebGL2 only guarantees 256 texture-array layers; typical hardware allows 2048. 81 stills fit as layers. 1296 clips × 16 frames does not, so clips will need **pages**: each layer becomes a grid of tiles, with gutters and clamped LOD.
- 1296 × 16 frames × 64² × 4 B ≈ 340 MB uncompressed. Plan on KTX2/Basis compression, fewer frames, or smaller tiles.
- Binary assets live outside the repo in `$TINDERBOX_DATA` (default `~/.local/share/tinderbox`). `vite.config.ts` serves `$TINDERBOX_DATA/atlas/` at `/atlas/`.

## Milestones

- [x] v0 per-cell renderer, procedural placeholder, Wan VACE generator, turnstile lease client
- [x] Binary assets moved outside the repo, plus a Vite plugin to serve them
- [x] **M1: dual-grid renderer** (`dualFrag`, `loadTiles`/`placeholderTiles` in `src/atlas.ts`, plus a renderer select in the panel). It's checked in the browser: fire glow crosses tile borders seamlessly, canopies assemble from 4 quarters centered in their cells, and wrap and no-wrap edges both work. Within a step it currently **crossfades** from the previous configuration's still to the current one. M3 replaces that with clips.
- [ ] **M2: generated stills.** World pass → corners → edges → 81 tiles, written as a v1 stills atlas. Success means no visible seams at 32–64 px cells.
- [ ] **M3: generated clips.** Animated corners and edges → 1296 clips (batch several tiles per VACE run), paged atlas, transition playback over the step. Probably an overnight job on the 3080.
- [ ] **M4: polish.** Texture compression, delete v0, and README/screenshots.

## Next steps

1. M2: write `tools/atlas/tiles.py`, kept separate from the v0 `generate.py`. Start with the 9×9 de Bruijn torus search and the world pass, then inspect the world image before building corners and edges.
2. Output format (already read by `loadTiles`): `$TINDERBOX_DATA/atlas/tiles.json` `{"version": 1, "tile": N}` and `tiles.png`, a 9×9 grid of N-px tiles, row-major, where index = SW + 3·SE + 9·NW + 27·NE. Rows run top-down, so a tile's top edge is north.

## Open questions

- World pass conditioning: VACE control video vs. SDEdit from the placeholder render. Try VACE control first.
- Seam width for frozen strips. Start at 1/8 of the tile (it must be a multiple of the VAE's 8 px).
- Wan 1.3B quality at small tiles. Generate at ≥128 px per tile and downscale.

## Environment notes (bifrost)

- **GPU:** RTX 3080, 10 GB, shared through turnstile (`tools/atlas/turnstile.py`). Without `TURNSTILE_API_KEY` the generator queues at priority 0.
- **Text encoder:** UMT5-XXL is about 11 GB in bf16 and doesn't fit on the GPU. `generate.py` encodes prompts on the CPU (`embed()`) and loads the pipeline with `text_encoder=None`.
- **IPv6** to Hugging Face times out on bifrost. Download weights once with IPv4 forced (patch `socket.getaddrinfo` to `AF_INET` and set `HF_HUB_DISABLE_XET=1`), then run with `HF_HUB_OFFLINE=1`. The weights (~23 GB) are already cached.
- **Python venv:** `tools/atlas/.venv` (gitignored). Recreate it with `pip install -r tools/atlas/requirements.txt`.

## Log

- **2026-10-08:** Built v0 end to end and generated a real atlas. The spread midpoint wipe reliably fixes fire direction. Keyframe lessons: the model ignites late in a burn clip, so pick the most-fire frame. The empty keyframe needs its own text-to-image pass. Loops with identical first and last frames come out nearly static. The user rejected v0 for its seams and rotation, which led to the v1 dual-grid design above.
