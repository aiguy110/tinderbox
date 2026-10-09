//! The windowing shell, shared by the web and native builds: one window per
//! view (the browser canvas, a desktop window, or one borderless fullscreen
//! window per monitor), all drawn from one engine on one GPU device.
//!
//! Every frame is driven by the primary view's redraw: it ticks the engine
//! once, then draws every view and presents them together, so the sim
//! advances once per frame however many monitors there are.

use std::sync::Arc;

use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Fullscreen, Window, WindowId};

use crate::engine::{Engine, Layout, MAX_PIXEL_RATIO};
use crate::input::{self, EguiInput};
use crate::params::Params;
use crate::render::{Rect, ViewTargets};
use crate::ui;

/// Dragging throws fewer bolts.
const DRAG_BOLT_SECS: f32 = 0.15;

#[cfg(target_arch = "wasm32")]
const HINT: &str = "H hides · F fullscreen · Space pause · click to ignite";
#[cfg(not(target_arch = "wasm32"))]
const HINT: &str = "H hides · F fullscreen · Space pause · Esc quits · click to ignite";

#[derive(Clone, Debug)]
// Each build constructs only some of these.
#[allow(dead_code)]
pub enum Mode {
    /// The page's canvas.
    Web,
    /// One desktop window.
    Windowed,
    /// One borderless fullscreen window per monitor, spanning the scene
    /// across them. `None` means every monitor; otherwise indices into the
    /// monitor list (see `--list-monitors`).
    Monitors(Option<Vec<usize>>),
    /// Ordinary windows standing in for monitors, at these rectangles of a
    /// layout, for trying a layout out on one screen.
    Views(Vec<Rect>),
    /// Print the monitors and exit.
    ListMonitors,
}

pub enum UserEvent {
    Ready(Result<Box<Running>, String>),
}

pub struct App {
    mode: Mode,
    proxy: EventLoopProxy<UserEvent>,
    started: bool,
    running: Option<Box<Running>>,
}

impl App {
    pub fn new(mode: Mode, proxy: EventLoopProxy<UserEvent>) -> Self {
        App { mode, proxy, started: false, running: None }
    }
}

struct ViewWindow {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The format we render the surface through: never sRGB, so colors are
    /// written as-is like the WebGL canvas did.
    format: wgpu::TextureFormat,
    rect: Rect,
    targets: ViewTargets,
    cursor: Option<PhysicalPosition<f64>>,
}

pub struct Running {
    engine: Engine,
    views: Vec<ViewWindow>,
    egui_ctx: egui::Context,
    egui_input: EguiInput,
    cursor_icon: egui::CursorIcon,
    egui_renderer: egui_wgpu::Renderer,
    /// Web and windowed: the layout follows the window. Monitors: it's fixed
    /// by where the monitors are.
    follows_window: bool,
    igniting: bool,
    bolt_at: Option<Instant>,
    backend: String,
}

/// The windows, where each sits in the layout, and the layout's scale.
type Windows = (Vec<Arc<Window>>, Vec<Rect>, f64);

