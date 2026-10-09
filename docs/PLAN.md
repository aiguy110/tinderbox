# Phase 2 plan: seamless diffusion tiles

Living document. Update it as you go (see CLAUDE.md): tick milestones, record decisions and why, and keep **Next steps** accurate for whoever picks this up next.

## Goal

Render each cell with diffusion-generated art that is seamless in **space**: adjacent tiles share their border pixels exactly. It must also be seamless in **time**: what a tile shows at the end of a step is exactly what the next step starts from. Orientation must come from the actual neighbor configuration. No rotating or mirroring a canonical sprite to force a match. Target playback is roughly 3–5 s per sim step.

The user also wants (2026-10-08): a **forest stand per cell** rather than a single tree, fire that visibly **enters from the side it came from**, a step of **plain burning**, and a **burnt stand for one step** before the cell returns to grass. They clarified afterwards:
- Forest cells should be **uniform pine forest**, drawn by new art, not by tiling single-tree stills into sub-cell groves. Groves were removed.
- A burnt cell becomes **charred dirt** that turns back into **lush grass gradually, over about 10 steps**. The regrowth can be procedural, and few keyframes are needed.

**Direction as of 2026-10-09:** the five-look diffusion stills (M4) didn't work out (see *M4 attempt*), so the renderer is now **100% procedural** (v2). For it, the user asked for:
- **many individual pines per forest cell**, catching in an order set by where the fire came from;
- **nicely animated flames** (particles if affordable);
- **grass regrowing gradually** after a burn;
- **saplings growing over a forest's first two steps**;
- "any other nice touches".

## Status

- **v0, per-cell sprites (done, removed from the app).** One clip per cell transition, rotated toward a burning neighbor, generated with Wan2.1-VACE-1.3B first/last-frame interpolation between three keyframes. Spatial seams were obvious. Only `tools/atlas/generate.py` remains; `tiles.py` still uses its keyframes in `work/keyframes/`.
- **v1, dual-grid tiles (in progress).** Stills per vertex configuration, with transitions drawn by the shader. Described below. The renderer and sim are ready for five looks; there is no five-look atlas, so the app currently draws the **procedural placeholder** tiles (`placeholderTiles` in `src/atlas.ts`).
- **v2, fully procedural (first cut, the default renderer).** `sceneFrag` in `src/scene.ts`; see *v2 design*. The tiles renderer (v1) and flat colors remain selectable.

## v2 design: procedural scene (`src/scene.ts`)

Everything is a function of world position p (in cells), the 4–16 nearby cell texels, the step progress s and wall time. No tiles, so there are no seams to manage. The view is **oblique**: the ground is seen from above, and pines stand up on it in profile, overlapping the trees behind.

- **Tree slots.** Each cell owns an n×n jittered grid of slots (`uTrees`, the "Trees per side" slider, default 4; capped so slots are ≥ 4 px apart). 10% of slots are gaps. A slot's position, height (1.55–1.9 slot spacings) and seed are hashes of its world slot index, so a regrown forest has the same trees as before.
- **The window.** A pixel evaluates a fixed 3×4 window of slots (its own column ±1, its row and the 3 rows below), back to front. Every tree's crown, flames (≤ 1.6 tree heights, leaning ≤ 0.15), shadow (offset right, short below) and snag tilt are sized to stay inside the slots that can see them, so nothing is clipped at window boundaries. **If you make trees taller, flames higher or shadows longer, recheck these bounds** (comment in `drawPine`).
- **Timeline per cell** (`fireAge`): 0–1 catching, 1–2 burning, 2–3 burnt, then 3 + ground age (`groundAge` in `common`). Per tree, `tau = fireAge - ignition`. Ignition is `arrival()` at the tree's position (0.85×) plus jitter, so the stand catches in order from the burning neighbors, or outward from the strike point. Everything that depends on ignition (char, flame ramp-up, embers) saturates by the end of the catching step's worst case, so the burnt step (whose A no longer has the mask) can use ignition 0 without a jump.
- **Fire:** flames flare up the crown (`climb`), then die back to about half height as the needles burn away (the crown is thresholded by noise rising with char, leaving trunk and branch stubs). Flames end per tree between fireAge 2.2 and 2.95. Shapes are a teardrop width profile distorted by scrolling fbm and vertical tongues, with a per-tree pulse, and composited semi-additively over the tree. Lightning flashes the tree nearest the strike.
- **Snags:** after the burnt step, each slot's snag stands until fireAge 3.8–7.3, then tips (≤ 0.35 rad) and crumbles over 0.7 steps. A burning tree with char 1 and no flame is drawn by the same function as a standing snag, so the hand-over is exact. A sapling can grow in front of a snag.
- **Growth:** a new cause, `MATURE`, marks a tree's second step (`stepFrag` sets it after `GROW` and keeps the ground age in A through both). Saplings grow from nothing to full size over those two steps, brighter green while young. The forest-floor litter comes in with them.
- **Ground:** grass (regrowth), needle litter and ash amounts are computed per cell at the pixel (ash follows the front during catch), bilinearly blended between cell centers through a noise-warped lattice, then thresholded by noise, so changes come in patches. Ash washes into dirt over ~3 steps after the clear; embers glow on it through the burnt step.
- **Sky effects** from a bilinear per-cell heat field (`heatAt`): fire light (warm, flickering; much stronger at night), smoke sampled from the heat field below the pixel and drifting with the wind, and sparks: two lattices of particles rising on the wind, each lit if there was fire where it came from and fading as it rises. Particles are evaluated in the shader, not simulated, so they cost a few hashes and one `heatAt` per layer.
- **Wind** is a slow function of time: it sways the trees and leans flames, smoke and sparks.
- **Night** (toggle): moonlit ambient light, with fire light and emissive flames on top.
- **Known limitation:** a sapling that catches during its two growing steps jumps to full size, because A is the sources mask while burning and the size isn't kept. Rare at the usual p.
- **Cost:** about 50 texel fetches per pixel plus noise, with early-outs for slots whose tree misses the pixel. Not yet measured on a real GPU (Playwright here is SwiftShader, about 1 fps at 560×360).

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

