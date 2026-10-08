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
- **Clips (during a step):** each cell plays one of {empty, tree, grow, catch, burnout}, so K=5 and there are 5⁴ = **625 clips**.
  - Spread and lightning are both "catch". A cell's own corner piece is shared by all 4 tiles around it, so it can't depend on where the fire came from anyway; direction can only show in strips and tile interiors. Splitting them would mean 1296 clips for a lightning flash.
  - A clip's first frame is the still of the previous states and its last frame is the still of the current states, which gives time continuity.
  - Every clip, idle ones included, plays once over the step (`progress` 0→1). Idle-only tiles start and end on the same still, so they don't pop.

### Generation: collage → harmonize → stitch (what works, `tools/atlas/tiles.py`)

1. **World layout.** A 12×12 non-wrapping grid in which every 2×2 configuration appears, found by annealing. It's rendered as a **sharp collage** of the v0 keyframes: ground texture, plus tree or burning-tree discs centered on cells.
2. **Harmonize.** One Wan VACE pass, used as plain text-to-image with SDEdit. The latents are held on the noised collage's trajectory for the first 40% of 40 flow-matching Euler steps, then denoised freely. Since the collage is identical at every cell of a state, all occurrences of a border piece come out nearly identical.
3. **Stitch** (pixel space). Border pieces are keyed by what they may depend on: a corner (one state) around each cell center, and h/v strips (a pair of states) along the lines between centers. The medoid occurrence of each key is pasted over all the others with feathered weights. Corners have full weight wherever the perpendicular strips overlap, so the tile-border lines are exact functions of their cells.
4. **Crop.** Each configuration is taken from its most typical occurrence (lowest summed per-cell deviation from that state's mean) and written to the 9×9 atlas.

### Clips: the same, in time (`tools/atlas/clips.py`)

1. **Worlds.** 14 non-wrapping 8×8 transition grids (768², one video pass each) that together contain all 625 configurations, found by greedy annealing.
2. **Animated collage**, 17 frames: a sapling scaling up (the v0 grow clip pops in one frame), the v0 ignition clip, and flames from the v0 burnout clip fading out into ground. Each sprite's ends match the still collage exactly.
3. **Harmonize** as one 17-frame Wan VACE video pass per world, SDEdit at strength 0.6. The schedule is truncated to start at the strength's step, which skips the discarded steps. **Frames 0 and 16 are given to VACE as keep-frames (conditioning scale 1), set to the M2 still worlds of the previous and current states** (assembled from atlas tiles). Without them the model slowly repaints the ground over the clip, so the middle of every clip would have different ground than its ends.
4. **Stitch** as for stills, but over all worlds at once. One leader per key, the medoid over every occurrence with all frames taken together, is pasted into every frame.
5. **Pin** the first and last 4 frames into the exact still worlds with a linear ramp, so clips start and end on M2 tiles to the pixel.
6. **Pack:** each config is cropped from its most typical occurrence and downscaled to 64 px.

#### What didn't work (don't retry without a new idea)

- **Hierarchical inpainting with frozen corners/strips** (VACE masks, re-pasted after each pass). The model treats the frozen lattice as content and paints a grid pattern; tile interiors ignore the configuration.
- **VACE control from a color-blob layout.** It's bimodal: at conditioning scale 0.5 the layout is ignored; at 1.0 it's copied literally as flat blobs.
- **SDEdit or ILVR (low-pass) guidance from a blurry layout.** The model keeps the blur: either blurry and on-layout, or sharp and off-layout.
- **A clean ground keyframe** (plain soil, no grass tufts, to get rid of the green fleck). The stronger high-frequency texture gave the model room to drift at strength 0.6: trees ballooned and merged, flames lost their scorch discs, and ground came out reddish. The candidate is kept as `work/keyframes/ground_candidate.png`; the outputs are in `work/tiles/cleanground/`.
- **Synchronizing latents during denoising** (averaging, or copying a leader occurrence, after every step, with identical start noise). Both turn the image into a degenerate repeated pattern. With UniPC it's worse, because the corrector reuses its stored pre-callback sample. If you edit latents in a callback, use `FlowMatchEulerDiscreteScheduler(shift=3.0)`.

### Placeholder rule (also the rule generated tiles must obey)

A cell's content is drawn centered on a tile corner and never reaches more than one cell from its center. Overlapping effects are drawn in a fixed order by layer, never by cell. With that, the 4 tiles sharing a cell all produce identical pixels for it.

### Atlas constraints

- WebGL2 only guarantees 256 texture-array layers; typical hardware allows 2048. 81 stills fit as layers. 625 clips × 17 frames do not, so clips use **pages**: `clips-NN.png` holds frame NN of every clip, as a 25×25 grid of 64 px tiles, each padded by a 4 px edge-extended gutter (1800² px). The texture array has one layer per frame, and `TEXTURE_MAX_LOD` is clamped to 2 so mips never mix neighbors on a page.
- That's 17 × 1800² × 4 B ≈ 220 MB uncompressed (≈290 MB with mips). Texture compression (KTX2/Basis) or fewer frames is M4.
- `clips.json`: `{"version":1,"tile":64,"gutter":4,"frames":17,"columns":25,"transitions":[...]}`. Clip index = SW + 5·SE + 25·NW + 125·NE over transitions in that order. The renderer maps (current, previous) state to a transition in `transition()` in `dualFrag`.
- Binary assets live outside the repo in `$TINDERBOX_DATA` (default `~/.local/share/tinderbox`). `vite.config.ts` serves `$TINDERBOX_DATA/atlas/` at `/atlas/`.

## Milestones

- [x] v0 per-cell renderer, procedural placeholder, Wan VACE generator, turnstile lease client
- [x] Binary assets moved outside the repo, plus a Vite plugin to serve them
- [x] **M1: dual-grid renderer** (`dualFrag`, `loadTiles`/`placeholderTiles` in `src/atlas.ts`, plus a renderer select in the panel). It's checked in the browser: fire glow crosses tile borders seamlessly, canopies assemble from 4 quarters centered in their cells, and wrap and no-wrap edges both work. Within a step it currently **crossfades** from the previous configuration's still to the current one. M3 replaces that with clips.
- [x] **M2: generated stills** (first cut). `tools/atlas/tiles.py` writes a 96 px 9×9 atlas, and it renders seamlessly in the dual renderer. Known blemishes: faint green flecks on some ground tiles, left from cells where the model drew something atypical. The art style is inherited from the photoreal-ish v0 keyframes.
- [x] **M3: generated clips** (first cut). `clips.py` makes 625 clips × 17 frames (14 worlds, about 2.5 min each); `loadClips` and `transition()`/`clipFrame()` in `dualFrag` play them over the step, and the stats line says "+ clips" when they've loaded. Checked in the browser: saplings grow, ignitions and burnouts play, and the tiles join. Known blemishes: burnout is smoky and mushy mid-clip, fire direction across edges isn't clearly legible, and the faint green fleck is still in the ground.
- [ ] **M4: polish.** Texture compression, delete v0, and README/screenshots.

## Next steps

1. Judge M3 in motion at 3–5 s per step (`stepsPerSecond` about 0.25) and decide what to fix. Candidates: the burnout sprite (the v0 burn frames are mushy), fire direction across edges, and the clip length.
2. Polish the stills and clips together, since the clips are pinned to the stills: the green fleck in the ground (a tuft in the `empty.png` keyframe that the model paints green; try inpainting the tuft out of the keyframe itself, not a new texture), the photoreal-ish style, and a scorched look for EMPTY. If the stills change, `python clips.py pack` re-pins the existing clips with no GPU; re-run `gen` only if the look changes.
3. M4: texture compression for the 17 × 1800² pages, delete v0, README and screenshots.
4. Format reference (read by `loadTiles`/`loadClips`): see *Atlas constraints*; stills are `tiles.json` `{"version": 1, "tile": N}` + `tiles.png`, a 9×9 grid with index = SW + 3·SE + 9·NW + 27·NE, rows top-down.

## Open questions

- World pass conditioning: VACE control video vs. SDEdit from the placeholder render. Try VACE control first.
- Seam width for frozen strips. Start at 1/8 of the tile (it must be a multiple of the VAE's 8 px).
- Wan 1.3B quality at small tiles. Generate at ≥128 px per tile and downscale.

## Environment notes (bifrost)

- **GPU:** RTX 3080, 10 GB, shared through turnstile (`tools/atlas/turnstile.py`). Without `TURNSTILE_API_KEY` the generator queues at priority 0.
- **Text encoder:** UMT5-XXL is about 11 GB in bf16 and doesn't fit on the GPU. `generate.py` encodes prompts on the CPU (`embed()`) and loads the pipeline with `text_encoder=None`.
- **IPv6** to Hugging Face times out on bifrost. Download weights once with IPv4 forced (patch `socket.getaddrinfo` to `AF_INET` and set `HF_HUB_DISABLE_XET=1`), then run with `HF_HUB_OFFLINE=1`. The weights (~23 GB) are already cached.
- **Max world size:** 1152² single-frame VACE fits the 3080; 1536² fails (`CUSOLVER_STATUS_INTERNAL_ERROR`, really OOM). A world pass takes about 100 s including model load.
- **Wan's comfortable scale:** trees about 100–150 px across. At 64 px cells it draws mush.
- **Video pass:** 768² × 17 frames fits in 10 GB only with `vae.enable_tiling()`, since the fp32 VAE encode runs out of memory otherwise. At about 3.6 s per step, a world takes about 2.5 min including load.
- **Dev server:** `tinderbox-dev.service` (user unit, `tools/systemd/`) runs Vite from the main checkout on :5191. See CLAUDE.md.
- **GPU queue:** the generators queue at priority 0, so a named consumer (xad-pod, 30) can hold the GPU for 15–20 minutes at a time. That's expected. Check with `curl -s localhost:8080/api/queue`.
- **Background jobs:** launch with `setsid nohup … < /dev/null & disown`, or the job dies with the agent session. Don't detect them with `pgrep -f clips.py`, which matches the shell running pgrep itself. Use `ps -eo args | grep "[.]venv/bin/python clips.py"`.
- **Python venv:** `tools/atlas/.venv` (gitignored). Recreate it with `pip install -r tools/atlas/requirements.txt`.

## Log

- **2026-10-08:** Built v0 end to end and generated a real atlas. The spread midpoint wipe reliably fixes fire direction. Keyframe lessons: the model ignites late in a burn clip, so pick the most-fire frame. The empty keyframe needs its own text-to-image pass. Loops with identical first and last frames come out nearly static. The user rejected v0 for its seams and rotation, which led to the v1 dual-grid design above.
- **2026-10-08 (later):** M2. Tried hierarchical frozen inpainting, VACE layout control, blurry-layout SDEdit/ILVR and per-step latent sync; all failed (see *What didn't work*). What works is a sharp keyframe collage, an SDEdit harmonize pass and a pixel-space stitch with medoid leaders. Verified seamless in the browser.
- **2026-10-08 (evening):** M3 started. A plain SDEdit video pass repaints the ground over the clip (smooth at frame 0, rocky by frame 16); VACE keep-frames at both ends, set to the still worlds, fix it.
- **2026-10-08 (night):** M3 first cut: 625 clips, played back in the dual renderer. A clean-soil ground keyframe made the stills drift (see *What didn't work*), so the old ground stays. Stills now stitch with per-pixel-median leaders. The dev server became a systemd unit. bifrost rebooted mid-run once; jobs launched without `setsid` die with their session.