fn create_windows(el: &ActiveEventLoop, mode: &Mode) -> Result<Windows, String> {
    let attrs = Window::default_attributes().with_title("tinderbox");
    let window = |a| el.create_window(a).map(Arc::new).map_err(|e| format!("couldn't create a window: {e}"));
    let fill = |w: &Window| {
        let s = w.inner_size();
        Rect { x: 0, y: 0, w: s.width.max(1), h: s.height.max(1) }
    };
    match mode {
        Mode::Web => {
            #[cfg(target_arch = "wasm32")]
            {
                use wasm_bindgen::JsCast;
                use winit::platform::web::WindowAttributesExtWebSys;
                let canvas = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.get_element_by_id("stage"))
                    .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
                    .ok_or("no #stage canvas")?;
                let _ = canvas.focus();
                let w = window(attrs.with_canvas(Some(canvas)).with_prevent_default(true))?;
                let r = fill(&w);
                let scale = w.scale_factor();
                Ok((vec![w], vec![r], scale))
            }
            #[cfg(not(target_arch = "wasm32"))]
            unreachable!()
        }
        Mode::Windowed => {
            let w = window(attrs.with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0)))?;
            let r = fill(&w);
            let scale = w.scale_factor();
            Ok((vec![w], vec![r], scale))
        }
        Mode::Monitors(which) => {
            let all: Vec<_> = el.available_monitors().collect();
            let chosen: Vec<_> = match which {
                None => all.clone(),
                Some(ix) => ix
                    .iter()
                    .map(|&i| all.get(i).cloned().ok_or(format!("no monitor {i}; see --list-monitors")))
                    .collect::<Result<_, _>>()?,
            };
            if chosen.is_empty() {
                return Err("no monitors found".into());
            }
            let mut windows = Vec::new();
            let mut rects = Vec::new();
            for m in &chosen {
                let (pos, size) = (m.position(), m.size());
                let w = window(
                    attrs
                        .clone()
                        .with_decorations(false)
                        .with_position(pos)
                        .with_inner_size(size)
                        .with_fullscreen(Some(Fullscreen::Borderless(Some(m.clone())))),
                )?;
                windows.push(w);
                rects.push(Rect { x: pos.x, y: pos.y, w: size.width, h: size.height });
            }
            // Cells are sized in the primary monitor's logical pixels; with
            // mixed DPI they're the same physical size on every monitor, so
            // the scene stays continuous across the seams.
            let scale = chosen[0].scale_factor();
            Ok((windows, rects, scale))
        }
        Mode::Views(rects) => {
            let mut windows = Vec::new();
            for (i, r) in rects.iter().enumerate() {
                let a = attrs
                    .clone()
                    .with_title(format!("tinderbox {i}"))
                    .with_position(winit::dpi::PhysicalPosition::new(r.x, r.y))
                    .with_inner_size(PhysicalSize::new(r.w, r.h))
                    .with_resizable(false);
                windows.push(window(a)?);
            }
            Ok((windows, rects.clone(), 1.0))
        }
        Mode::ListMonitors => unreachable!(),
    }
}

impl Running {
    async fn new(
        windows: Vec<Arc<Window>>,
        rects: Vec<Rect>,
        scale: f64,
        follows_window: bool,
        instance: wgpu::Instance,
    ) -> Result<Self, String> {
        let surfaces = windows
            .iter()
            .map(|w| instance.create_surface(w.clone()).map_err(|e| format!("couldn't create a surface: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surfaces[0]),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| format!("no suitable GPU: {e}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("tinderbox"),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("couldn't open the GPU: {e}"))?;
        let info = adapter.get_info();
        let backend = match info.backend {
            wgpu::Backend::BrowserWebGpu => "WebGPU".to_string(),
            wgpu::Backend::Gl if cfg!(target_arch = "wasm32") => "WebGL2".to_string(),
            b => format!("{b:?}"),
        };

        let layout = Layout { views: rects.clone(), scale }.normalized();
        let engine = Engine::new(device, queue, layout.clone(), Params::load());

        let mut views = Vec::new();
        for (i, (window, surface)) in windows.into_iter().zip(surfaces).enumerate() {
            let caps = surface.get_capabilities(&adapter);
            let surface_format =
                caps.formats.iter().find(|f| !f.is_srgb()).or(caps.formats.first()).copied().ok_or("the surface has no formats")?;
            let views_ok = adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::SURFACE_VIEW_FORMATS);
            let format = if views_ok { surface_format.remove_srgb_suffix() } else { surface_format };
            // The primary view paces frames; the rest shouldn't each wait
            // for another vblank.
            let present_mode = if i > 0 && caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::Fifo
            };
            let size = window.inner_size();
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode: caps.alpha_modes[0],
                color_space: Default::default(),
                view_formats: if format != surface_format { vec![format] } else { vec![] },
            };
            surface.configure(&engine.device, &config);
            window.set_cursor(CursorIcon::Crosshair);
            let rect = layout.views[i];
            let (tw, th) = engine.target_size(&rect);
            let targets = ViewTargets::new(&engine.device, &engine.pipelines, tw, th);
            views.push(ViewWindow { window, surface, config, format, rect, targets, cursor: None });
        }