- **Sim states:** EMPTY, TREE, FIRE, BURNT. A tree that catches is FIRE for two steps (catching, then burning; it spreads during both), then BURNT for one step, then EMPTY. The step shader keys this off the previous state in G (FIRE with prev TREE → keep burning).
- **Ground age:** for EMPTY cells, A holds the steps since the ground burned. It's 0 on the CLEAR step and counts up to 254; 255 (`SETTLED`) means it never burned, and seeding and off-grid cells use it. A sapling (EMPTY→TREE) keeps the age of the ground it grew from, so the renderer can draw what it grows over. The rule ignores the age; it's for drawing only.
- **Looks:** the atlas is indexed by look, not by state: `grass, forest, fire, burnt, dirt` (K=5, 5⁴ = **625 stills**, `LOOKS` in `src/shaders.ts`, which must match `tools/atlas/stills.py`). TREE→forest, FIRE→fire, BURNT→burnt. EMPTY blends dirt→grass by regrowth = age / `uRegrow` (the Regrowth slider, default 10 steps).
- **M2 stills (superseded):** {EMPTY, TREE, FIRE}, K=3, 81 stills, in `tiles.png`. The current renderer no longer reads them.
- **Clips (parked):** M3 generated one clip per transition configuration, with each cell playing one of {empty, tree, grow, catch, burnout}: K=5, 5⁴ = **625 clips**. They're no longer played; see *Why transitions moved to the shader*.
  - Spread and lightning are both "catch". A cell's own corner piece is shared by all 4 tiles around it, so it can't depend on where the fire came from anyway; direction can only show in strips and tile interiors. Splitting them would mean 1296 clips for a lightning flash.
  - A clip's first frame is the still of the previous states and its last frame is the still of the current states, which gives time continuity.
  - Every clip, idle ones included, plays once over the step (`progress` 0→1). Idle-only tiles start and end on the same still, so they don't pop.

### Generation: collage → harmonize → stitch (what works, `tools/atlas/tiles.py`)

