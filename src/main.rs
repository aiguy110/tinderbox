//! tinderbox: a Drossel–Schwabl forest-fire sim, drawn procedurally on the
//! GPU. Runs in the browser (WebGPU, or WebGL2) and natively, where it can
//! span one scene across several monitors. See README.md.

mod app;
mod engine;
mod input;
mod lightning;
mod params;
mod render;
mod rng;
mod save;
#[cfg(target_os = "linux")]
mod saver;
mod shaders;
#[cfg(not(target_arch = "wasm32"))]
mod shot;
mod sim;
mod ui;
#[cfg(not(target_arch = "wasm32"))]
mod update;

use winit::event_loop::EventLoop;

#[cfg(target_arch = "wasm32")]
fn main() {
    use winit::platform::web::EventLoopExtWebSys;
    console_error_panic_hook::set_once();
    let el = EventLoop::<app::UserEvent>::with_user_event().build().expect("event loop");
    let proxy = el.create_proxy();
    el.spawn_app(app::App::new(app::Mode::Web, proxy));
}

#[cfg(not(target_arch = "wasm32"))]
const USAGE: &str = "\
usage: tinderbox [options]
       tinderbox update [--force]

  (no options)        span the scene across every monitor, fullscreen
  --monitors 0,2      span only these monitors
  --list-monitors     print the monitors and their indices
  --windowed          run in a single window instead
  --views WxH+X+Y,... ordinary windows standing in for monitors, to try a
                      layout on one screen
  --shot DIR          render headless to DIR/view-N.png and DIR/layout.png
      --layout WxH+X+Y,...   virtual monitors (default 1280x720+0+0)
      --cell N               cell size in pixels (default 64)
      --steps N              steps to run after igniting each view's center (default 3)
      --progress X           how far through the last step, 0 to 1 (default 0.5)
      --flat, --night        renderer and lighting
  --saver             screensaver mode (Linux, X11): draw into the window in
                      XSCREENSAVER_WINDOW, for xsecurelock or xscreensaver
      --window-id ID         draw into this X window instead

  update              replace this binary with the latest GitHub release
                      (--force reinstalls it even if it's not newer)
  --version           print the version

Keys: H hides the panel, F toggles fullscreen (windowed), Space pauses,
. steps, Esc quits. Click or drag to start fires.";

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("tinderbox {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.first().is_some_and(|a| a == "update") {
        if let Err(e) = update::run(&args[1..]) {
            eprintln!("tinderbox update: {e}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--saver") {
        if let Err(e) = saver::run(&args) {
            eprintln!("tinderbox: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--shot") {
        if let Err(e) = shot::run(&args) {
            eprintln!("tinderbox: {e}");
            std::process::exit(1);
        }
        return;
    }
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1));
    let mode = if args.iter().any(|a| a == "--list-monitors") {
        app::Mode::ListMonitors
    } else if args.iter().any(|a| a == "--windowed") {
        app::Mode::Windowed
    } else if let Some(list) = value("--views") {
        match list.split(',').map(|s| render::Rect::parse(s.trim())).collect::<Option<Vec<_>>>() {
            Some(rects) => app::Mode::Views(rects),
            None => {
                eprintln!("--views takes rectangles like 1280x720+0+0,1280x720+1280+0\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    } else if let Some(list) = value("--monitors") {
        match list.split(',').map(|s| s.trim().parse::<usize>()).collect::<Result<Vec<_>, _>>() {
            Ok(ix) => app::Mode::Monitors(Some(ix)),
            Err(_) => {
                eprintln!("--monitors takes indices like 0,2\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    } else {
        app::Mode::Monitors(None)
    };
    let el = match EventLoop::<app::UserEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("tinderbox: no display ({e}). Try --shot for headless rendering.");
            std::process::exit(1);
        }
    };
    let proxy = el.create_proxy();
    let mut app = app::App::new(mode, proxy);
    if let Err(e) = el.run_app(&mut app) {
        eprintln!("tinderbox: {e}");
        std::process::exit(1);
    }
}