        let egui_ctx = egui::Context::default();
        ui::style(&egui_ctx);
        let egui_renderer = egui_wgpu::Renderer::new(&engine.device, views[0].format, Default::default());
        Ok(Running {
            engine,
            views,
            egui_ctx,
            egui_input: EguiInput::new(),
            cursor_icon: egui::CursorIcon::Default,
            egui_renderer,
            follows_window,
            igniting: false,
            bolt_at: None,
            backend,
        })
    }

    fn resized(&mut self, i: usize, size: PhysicalSize<u32>) {
        let v = &mut self.views[i];
        if size.width == 0 || size.height == 0 {
            return;
        }
        // A window that isn't a whole monitor (the browser canvas, mostly)
        // gets a surface of at most MAX_PIXEL_RATIO per logical pixel, like
        // the scene: at a phone's 3×, the blit, panel and present passes cost
        // 2.25× as much for nothing visible.
        let (size, scale) = if self.follows_window {
            let s = v.window.scale_factor();
            let k = s.min(MAX_PIXEL_RATIO);
            let fit = |n: u32| ((n as f64 * k / s).round() as u32).max(1);
            (PhysicalSize::new(fit(size.width), fit(size.height)), k)
        } else {
            (size, v.window.scale_factor())
        };
        v.config.width = size.width;
        v.config.height = size.height;
        v.surface.configure(&self.engine.device, &v.config);
        if self.follows_window {
            let views = vec![Rect { x: 0, y: 0, w: size.width, h: size.height }];
            v.rect = views[0];
            self.engine.set_layout(Layout { views, scale });
        }
    }

    /// Strike lightning under a pointer at `pos` in view `i`'s window.
    fn ignite(&mut self, i: usize, pos: PhysicalPosition<f64>, dragging: bool) {
        let v = &self.views[i];
        // Pointer positions are in the window's pixels, which may be more
        // than the surface's.
        let win = v.window.inner_size();
        let gx = v.rect.x as f64 + pos.x * v.rect.w as f64 / win.width.max(1) as f64;
        let gy = v.rect.y as f64 + pos.y * v.rect.h as f64 / win.height.max(1) as f64;
        let bolt = !dragging || self.bolt_at.is_none_or(|t| t.elapsed().as_secs_f32() > DRAG_BOLT_SECS);
        if self.engine.ignite(gx, gy, bolt) && bolt {
            self.bolt_at = Some(Instant::now());
        }
    }

    fn over_panel(&self, i: usize) -> bool {
        i == 0 && self.egui_ctx.is_pointer_over_egui()
    }

    fn toggle_fullscreen(&self) {
        if self.follows_window {
            let w = &self.views[0].window;
            w.set_fullscreen(if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) });
        }
    }

    fn key(&mut self, el: &ActiveEventLoop, key: &Key) {
        let p = &mut self.engine.params;
        match key.as_ref() {
            Key::Character("h" | "H") => p.panel_hidden = !p.panel_hidden,
            Key::Character("f" | "F") => self.toggle_fullscreen(),
            Key::Character(" ") | Key::Named(NamedKey::Space) => p.paused = !p.paused,
            Key::Character(".") => self.engine.step_once(),
            Key::Named(NamedKey::Escape) if cfg!(not(target_arch = "wasm32")) => {
                let w = &self.views[0].window;
                if self.follows_window && w.fullscreen().is_some() {
                    w.set_fullscreen(None);
                } else {
                    el.exit();
                }
            }
            _ => {}
        }
        self.engine.params_changed();
    }

    fn frame(&mut self) {
        let Running { engine: e, views, egui_ctx, egui_input, .. } = self;
        e.tick();

        let primary = views[0].window.clone();
        let stats = e.stats(views.len(), &self.backend);
        let mut actions = ui::Actions::default();
        let surface_ppp = views[0].config.width as f32 * primary.scale_factor() as f32 / primary.inner_size().width.max(1) as f32;
        let raw = egui_input.take(&primary, surface_ppp);
        let out = egui_ctx.run_ui(raw, |ui| actions = ui::panel(ui.ctx(), &mut e.params, &stats, HINT));
        if out.platform_output.cursor_icon != self.cursor_icon {
            self.cursor_icon = out.platform_output.cursor_icon;
            input::cursor(&primary, self.cursor_icon);
        }
        if actions.step {
            e.step_once();
        }
        if actions.reseed {
            e.reseed();
        }
        e.params_changed();
        e.autosave();
        if actions.fullscreen {
            self.toggle_fullscreen();
        }
        let Running { engine: e, views, egui_ctx, egui_renderer, .. } = self;

        let prims = egui_ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut textures = out.textures_delta;
        for (id, deltas) in textures.set.drain() {
            for delta in deltas {
                egui_renderer.update_texture(&e.device, &e.queue, id, &delta);
            }
        }

        let mut enc = e.device.create_command_encoder(&Default::default());
        let mut cmds = Vec::new();
        let mut frames = Vec::new();
        for (i, v) in views.iter_mut().enumerate() {
            let want = e.target_size(&v.rect);
            if v.targets.size != want {
                v.targets = ViewTargets::new(&e.device, &e.pipelines, want.0, want.1);
            }
            let frame = match v.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    v.surface.configure(&e.device, &v.config);
                    continue;
                }
                _ => continue,
            };
            let target = frame.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(v.format), ..Default::default() });
            let rect = v.rect;
            e.draw_view(&mut enc, &rect, &v.targets, &target, v.format);
            if i == 0 {
                let screen = egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [v.config.width, v.config.height],
                    pixels_per_point: out.pixels_per_point,
                };
                cmds.extend(egui_renderer.update_buffers(&e.device, &e.queue, &mut enc, &prims, &screen));
                let mut pass = enc
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        ..Default::default()
                    })
                    .forget_lifetime();
                egui_renderer.render(&mut pass, &prims, &screen);
            }
            frames.push((v.window.clone(), frame));
        }
        cmds.push(enc.finish());
        e.queue.submit(cmds);
        for (w, f) in frames {
            w.pre_present_notify();
            e.queue.present(f);
        }
        for id in textures.free.drain() {
            egui_renderer.free_texture(&id);
        }
        // Natively, map callbacks (strike readbacks) only run when polled.
        #[cfg(not(target_arch = "wasm32"))]
        let _ = e.device.poll(wgpu::PollType::Poll);
        primary.request_redraw();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        if let Mode::ListMonitors = self.mode {
            for (i, m) in el.available_monitors().enumerate() {
                let (p, s) = (m.position(), m.size());
                println!("{i}: {} {}x{} at {},{} scale {}", m.name().unwrap_or_default(), s.width, s.height, p.x, p.y, m.scale_factor());
            }
            el.exit();
            return;
        }
        let (windows, rects, scale) = match create_windows(el, &self.mode) {
            Ok(w) => w,
            Err(e) => return fail(el, e),
        };
        let follows_window = matches!(self.mode, Mode::Web | Mode::Windowed);

        #[cfg(target_arch = "wasm32")]
        {
            let proxy = self.proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
                desc.backends = wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL;
                let instance = wgpu::util::new_instance_with_webgpu_detection(desc).await;
                let r = Running::new(windows, rects, scale, follows_window, instance).await;
                let _ = proxy.send_event(UserEvent::Ready(r.map(Box::new)));
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle()));
            desc.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
            let instance = wgpu::Instance::new(desc);
            let r = pollster::block_on(Running::new(windows, rects, scale, follows_window, instance));
            let _ = self.proxy.send_event(UserEvent::Ready(r.map(Box::new)));
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Ready(Ok(mut r)) => {
                // Resizes that arrived while the GPU was starting up.
                for i in 0..r.views.len() {
                    let size = r.views[i].window.inner_size();
                    r.resized(i, size);
                }
                r.engine.resume();
                r.views[0].window.request_redraw();
                self.running = Some(r);
            }
            UserEvent::Ready(Err(e)) => fail(el, e),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(r) = &mut self.running else { return };
        let Some(i) = r.views.iter().position(|v| v.window.id() == id) else { return };
        if i == 0 {
            r.egui_input.on_event(&r.views[0].window, &event);
        }
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => r.resized(i, size),
            WindowEvent::RedrawRequested if i == 0 => r.frame(),
            WindowEvent::Occluded(true) => r.engine.pause_adapting(),
            WindowEvent::CursorMoved { position, .. } => {
                r.views[i].cursor = Some(position);
                if r.igniting {
                    r.ignite(i, position, true);
                }
            }
            WindowEvent::CursorLeft { .. } => r.views[i].cursor = None,
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let (Some(pos), false) = (r.views[i].cursor, r.over_panel(i)) {
                    r.igniting = true;
                    r.ignite(i, pos, false);
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => r.igniting = false,
            WindowEvent::Touch(t) => {
                let pos = t.location;
                match t.phase {
                    TouchPhase::Started if !r.over_panel(i) => r.ignite(i, pos, false),
                    TouchPhase::Moved if !r.over_panel(i) => r.ignite(i, pos, true),
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !r.egui_ctx.egui_wants_keyboard_input() => {
                r.key(el, &event.logical_key);
            }
            _ => {}
        }
    }

    /// Natively, save the grid on the way out, as well as every few seconds.
    #[cfg(not(target_arch = "wasm32"))]
    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if let Some(r) = &mut self.running {
            r.engine.save_now();
        }
    }
}

fn fail(el: &ActiveEventLoop, msg: String) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = el;
        if let Some(body) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.body()) {
            body.set_text_content(Some(&msg));
        }
        web_sys::console::error_1(&msg.into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("tinderbox: {msg}");
        el.exit();
    }
}