1. **World layout.** A 12×12 non-wrapping grid in which every 2×2 configuration appears, found by annealing. It's rendered as a **sharp collage** of the v0 keyframes: ground texture, plus tree or burning-tree discs centered on cells.
2. **Harmonize.** One Wan VACE pass, used as plain text-to-image with SDEdit. The latents are held on the noised collage's trajectory for the first 40% of 40 flow-matching Euler steps, then denoised freely. Since the collage is identical at every cell of a state, all occurrences of a border piece come out nearly identical.
3. **Stitch** (pixel space). Border pieces are keyed by what they may depend on: a corner (one state) around each cell center, and h/v strips (a pair of states) along the lines between centers. The medoid occurrence of each key is pasted over all the others with feathered weights. Corners have full weight wherever the perpendicular strips overlap, so the tile-border lines are exact functions of their cells.
4. **Crop.** Each configuration is taken from its most typical occurrence (lowest summed per-cell deviation from that state's mean) and written to the 9×9 atlas.

### Shader transitions (`dualFrag`)

Over a step each cell blends, per pixel, from its previous look to its current one with a weight that is a function of **world position** and the cell's data. A pixel mixes the stills for every combination of its 4 corners' looks, weighted multilinearly (up to 16 tile lookups, usually 1). The weights depend only on the cell and the world position, so both tiles along a shared border compute the same thing and the result stays seamless.

- **Catch:** a front crosses the cell over the step. Its arrival time comes from the sources mask in A (distance from the burning edges or corners), or radially from a hashed point for lightning. It's ragged from fbm noise, has flame tongues along it, and casts a warm light on the canopy just ahead.
- **Burning:** FIRE stills, with flames that flicker (a color-keyed mask times scrolling noise).
- **Burn-down** (FIRE→BURNT) and **clear** (BURNT→EMPTY) happen in noisy patches. Embers sparkle on charred wood.
- **Grow:** the TREE look spreads out from the middle of the cell.
- **Clear:** BURNT→EMPTY blends the burnt look to dirt in patches.
- **Regrow:** an EMPTY cell blends dirt→grass. The grass coverage at a pixel is a noise threshold (`grassAt`) on regrowth, which moves from (age−1)/R to age/R over the step. Grass comes back in patches that spread and join, and is continuous in time.
- **Up to 3 looks per corner.** A sapling on half-regrown ground is dirt, grass and forest at once. So each corner carries 3 looks with weights, and the blend loops over 3⁴ = 81 combinations, skipping zero weights (usually 1 lookup).
- **Fire light and smoke** are bilinear over the dual grid.
- **Removed:** groves (n×n sub-cells per cell) and the charred-TREE stand-in for BURNT, both replaced by real looks.

### Lightning bolts (`src/lightning.ts`)

A lightning ignition is punctuated by a procedural bolt from the top of the screen to the spot in the cell where `arrival()` starts the fire (the CPU ports the shader hash to find it). A dim leader descends over 70 ms, then the return stroke flashes, with one or two restrikes and a faint sky flash; it's gone after 0.75 s of wall time, whatever the step rate.

- **Knowing where it struck.** Lightning is rolled per cell on the GPU, so the CPU can't see it. Instead, up to `BOLTS_PER_SECOND` (3) strikes per second are picked on the CPU as a short Poisson-sampled list of cells (`uStrikes`), and the shader's per-cell rate is reduced by the same amount, so the total process is unchanged. After the step, only the listed cells are read back (sync `readPixels`, cheap at this rate); those that became FIRE with cause LIGHTNING get a bolt. A listed strike on bare ground, or on a tree that caught by spread anyway, shows nothing.
- **Drawing.** Midpoint-displaced polyline with forks, drawn as capsule quads into an offscreen buffer with MAX blending (so joints and overlapping glow don't double), then added onto the scene. Clicking to ignite also throws a bolt (throttled while dragging).
- **Testing tip:** Playwright's `page.clock` fake timers hang this page. To inspect a bolt frame, wrap `Lightning.render` to pass a fixed time instead.

#### Why transitions moved to the shader

Showing which side fire entered needs the cell to remember the direction; a tile containing only the far half of the cell can't see the source otherwise. That turns catch into 4–5 variants. Together with burning and burnt, each corner has about 10 transitions, so there would be 10⁴ = 10,000 clips (about 3.5 GB and 9 GPU hours at M3 settings, against 220 MB and 35 min for 625). The M3 clips were also mushy in exactly the part a shader does well: a front moving across a cell.

### Clips: the same, in time (`tools/atlas/clips.py`, parked)

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
- [x] **M3: generated clips** (first cut, parked; the playback code was removed, and the generator still targets the old 3-state sim). `clips.py` makes 625 clips × 17 frames (14 worlds, about 2.5 min each); `loadClips` and `transition()`/`clipFrame()` in `dualFrag` play them over the step, and the stats line says "+ clips" when they've loaded. Checked in the browser: saplings grow, ignitions and burnouts play, and the tiles join. Known blemishes: burnout is smoky and mushy mid-clip, fire direction across edges isn't clearly legible, and the faint green fleck is still in the ground.
- [x] **M3b: shader transitions.** Two-step fire, a BURNT state, directional fronts, flicker, embers, smoke and fire light, plus groves. The v0 renderer is removed from the app. Checked in the browser at 80 px and 24 px cells.
- [~] **M4: forest stills (attempted, on hold).** The renderer side is done: five looks, ground age and regrowth, a Regrowth slider, and groves and the charring removed. Generation in `tools/atlas/stills.py` produced good key textures, but no harmonize strength gave usable worlds. See *M4 attempt*.
- [x] **M4b: procedural renderer** (first cut). `sceneFrag`: individual pines, per-tree ignition, flames, snags, sparks, smoke, regrowth, two-step growth, night. Checked in the browser (SwiftShader) at 64 px cells through catch, burn, burnt, clear, regrowth and saplings, by day and night. Needs tuning in motion on a real GPU.
- [ ] **M5: polish.** README/screenshots; delete `clips.py`/`generate.py` once nothing needs them.

## M4 attempt: five-look stills (`tools/atlas/stills.py`)

The M2 recipe with five looks, so 625 configurations:

1. **Keys.** One 512² texture per look, cropped to 192 px and scaled to one 96 px cell (pine crowns about 20 px), then made periodic with period C. The texture is cross-faded near the edges with itself shifted by half a period, so runs of one look are seamless in the collage. Results are in `work/stills/keys/`. They're good:
   - forest, grass and dirt are text-to-image;
   - fire and burnt are SDEdits of the forest key (same trees), after a pixel-space prep. Without the prep the model keeps the canopy green: plain SDEdit at 0.75–0.8 gave a perspective forest, or a few flames on a green canopy. `prepare()` paints orange blobs for `ignite`, and for `char` turns crowns black and gaps to ash.
   - Prompt lessons: "aerial drone photo looking straight down at a lawn" gives top-down grass, where "meadow" gave side-on blades. Burnt needs "charred crowns seen from directly above", or the model draws perspective snags.
2. **Worlds.** A greedy cover of 6 non-wrapping 12×12 worlds that contain all 625 configs (`work/stills/worlds.json`).
3. **Collage.** Each cell shows its look's periodic texture in a ragged mask grown by a per-look margin. Layer order is grass, dirt, burnt, forest, fire.
4. **Harmonize** (`tiles.harmonize`, one SDEdit pass per world). **This is where it failed:**
   - 0.6 and 0.5 ignore the layout: a perspective forest scene, burnt cells turned to dirt or forest, fire cells turned green.
   - 0.4 keeps the layout but stays soft and blocky (square forest patches), and the periodic cross-fade shows as diagonal streaks in fire cells.
   - Probes are in `work/stills/probe_s0.{4,5,6}.jpg`. The stitch and pack stages are written but have never run.
   - Untried ideas: a more natural collage (ragged canopy edges with shadows, less periodic textures) at about 0.5; or inpainting that keeps cell interiors and repaints only borders.

## Next steps

1. **Get the user's verdict on the scene in motion** (64–96 px cells, 3–5 s per step), and measure fps on their GPU. If it's slow: fewer smoke/glow `heatAt` samples, or render the heat field to a small texture once per frame.
2. Tuning candidates noticed so far: flames are still fairly uniform cones from a distance; the ground under a burning stand reads as a squarish ash patch; dirt may be too orange. Possible touches: heat shimmer above flames, scorch marks where lightning strikes, crown fire jumping between trees, birds or deer fleeing, a day/night cycle instead of a toggle.
3. Decide whether to keep the tiles renderer and `atlas.ts` around, or delete them with `stills.py` (M5).
4. Get the user's verdict on the lightning bolts (look, duration, bolts per second).
5. Format reference, in case stills come back: `stills.json` `{"version": 1, "tile": N, "looks": [...]}` + `stills.png`, a K²×K² grid with index = SW + K·SE + K²·NW + K³·NE, rows top-down. `loadTiles` rejects an atlas whose `looks` don't match `LOOKS`.

## Open questions

- World pass conditioning: VACE control video vs. SDEdit from the placeholder render. Try VACE control first.
- Seam width for frozen strips. Start at 1/8 of the tile (it must be a multiple of the VAE's 8 px).
- Wan 1.3B quality at small tiles. Generate at ≥128 px per tile and downscale.

## Environment notes (bifrost)

- **GPU:** RTX 3080, 10 GB, shared through turnstile (`tools/atlas/turnstile.py`). Without `TURNSTILE_API_KEY` the generator queues at priority 0.
- **Text encoder:** UMT5-XXL is about 11 GB in bf16 and doesn't fit on the GPU. `generate.py` encodes prompts on the CPU (`embed()`) and loads the pipeline with `text_encoder=None`.
- **IPv6** to Hugging Face times out on bifrost. Download weights once with IPv4 forced (patch `socket.getaddrinfo` to `AF_INET` and set `HF_HUB_DISABLE_XET=1`), then run with `HF_HUB_OFFLINE=1`. The weights (~23 GB) are already cached.
- **Max world size:** 1152² single-frame VACE fits the 3080; 1536² fails (`CUSOLVER_STATUS_INTERNAL_ERROR`, really OOM). A world pass takes about 100 s including model load. As of 2026-10-09 the 1152² VAE decode also OOMs (asks for 486 MiB with 173 free) unless run with `PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True`.
- **Wan's comfortable scale:** trees about 100–150 px across. At 64 px cells it draws mush.
- **Video pass:** 768² × 17 frames fits in 10 GB only with `vae.enable_tiling()`, since the fp32 VAE encode runs out of memory otherwise. At about 3.6 s per step, a world takes about 2.5 min including load.
- **Live app:** `tinderbox-build.service` (user unit, `tools/systemd/`) runs `vite build --watch` in the main checkout. Tailscale serves `dist/` at `https://bifrost.tail2280a0.ts.net:8451/`. It replaced the old :5191 dev-server unit on 2026-10-08. See CLAUDE.md.
- **GPU queue:** the generators queue at priority 0, so a named consumer (xad-pod, 30) can hold the GPU for 15–20 minutes at a time. That's expected. Check with `curl -s localhost:8080/api/queue`.
- **Background jobs:** launch with `setsid nohup … < /dev/null & disown`, or the job dies with the agent session. Don't detect them with `pgrep -f clips.py`, which matches the shell running pgrep itself. Use `ps -eo args | grep "[.]venv/bin/python clips.py"`.
- **Inspecting the scene in Playwright:** WebGL there is SwiftShader, about 1 fps at 560×360; bigger viewports make screenshots time out. In dev, `window.tinderbox` has `sim`, `params`, `step()` and `setProgress(x)`: set `params.paused = true` and `params.stepsPerSecond = 0.001`, call `sim.ignite(x, y)` (CSS px), `step()` a few times, then `setProgress(0.5)` and screenshot. Saved params come from localStorage `tinderbox:params` (`paused` is never restored).
- **Python venv:** `tools/atlas/.venv` (gitignored). Recreate it with `pip install -r tools/atlas/requirements.txt`.

## Log

- **2026-10-08:** Built v0 end to end and generated a real atlas. The spread midpoint wipe reliably fixes fire direction. Keyframe lessons: the model ignites late in a burn clip, so pick the most-fire frame. The empty keyframe needs its own text-to-image pass. Loops with identical first and last frames come out nearly static. The user rejected v0 for its seams and rotation, which led to the v1 dual-grid design above.
- **2026-10-08 (later):** M2. Tried hierarchical frozen inpainting, VACE layout control, blurry-layout SDEdit/ILVR and per-step latent sync; all failed (see *What didn't work*). What works is a sharp keyframe collage, an SDEdit harmonize pass and a pixel-space stitch with medoid leaders. Verified seamless in the browser.
- **2026-10-08 (evening):** M3 started. A plain SDEdit video pass repaints the ground over the clip (smooth at frame 0, rocky by frame 16); VACE keep-frames at both ends, set to the still worlds, fix it.
- **2026-10-08 (night):** M3 first cut: 625 clips, played back in the dual renderer. A clean-soil ground keyframe made the stills drift (see *What didn't work*), so the old ground stays. Stills now stitch with per-pixel-median leaders. The dev server became a systemd unit. bifrost rebooted mid-run once; jobs launched without `setsid` die with their session.
- **2026-10-08 (late night):** The user asked for forest stands, directional fire entry, a burning step and a burnt step. Per-config clips with direction would be about 10,000 clips, so transitions moved to the shader (M3b): world-space blend weights over the stills, multilinear across a tile's corners. Groves (n×n sub-cells per cell) give a stand of trees without new art; real forest stills are M4.
- **2026-10-08/09 (night):** The user clarified: real pine-forest art, not groves, and charred dirt greening back over about 10 steps. Built five looks, ground age in A, regrowth in the shader, and `stills.py`. Keys came out well after pixel-space prep for fire and burnt; harmonize at 0.4/0.5/0.6 didn't give usable worlds (see *M4 attempt*). The live app falls back to placeholder tiles. The user's next direction: a fully procedural, shader-heavy renderer. An unsetsid'd probe died with the session; keep using `setsid nohup`.
- **2026-10-09:** v2 first cut: the procedural scene renderer, now the default (the saved renderer `dual` migrates to `scene`; tiles are `tiles`). Added `MATURE` and a continuous `groundAge`. Lessons: low-temperature flame fragments from high-frequency noise read as black specks, so flame alpha needs a threshold; flames must fade before the slot window's edge or they get flat tops; reversed `smoothstep` edges are undefined in GLSL, so use `1 - smoothstep(lo, hi, x)`.
