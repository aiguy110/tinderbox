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
- **v1, dual-grid tiles (on hold, removed from the app in the Rust port).** Stills per vertex configuration, with transitions drawn by the shader. Described below. There was never a five-look atlas, so it only ever drew the procedural placeholder. The TypeScript renderer (`dualFrag`, `src/atlas.ts`) is in git history at `503601c` if stills come back.
- **v2, fully procedural (first cut, the default renderer).** `src/shaders/scene.wgsl`; see *v2 design*. Flat colors remain selectable.
- **Rust port (done 2026-10-09).** The app is now one Rust crate (wgpu + winit + egui) built for the web (trunk → wasm, WebGPU with a WebGL2 fallback) and natively, where it spans the scene across monitors. See *Rust port*.

## Rust port

The user asked (2026-10-09) to move to Rust so the same scene can render in a web canvas and directly on native monitors, with one scene spanning several monitors.

- **Stack:** wgpu 30, winit 0.30, egui 0.36 (+ egui-wgpu), Rust 1.95 pinned by `rust-toolchain.toml` (the system default toolchain is an old 1.81). Shaders were ported GLSL → WGSL by hand (`src/shaders/*.wgsl`), assembled with a constants prelude generated in `src/shaders.rs` (WGSL has no includes). Same state encoding, same algorithms, same look.
- **Layout and views** (`engine.rs`, `app.rs`): a *layout* is a list of view rectangles in global physical pixels plus a scale (physical px per logical px). The sim grid covers the layout's bounding box; cells in gaps between monitors are simulated but unseen. Each view (canvas, window, or monitor) draws its own rectangle of the same world: `View.origin`/`scale` in `view.wgsl` map a fragment to global cells, so the picture is continuous across monitor seams with nothing shared but the state texture. Mixed DPI: cells are sized in the *primary* monitor's logical px, so they're the same physical size everywhere.
- **Frame loop:** the primary view's `RedrawRequested` ticks the engine once, then draws every view and presents all of them. The primary surface uses Fifo (it paces frames); others use Mailbox where available, so they don't each wait for another vblank.
- **Per view:** scene → offscreen `Rgba8Unorm` target at the adaptive resolution → bolts (MAX-blended offscreen, then added) → linear blit to the surface → egui (primary only, full resolution). Surfaces use a non-sRGB format so colors are written as-is, like the WebGL canvas. (WebGL2 can't reinterpret surface formats; `SURFACE_VIEW_FORMATS` is checked.)
- **Lightning readback** is now asynchronous: after a step with listed strikes, a probe pass draws a 16×1 `Rgba8Unorm` texture (white where strike i lit a tree) and copies it to a mappable buffer. Hits arrive a frame or two later via `Sim::hits()`. Reading the `Rgba8Uint` state directly is avoided because WebGL2 readback of integer formats is fussy.
- **Uniforms per step:** `queue.write_buffer` lands at the start of the next submit, so each sim step is its own submit (up to 16 per frame).
- **Panel:** egui, styled like the old HTML panel. egui-winit doesn't build for wasm, so `src/input.rs` translates the few winit events the panel needs (pointer, wheel, touch).
- **Keep awake** (`src/awake.rs`): web: an inline-JS screen wake lock (web-sys hides the API behind an unstable cfg), re-requested on `visibilitychange` and on taps. Linux: `org.gnome.SessionManager.Inhibit` (flags idle|suspend) over zbus, else `org.freedesktop.ScreenSaver.Inhibit`; the inhibitor lasts as long as the D-Bus connection, so it lifts on exit or crash. Not in `--saver`, `--shot` or `--list-monitors`. Tested against a mock session manager under `dbus-run-session` + `xvfb-run`, and in Playwright Chromium (granted); not yet on a real GNOME desktop or phone.
- **Saved grid** (`src/save.rs`): the app (not `--shot` or `--saver`) copies the grid back every 5 s (`Sim::capture`, async map) and stores it deflated with a header (size, generation, cell size): localStorage `tinderbox:grid` as base64 on the web (~50 K chars for 260×165), `$XDG_STATE_HOME/tinderbox/grid.bin` natively, plus a blocking save on exit. On start (after the first resizes) it's restored if the cell size matches, overlap only if the grid size differs.
- **Params:** same JSON as before (camelCase), so the web build reads the TS app's saved `tinderbox:params`; natively `$XDG_CONFIG_HOME/tinderbox/params.json`. A saved `tiles` renderer becomes `scene`.
- **Native modes** (`tinderbox --help`): every monitor (default), `--monitors 0,2`, `--windowed`, `--views WxH+X+Y,...` (ordinary windows standing in for monitors), and `--shot DIR` (headless render of a virtual layout to PNGs, plus `layout.png` stitched from the views, for checking seams without a display).
- **Removed:** the tiles renderer and atlas loading (see Status), Node/Vite/TypeScript. `tools/atlas/` (Python) is untouched.

## v2 design: procedural scene (`src/shaders/scene.wgsl`)

Everything is a function of world position p (in cells), the 4–16 nearby cell texels, the step progress s and wall time. No tiles, so there are no seams to manage. The view is **oblique**: the ground is seen from above, and pines stand up on it in profile, overlapping the trees behind.

- **Tree slots.** Each cell owns an n×n jittered grid of slots (`uTrees`, the "Trees per side" slider, default 4; capped so slots are ≥ 4 px apart). 10% of slots are gaps. A slot's position, height (1.55–1.9 slot spacings) and seed are hashes of its world slot index, so a regrown forest has the same trees as before.
- **The window.** A pixel evaluates a fixed 3×4 window of slots (its own column ±1, its row and the 3 rows below), back to front. Every tree's crown, flames (≤ 1.6 tree heights, leaning ≤ 0.15), shadow (offset right, short below) and snag tilt are sized to stay inside the slots that can see them, so nothing is clipped at window boundaries. **If you make trees taller, flames higher or shadows longer, recheck these bounds** (comment in `draw_pine`).
- **Timeline per cell** (`fireAge`): 0–1 catching, 1–2 burning, 2–3 burnt, then 3 + ground age (`groundAge` in `common`). Per tree, `tau = fireAge - ignition`. Ignition is `arrival()` at the tree's position (0.85×) plus jitter, so the stand catches in order from the burning neighbors, or outward from the strike point. Everything that depends on ignition (char, flame ramp-up, embers) saturates by the end of the catching step's worst case, so the burnt step (whose A no longer has the mask) can use ignition 0 without a jump.
- **Fire:** flames flare up the crown (`climb`), then die back to about half height as the needles burn away (the crown is thresholded by noise rising with char, leaving trunk and branch stubs). Flames end per tree between fireAge 2.2 and 2.95. Shapes are a teardrop width profile distorted by scrolling fbm and vertical tongues, with a per-tree pulse, and composited semi-additively over the tree. Lightning flashes the tree nearest the strike.
- **Snags:** after the burnt step, each slot's snag stands until fireAge 3 + snag × (0.3–1.7), where snag is the Snag life slider (`view.snag`, default 10 steps after the clear; capped so it falls before the ground age saturates at 254), then tips (≤ 0.35 rad) and crumbles over 0.7 steps. They're drawn skeletal: the trunk thins by a third and snaps off at 0.82–0.94 of the height, and branches thin toward the tip, each cut to 0.3–0.8 of the crown's reach, with about a third missing (hashed per branch and side). A burning tree with char 1 and no flame is drawn by the same function as a standing snag, so the hand-over is exact. A sapling can grow in front of a snag.
- **Growth:** a new cause, `MATURE`, marks a tree's second step (`stepFrag` sets it after `GROW` and keeps the ground age in A through both). Saplings grow from nothing to full size over those two steps, brighter green while young. The forest-floor litter comes in with them.
- **Ground:** grass (regrowth), needle litter and ash amounts are computed per cell at the pixel (ash follows the front during catch), bilinearly blended between cell centers through a noise-warped lattice, then thresholded by noise, so changes come in patches. Ash washes into dirt over ~3 steps after the clear; embers glow on it through the burnt step.
- **Sky effects** from a bilinear per-cell heat field (`heatAt`): fire light (warm, flickering; much stronger at night), smoke sampled from the heat field below the pixel and drifting with the wind, and sparks: two lattices of particles rising on the wind, each lit if there was fire where it came from and fading as it rises. Particles are evaluated in the shader, not simulated, so they cost a few hashes and one `heatAt` per layer.
- **Wind** is a slow function of time: it sways the trees and leans flames, smoke and sparks.
- **Night** (toggle): moonlit ambient light, with fire light and emissive flames on top.
- **Known limitation:** a sapling that catches during its two growing steps jumps to full size, because A is the sources mask while burning and the size isn't kept. Rare at the usual p.
- **Adaptive resolution** (`adapt_resolution` in `engine.rs`): views render at `pixel_ratio` px per logical px (into an offscreen target that's scaled up), capped at 2 (phones report 3, i.e. 9× the pixels of 1×). Each second, if fps < 40 the ratio drops (by √(fps/57), floor 0.5) and the ceiling is set just below the old ratio; after 4 s at ≥ 57 fps it creeps back up 15% towards the ceiling. A drop that doesn't raise fps by 15% is undone, and dropping stops, since the cap was the browser's (iOS Low Power Mode runs rAF at 30). Changing renderer resets it. The grid is sized in CSS px, so changing the ratio never reseeds. The TS app reloaded the page on a lost WebGL context; the Rust port doesn't handle device loss yet. The stats line shows the ratio.
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
- **Looks:** the atlas is indexed by look, not by state: `grass, forest, fire, burnt, dirt` (K=5, 5⁴ = **625 stills**, `LOOKS` in `src/shaders.ts`, which must match `tools/atlas/stills.py`). TREE→forest, FIRE→fire, BURNT→burnt. EMPTY blends dirt→grass by regrowth = age / `uRegrow` (the Regrowth slider, default 20 steps).
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

### Lightning bolts (`src/lightning.rs`)

A lightning ignition is punctuated by a procedural bolt from the top of the screen to the spot in the cell where `arrival()` starts the fire (the CPU ports the shader hash to find it). A dim leader descends over 70 ms, then the return stroke flashes, with one or two restrikes and a faint sky flash; it's gone after 0.75 s of wall time, whatever the step rate.

- **Knowing where it struck.** Lightning is rolled per cell on the GPU, so the CPU can't see it. Instead, up to `BOLTS_PER_SECOND` (3) strikes per second are picked on the CPU as a short Poisson-sampled list of cells (`uStrikes`), and the shader's per-cell rate is reduced by the same amount, so the total process is unchanged. After the step, only the listed cells are read back (asynchronously since the Rust port; see *Rust port*); those that became FIRE with cause LIGHTNING get a bolt. A listed strike on bare ground, or on a tree that caught by spread anyway, shows nothing.
- **Drawing.** Midpoint-displaced polyline with forks, drawn as capsule quads into an offscreen buffer with MAX blending (so joints and overlapping glow don't double), then added onto the scene. Clicking to ignite also throws a bolt (throttled while dragging).

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
- Binary assets live outside the repo in `$TINDERBOX_DATA` (default `~/.local/share/tinderbox`). Nothing in the app reads them since the Rust port dropped the tiles renderer.

## Criticality: `tinderbox bench` and Lock to critical

The user asked (2026-10-09) for a way to measure how scale-free the fires are for given settings, and an option to lock lightning to the settings that maximise it.

- **Bench** (`src/bench.rs`, native only): a CPU replica of `step.wgsl` (fire spreads for 2 steps, BURNT 1 step, then EMPTY; growth only on EMPTY), run event by event. Growth and strikes are Poisson numbers of random cell picks, only burning cells are visited, and quiet spells skip straight to the next strike. Fires get ids, merged with union-find when they meet, and a fire's size is the trees it burnt. Score: log-bin sizes (5 bins per decade, ≥ 20 fires per bin) and find the widest run of bins that a line fits within 0.06 in log10. That run's width in decades is `decades` and minus its slope is `tau`. `spanning` is the share of burnt trees that burnt in fires over a tenth of the grid. Sweeps go to `$TINDERBOX_DATA/work/bench/*.tsv`.
- **What it showed (4-neighbor, wrap):** θ = p/f is the main control. Raising θ lengthens the power law until fires start sweeping the grid, so `decades` alone rewards the spanning regime (whole-grid fires still leave a straight middle). The objective is therefore **θ\* = the largest θ with spanning ≤ 5%**. θ\* is not monotone in p. At p ≥ 0.01, regrowth refeeds the fire from behind and fires never go out (one burnt 11× the grid). At small p, the finite grid limits it. The peak shifts with grid size (about p = 10⁻³ at 16 K cells and 3·10⁻⁴ at 1 M cells), so one smooth surface in (cells, p) fit badly (rms 0.43 in ln θ\*). Instead, each p row is its own power law in cells (worst residual 0.2), interpolated in ln p. At θ\* there are about 2 decades at 16 K cells and 3–3.8 at 1 M, with τ ≈ 1.1, consistent with the DS literature. **8-neighbor** θ\* is about half as large (worst residual 0.22), and fast growth costs more: at p = 3·10⁻³ only about 1.5 decades at any grid size. For both neighborhoods the longest power laws come from p ≈ 10⁻⁴–3·10⁻⁴ (with Speed raised to compensate).
- **Lock to critical** (`src/critical.rs`, the panel checkbox): growth is capped at `P_MAX` = 0.003 and lightning = p / θ\*(cells, p). It's re-applied whenever params change or the grid resizes, and the Lightning slider is disabled. Below p = 10⁻⁴, θ\* is held at the 10⁻⁴ row. Beyond 1 M cells it's extrapolated along the row's power law.
- To refit: run `tinderbox bench --grid 128x128,256x256,512x512,1024x1024 --theta 8,12,…,1600 --p 0.003,0.001,0.0003,0.0001 [--moore] > sweep.tsv`, then `tools/bench/fit.py sweep.tsv`, and paste the rows into `critical.rs`. One sweep takes about 20 minutes on 16 cores; runs in the spanning regime are the slow ones. **Launch long sweeps with `systemd-run --user`**: a Tandem restart kills everything the session started, `setsid nohup` included. `--resume FILE` skips runs already in FILE, so append to it (`>> FILE`) to finish an interrupted sweep.

## Milestones

- [x] v0 per-cell renderer, procedural placeholder, Wan VACE generator, turnstile lease client
- [x] Binary assets moved outside the repo, plus a Vite plugin to serve them
- [x] **M1: dual-grid renderer** (`dualFrag`, `loadTiles`/`placeholderTiles` in `src/atlas.ts`, plus a renderer select in the panel). It's checked in the browser: fire glow crosses tile borders seamlessly, canopies assemble from 4 quarters centered in their cells, and wrap and no-wrap edges both work. Within a step it currently **crossfades** from the previous configuration's still to the current one. M3 replaces that with clips.
- [x] **M2: generated stills** (first cut). `tools/atlas/tiles.py` writes a 96 px 9×9 atlas, and it renders seamlessly in the dual renderer. Known blemishes: faint green flecks on some ground tiles, left from cells where the model drew something atypical. The art style is inherited from the photoreal-ish v0 keyframes.
- [x] **M3: generated clips** (first cut, parked; the playback code was removed, and the generator still targets the old 3-state sim). `clips.py` makes 625 clips × 17 frames (14 worlds, about 2.5 min each); `loadClips` and `transition()`/`clipFrame()` in `dualFrag` play them over the step, and the stats line says "+ clips" when they've loaded. Checked in the browser: saplings grow, ignitions and burnouts play, and the tiles join. Known blemishes: burnout is smoky and mushy mid-clip, fire direction across edges isn't clearly legible, and the faint green fleck is still in the ground.
- [x] **M3b: shader transitions.** Two-step fire, a BURNT state, directional fronts, flicker, embers, smoke and fire light, plus groves. The v0 renderer is removed from the app. Checked in the browser at 80 px and 24 px cells.
- [~] **M4: forest stills (attempted, on hold).** The renderer side is done: five looks, ground age and regrowth, a Regrowth slider, and groves and the charring removed. Generation in `tools/atlas/stills.py` produced good key textures, but no harmonize strength gave usable worlds. See *M4 attempt*.
- [x] **M4b: procedural renderer** (first cut). `sceneFrag`: individual pines, per-tree ignition, flames, snags, sparks, smoke, regrowth, two-step growth, night. Checked in the browser (SwiftShader) at 64 px cells through catch, burn, burnt, clear, regrowth and saplings, by day and night. Needs tuning in motion on a real GPU.
- [x] **Rust port:** web (WebGPU/WebGL2) and native multi-monitor from one crate. Checked: headless 2-view layout seamless; web build in Playwright (WebGL2: panel, H, click-to-ignite); native `--views` with two windows under Xvfb + lavapipe (shared grid across both, bolts, panel). Not yet seen on real multiple monitors.
- [x] **Lock screen + releases:** `--saver` (`src/saver.rs`, Linux/X11) draws into the window in `XSCREENSAVER_WINDOW` (or `--window-id`) with no egui and no input, using the saved params without writing them. `tools/saver/tinderbox-saver` (xsecurelock runs savers without arguments) and `tinderbox-lock` (xss-lock + xsecurelock). Published at https://github.com/aiguy110/tinderbox; `.github/workflows/release.yml` builds Linux/macOS/Windows binaries on a `v*` tag. Checked under Xvfb with the real xsecurelock binary; the GNOME setup steps in the README haven't been tried on a real desktop.
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

00. **Chromecast background (2026-10-09):** deployed to `https://tinderbox.kleinflask.us/` (outpost, `/srv/tinderbox`, nginx + LE, Cloudflare DNS-only A record). Deploy by hand: `rsync -av --delete ~/Projects/tinderbox/dist/ outpost:/srv/tinderbox/`. Cast custom receiver (Cast console, $5 paid) has App ID `A7739EC5`, URL `/receiver.html` (an iframe of `index.html` with `disableIdleTimeout`; trunk copies it into `dist/`). Still to do: register the Chromecast serial as a test device in the console and reboot it; run `tools/cast-launch.py "<friendly name>"` (a PEP 723 script: `uv run tools/cast-launch.py "<name>"`; uv is at `~/.hermes/bin/uv` or the snap `astral-uv`, not always on PATH); **check the Chromecast's browser has WebGPU or WebGL2**, which is unverified. If the sim doesn't render there, the receiver needs a fallback.
0. **Lock screen on a real desktop:** get the user's report on the README's xsecurelock/GNOME steps, and fix the README with whatever was wrong. Ideas: span monitors in saver mode, so fires cross between them (the user wants this, 2026-10-09, but not yet). xsecurelock's `saver_multiplex` gives each monitor its own window and saver process, so each runs its own sim. `XSECURELOCK_NO_XRANDR=1` in `tinderbox-lock` should give one window over the whole X screen (unverified; the auth prompt may then straddle a seam). The saver doesn't save/resume the grid yet; with one window it could use one file, and per-monitor saves would need a file per monitor geometry. It's killed on unlock, so it would rely on the 5 s save (or catch SIGTERM). a Windows `.scr` (`/s`, `/p HWND`, `/c`) and a macOS `.saver`.
1. **Try the native build on real monitors** (bifrost has no display attached; this session tested under Xvfb). `cargo run --release` spans every monitor; check seams, frame pacing with several Fifo/Mailbox surfaces, and mixed DPI. Also check the web build on the user's phone (8 MB wasm; WebGPU or WebGL2 per the stats line).
2. **Get the user's verdict on the scene in motion** (64–96 px cells, 3–5 s per step), and measure fps on their GPU. The user saw flickering and freezing on their phone (2026-10-09), which led to adaptive resolution. Ask whether it's fixed and what fps and ratio the stats line shows. If it's still slow: fewer smoke/glow `heatAt` samples, or render the heat field to a small texture once per frame.
3. Tuning candidates noticed so far: flames are still fairly uniform cones from a distance; the ground under a burning stand reads as a squarish ash patch; dirt may be too orange. Possible touches: heat shimmer above flames, scorch marks where lightning strikes, crown fire jumping between trees, birds or deer fleeing, a day/night cycle instead of a toggle.
4. Possible: handle GPU device loss (the TS app reloaded on a lost WebGL context); shrink the wasm (it's mostly wgpu-core + naga for the WebGL path; fat LTO / `opt-level = "z"` for wasm only).
5. Get the user's verdict on the lightning bolts (look, duration, bolts per second).
6. Format reference, in case stills come back: `stills.json` `{"version": 1, "tile": N, "looks": [...]}` + `stills.png`, a K²×K² grid with index = SW + K·SE + K²·NW + K³·NE, rows top-down. `loadTiles` rejects an atlas whose `looks` don't match `LOOKS`.

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
- **Live app:** `tinderbox-build.service` (user unit, `tools/systemd/`) runs `trunk watch --release` in the main checkout (vite until the Rust port). A wasm release build takes about 40 s. Tailscale serves `dist/` at `https://bifrost.tail2280a0.ts.net:8451/`. It replaced the old :5191 dev-server unit on 2026-10-08. See CLAUDE.md.
- **GPU queue:** the generators queue at priority 0, so a named consumer (xad-pod, 30) can hold the GPU for 15–20 minutes at a time. That's expected. Check with `curl -s localhost:8080/api/queue`.
- **Background jobs:** launch with `setsid nohup … < /dev/null & disown`, or the job dies with the agent session. Don't detect them with `pgrep -f clips.py`, which matches the shell running pgrep itself. Use `ps -eo args | grep "[.]venv/bin/python clips.py"`.
- **Rust toolchain:** `rust-toolchain.toml` pins 1.95 with the wasm32 target; the default toolchain (1.81) is too old for wgpu 30. `trunk` 0.21 is in `~/.cargo/bin` and downloads `wasm-opt` itself.
- **Native testing without a display:** `tinderbox --shot DIR --layout 1280x720+0+0,960x720+1280+0` renders on the 3080 (Vulkan) headless. For real windows: `Xvfb :77 -screen 0 2560x720x24 &`, then `DISPLAY=:77 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json tinderbox --views 1280x720+0+0,1280x720+1280+0` (lavapipe, ~6 fps) and grab the screen with `xwd -root`. Xvfb can't fake several RandR monitors (`xrandr --setmonitor` fails), hence `--views`. Set `XDG_CONFIG_HOME` to a temp dir so tests don't overwrite saved params.
- **Inspecting the web build in Playwright:** serve `dist/` (`python3 -m http.server --directory dist`), set params through localStorage `tinderbox:params` (e.g. `{"cellSize": 64, "stepsPerSecond": 0.5}`) and reload. On 2026-10-09 Playwright's Chromium got WebGL2 on the NVIDIA GPU (~59 fps at 700×450; the TS-era notes said SwiftShader at ~1 fps) and no WebGPU. There's no `window.tinderbox` console hook in the Rust build; for frame-exact stills use native `--shot` (`--steps`, `--progress`).
- **Testing the saver with xsecurelock without root:** `apt-get download xsecurelock`, `dpkg -x` it, and byte-patch `/usr/libexec/xsecurelock` (24 chars) in `xsecurelock` and every helper to another 24-char path such as `/tmp/xsl/hhhhhhhhhhhhhhh` holding the patched helpers (bwrap can't make user namespaces here). Then run it on an Xvfb display with `XSECURELOCK_SAVER=tools/saver/tinderbox-saver`, lavapipe, and `timeout`, and grab the screen with `xwd -root`.
- **Python venv:** `tools/atlas/.venv` (gitignored). Recreate it with `pip install -r tools/atlas/requirements.txt`.

## Log

- **2026-10-09 (late evening):** `tinderbox bench` and Lock to critical (see *Criticality*).
- **2026-10-09 (afternoon):** `tinderbox update` (`src/update.rs`) installs the latest GitHub release over the running binary via system `curl` and `tar` (no HTTP/TLS crates), renaming over it (on Windows the running exe is renamed to `.old` first and removed on the next update); `--force` reinstalls. Also `--version`. Tested on Linux with `--force` against v0.4.0, including through a symlink; macOS and Windows untested. Released v0.5.0.
- **2026-10-09 (night):** Release builds never hit the rust-cache: caches saved on a tag ref are invisible to other tags. The workflow now also builds on pushes to main (Rust paths only, superseded runs cancelled) to save the cache there; tag runs restore it without saving.
- **2026-10-09 (evening, later):** Keep the screen awake while running (web wake lock; Linux GNOME/freedesktop inhibitor). Released v0.7.0.
- **2026-10-09 (evening):** Snag life slider (1–120 steps, default 10); Regrowth goes to 240 steps (log slider; ground age saturates at 254). Grass slightly and pine green notably lighter, so black snags read against live trees. Released v0.6.0.
- **2026-10-09 (late morning):** Released v0.4.0 (save/resume, skeletal snags).
- **2026-10-09 (morning):** Skeletal snags standing twice as long; the grid is saved every 5 s and resumed on reload or re-run. Verified on the web in Playwright (generation carried over a reload); native save/load is unit-tested only (no display on bifrost).
- **2026-10-08:** Built v0 end to end and generated a real atlas. The spread midpoint wipe reliably fixes fire direction. Keyframe lessons: the model ignites late in a burn clip, so pick the most-fire frame. The empty keyframe needs its own text-to-image pass. Loops with identical first and last frames come out nearly static. The user rejected v0 for its seams and rotation, which led to the v1 dual-grid design above.
- **2026-10-08 (later):** M2. Tried hierarchical frozen inpainting, VACE layout control, blurry-layout SDEdit/ILVR and per-step latent sync; all failed (see *What didn't work*). What works is a sharp keyframe collage, an SDEdit harmonize pass and a pixel-space stitch with medoid leaders. Verified seamless in the browser.
- **2026-10-08 (evening):** M3 started. A plain SDEdit video pass repaints the ground over the clip (smooth at frame 0, rocky by frame 16); VACE keep-frames at both ends, set to the still worlds, fix it.
- **2026-10-08 (night):** M3 first cut: 625 clips, played back in the dual renderer. A clean-soil ground keyframe made the stills drift (see *What didn't work*), so the old ground stays. Stills now stitch with per-pixel-median leaders. The dev server became a systemd unit. bifrost rebooted mid-run once; jobs launched without `setsid` die with their session.
- **2026-10-08 (late night):** The user asked for forest stands, directional fire entry, a burning step and a burnt step. Per-config clips with direction would be about 10,000 clips, so transitions moved to the shader (M3b): world-space blend weights over the stills, multilinear across a tile's corners. Groves (n×n sub-cells per cell) give a stand of trees without new art; real forest stills are M4.
- **2026-10-08/09 (night):** The user clarified: real pine-forest art, not groves, and charred dirt greening back over about 10 steps. Built five looks, ground age in A, regrowth in the shader, and `stills.py`. Keys came out well after pixel-space prep for fire and burnt; harmonize at 0.4/0.5/0.6 didn't give usable worlds (see *M4 attempt*). The live app falls back to placeholder tiles. The user's next direction: a fully procedural, shader-heavy renderer. An unsetsid'd probe died with the session; keep using `setsid nohup`.
- **2026-10-09:** adaptive resolution for phones (see *v2 design*). Under SwiftShader at 560×360 it went from 1 fps at 1× to 9–10 fps at 0.5×.
- **2026-10-09:** v2 first cut: the procedural scene renderer, now the default (the saved renderer `dual` migrates to `scene`; tiles are `tiles`). Added `MATURE` and a continuous `groundAge`. Lessons: low-temperature flame fragments from high-frequency noise read as black specks, so flame alpha needs a threshold; flames must fade before the slot window's edge or they get flat tops; reversed `smoothstep` edges are undefined in GLSL, so use `1 - smoothstep(lo, hi, x)`.
- **2026-10-09:** ported the app to Rust (see *Rust port*). Lessons: egui-winit doesn't compile for wasm; WebGL2 lacks `SURFACE_VIEW_FORMATS`, so pick a non-sRGB surface format directly; on the web, resize events can arrive before async GPU init finishes, so sync sizes once it's ready; egui 0.36's `TexturesDelta` must be drained, not iterated.
- **2026-10-09:** mobile slowed down after the Rust port, and slow frames make the adaptive resolution drop, which looks blocky. Two costs the port added: the web surface ran at the full device ratio (3× on phones; the TS canvas was capped at 2×), so the blit, panel and present passes ran at 2.25× the pixels; and naga's bounds checks clamped every texel fetch in the WebGL2 shaders. Now windows that follow the browser or a window (not whole monitors) get a surface of at most `MAX_PIXEL_RATIO`, pointer positions are mapped through the window size, egui gets the surface's pixels per point, and on wasm shaders are compiled with `create_shader_module_trusted` without runtime checks. Testing tip: Playwright's `deviceScaleFactor` emulation reports `devicePixelContentBoxSize` in CSS pixels, so winit sees a 1× canvas. To test real DPR, launch Chromium with `--force-device-scale-factor=3` and a `viewport: null` context. Real phone fps can't be measured here (headless is SwiftShader).
- **2026-10-09:** lock screen and releases. The saver creates the wgpu surface from raw Xlib handles (`x11-dl`, already in the tree through winit) and polls the window size twice a second rather than selecting events on a window it doesn't own. It exits when the locker kills it (or through Xlib's default error handler if the window goes away). Repo published to GitHub as aiguy110/tinderbox.
