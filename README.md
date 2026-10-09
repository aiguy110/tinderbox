# tinderbox

A full-screen GPU [Drossel–Schwabl forest-fire model](https://en.wikipedia.org/wiki/Forest-fire_model), written in Rust. It runs in the browser (WebGPU, or WebGL2) and natively, where one scene can span several monitors.

Each step, every cell updates at the same time:

- **Tree → burning** if any neighbor is burning, or with probability **f** (lightning)
- **Burning** lasts two steps (catching, then burning), then the cell is **burnt** for one step and **empty** after that. Burnt ground starts as bare dirt and greens back to grass over **Regrowth** steps (drawing only; it doesn't affect the rule)
- **Empty → tree** with probability **p** (growth)

The interesting regime is f ≪ p ≪ 1. Fires then follow a power-law size distribution (self-organized criticality).

## Download

Binaries for Linux (x86_64), macOS (Apple Silicon) and Windows (x86_64) are on the [releases page](https://github.com/aiguy110/tinderbox/releases). They need a GPU with Vulkan, Metal or DirectX 12 support.

- **Linux:** `tar xzf tinderbox-linux-x86_64.tar.gz`, then run `tinderbox-linux-x86_64/tinderbox`. To have it on your PATH, copy `tinderbox`, `tinderbox-saver` and `tinderbox-lock` into `~/.local/bin`.
- **macOS:** extract the archive, then run `xattr -d com.apple.quarantine tinderbox` once (the binary isn't signed) and `./tinderbox`.
- **Windows:** extract the zip and run `tinderbox.exe`. SmartScreen may warn about an unrecognized app; choose *More info → Run anyway*.

By default it spans every monitor fullscreen; `Esc` quits. `tinderbox --windowed` runs in a window, and `tinderbox --help` lists the other options. Controls are under [Run](#run).

## Lock screen (Linux, X11)

`tinderbox --saver` draws into a window owned by a screen locker, so the forest keeps burning behind the password prompt. The locker handles the keyboard, mouse and unlocking. tinderbox only draws.

The saver uses the settings you last saved in the regular app (`~/.config/tinderbox/params.json`), so set it up the way you like there first. Big, slow cells look best, for example 64 px and 0.3 steps per second. Each monitor runs its own forest.

xsecurelock and xscreensaver only work in an X11 session, not Wayland. On Ubuntu, pick *Ubuntu on Xorg* from the gear on the login screen.

### xsecurelock (recommended)

[xsecurelock](https://github.com/google/xsecurelock) runs a saver program behind its password prompt. [xss-lock](https://bitbucket.org/raymonad/xss-lock) runs xsecurelock when the screen should lock.

1. Install both, and put the three tinderbox files on your PATH:

   ```sh
   sudo apt install xsecurelock xss-lock          # Debian/Ubuntu; Arch: xsecurelock, xss-lock
   mkdir -p ~/.local/bin
   cp tinderbox tinderbox-saver tinderbox-lock ~/.local/bin/
   ```

2. Try it. This locks the screen right away, with tinderbox behind it. Type your password to unlock:

   ```sh
   XSECURELOCK_SAVER=~/.local/bin/tinderbox-saver XSECURELOCK_BLANK_TIMEOUT=-1 xsecurelock
   ```

3. Start `tinderbox-lock` when you log in. It locks after 10 idle minutes (set `TINDERBOX_IDLE_SECONDS` to change that), on `loginctl lock-session`, and before suspend:

   ```sh
   mkdir -p ~/.config/autostart
   cat > ~/.config/autostart/tinderbox-lock.desktop <<DESKTOP
   [Desktop Entry]
   Type=Application
   Name=tinderbox lock screen
   Exec=$HOME/.local/bin/tinderbox-lock
   DESKTOP
   ```

   With i3 or another bare window manager, add `exec ~/.local/bin/tinderbox-lock` to its config instead.

4. **GNOME only:** turn off GNOME's own lock screen, so it doesn't lock over xsecurelock, and point the lock shortcut at the session lock:

   ```sh
   gsettings set org.gnome.desktop.lockdown disable-lock-screen true
   gsettings set org.gnome.desktop.session idle-delay 0   # Settings › Power › Screen Blank: Never
   ```

   Then in *Settings › Keyboard › View and Customize Shortcuts*:
   - under *System*, disable *Lock Screen*;
   - under *Custom Shortcuts*, add `loginctl lock-session` on `Super+L`.

   To undo all of this, run `gsettings reset` on the same two keys, delete the autostart file, and restore the shortcut.

5. Log out and back in.

By default xsecurelock blanks the screen after 10 minutes, and `tinderbox-lock` turns that off so the fire keeps going. To blank eventually and save power, set `XSECURELOCK_BLANK_TIMEOUT` to a number of seconds before starting `tinderbox-lock`. Other xsecurelock settings (`XSECURELOCK_*`, see `man xsecurelock`) pass through.

### xscreensaver

Add tinderbox to the `programs:` list in `~/.xscreensaver`, then pick it in `xscreensaver-settings`:

```
programs:                                                   \
  "tinderbox"   tinderbox --saver --root                  \n\
```

xscreensaver's `--root` is accepted and ignored. tinderbox draws into whatever window `XSCREENSAVER_WINDOW` names.

## Run

To build from source, you need rustup (the toolchain is pinned in `rust-toolchain.toml`) and, for the web, [trunk](https://trunkrs.dev) (`cargo install trunk --locked`).

```sh
cargo run --release                     # span every monitor, fullscreen
cargo run --release -- --windowed       # one window
cargo run --release -- --monitors 0,2   # only some monitors (see --list-monitors)
cargo run --release -- --help           # also --views and headless --shot
trunk serve                             # web build at http://localhost:8080
trunk build --release                   # web build into dist/
```

On several monitors the grid covers their bounding box, and each monitor shows its own part of it, so fires burn across the seams. Cells are sized in the primary monitor's logical pixels.

**Controls:** use the panel to set cell size, trees per cell, p, f, speed, regrowth time, seed density, neighborhood, edge wrap, and day or night. The scene renderer wants big, slow cells: try 48–96 px and 3–5 s per step. Keys:

- `H` or the × / ☰ buttons hide and show the panel
- `F` toggles fullscreen (web and `--windowed`)
- `Space` pauses
- `.` steps once
- `Esc` quits (native)

Click or drag to start fires. Settings, including whether the panel is hidden, are saved (localStorage on the web, `~/.config/tinderbox/params.json` natively). The forest itself is saved every few seconds and picks up where it left off (localStorage on the web, `~/.local/state/tinderbox/grid.bin` natively).

## Layout

- `src/shaders/*.wgsl`: the update rule (`step`), the procedural scene (`scene`, the default), flat colors (`flat`), lightning bolts and the upscaling blit. `src/shaders.rs` assembles them with the state constants
- `src/sim.rs`: ping-pong simulation, resizing, ignition, and reading back where lightning struck
- `src/render.rs`: drawing one view: scene at the adaptive resolution, bolts, then up to the screen
- `src/engine.rs`: the layout, step timing, adaptive resolution and input-to-cell mapping, independent of windows
- `src/app.rs`: windows (canvas, desktop window, or one per monitor), surfaces, input and the frame loop
- `src/ui.rs`, `src/input.rs`, `src/params.rs`: the egui panel, its input, and saved settings
- `src/lightning.rs`: bolt geometry and drawing
- `src/shot.rs`: headless rendering of a virtual monitor layout to PNG (`--shot`)

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

An earlier renderer drew tiles centered on grid vertices, from a diffusion-generated atlas. Generating the atlas didn't work out, and the renderer was dropped in the Rust port. It's described in [docs/PLAN.md](docs/PLAN.md) and lives in git history.

## Roadmap

- **Phase 1 (done):** GPU sim, flat-color renderer, adjustable parameters, fullscreen.
- **Phase 2 (in progress):** a fully procedural scene (see [Procedural scene](#procedural-scene)). Before it: dual-grid tiles with shader-drawn transitions; diffusion-generated tiles are on hold.
- **Rust port (done):** web and native from one crate, with multi-monitor spanning.
- Also planned: fire-size histogram and density plots (needs GPU reduction or async readback), and painting trees and firebreaks.
