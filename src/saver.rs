//! Screensaver mode (Linux, X11): draw into a window that a screen locker
//! owns, such as xsecurelock's or xscreensaver's. The locker creates the
//! window, handles input and the password prompt, and kills us when it's
//! done; we just draw the scene into it until then.
//!
//! The window comes from `--window-id ID` or, as both lockers set it, the
//! `XSCREENSAVER_WINDOW` environment variable. Settings are the ones saved
//! by the regular app, and the saver never writes them.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use raw_window_handle::{RawDisplayHandle, RawWindowHandle, XlibDisplayHandle, XlibWindowHandle};
use x11_dl::xlib;

use crate::engine::{Engine, Layout};
use crate::params::Params;
use crate::render::{Rect, ViewTargets};

/// How often to check whether the locker resized the window.
const RESIZE_POLL: Duration = Duration::from_millis(500);

pub fn run(args: &[String]) -> Result<(), String> {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let id = value("--window-id")
        .or_else(|| value("-window-id"))
        .or_else(|| std::env::var("XSCREENSAVER_WINDOW").ok())
        .ok_or("--saver needs --window-id or XSCREENSAVER_WINDOW")?;
    let id = id.trim();
    let window = match id.strip_prefix("0x").or_else(|| id.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => id.parse::<u64>(),
    }
    .map_err(|_| format!("bad window id {id:?}"))?;

    let x = xlib::Xlib::open().map_err(|e| format!("couldn't load Xlib: {e}"))?;
    // Lives until exit, as the surface needs it to.
    let dpy = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
    if dpy.is_null() {
        return Err("couldn't open the X display (is DISPLAY set?)".into());
    }
    let screen = unsafe { (x.XDefaultScreen)(dpy) };
    let size = || {
        let mut attrs: xlib::XWindowAttributes = unsafe { std::mem::zeroed() };
        let ok = unsafe { (x.XGetWindowAttributes)(dpy, window, &mut attrs) };
        (ok != 0).then(|| ((attrs.width as u32).max(1), (attrs.height as u32).max(1)))
    };
    let (mut w, mut h) = size().ok_or(format!("no window {id}"))?;

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
    let instance = wgpu::Instance::new(desc);
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::Xlib(XlibDisplayHandle::new(NonNull::new(dpy as *mut c_void), screen))),
        raw_window_handle: RawWindowHandle::Xlib(XlibWindowHandle::new(window as _)),
    };
    // Safety: the display stays open and the locker keeps the window until
    // it kills us.
    let surface = unsafe { instance.create_surface_unsafe(target) }.map_err(|e| format!("couldn't create a surface: {e}"))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|e| format!("no suitable GPU: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("tinderbox saver"),
        required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| format!("couldn't open the GPU: {e}"))?;

    let caps = surface.get_capabilities(&adapter);
    let surface_format =
        caps.formats.iter().find(|f| !f.is_srgb()).or(caps.formats.first()).copied().ok_or("the surface has no formats")?;
    let views_ok = adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::SURFACE_VIEW_FORMATS);
    let format = if views_ok { surface_format.remove_srgb_suffix() } else { surface_format };
    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: w,
        height: h,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        color_space: Default::default(),
        view_formats: if format != surface_format { vec![format] } else { vec![] },
    };
    surface.configure(&device, &config);

    let params = Params { paused: false, ..Params::load() };
    let layout = |w, h| Layout { views: vec![Rect { x: 0, y: 0, w, h }], scale: 1.0 };
    let mut e = Engine::new(device, queue, layout(w, h), params);
    let mut targets = None::<ViewTargets>;
    let mut checked = Instant::now();
    loop {
        if checked.elapsed() > RESIZE_POLL {
            checked = Instant::now();
            // The window is gone: the locker is done with us.
            let (nw, nh) = size().ok_or("the window went away")?;
            if (nw, nh) != (w, h) {
                (w, h) = (nw, nh);
                (config.width, config.height) = (w, h);
                surface.configure(&e.device, &config);
                e.set_layout(layout(w, h));
            }
        }
        e.tick();
        let rect = e.layout.views[0];
        let want = e.target_size(&rect);
        let t = match targets.take() {
            Some(t) if t.size == want => t,
            _ => ViewTargets::new(&e.device, &e.pipelines, want.0, want.1),
        };
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(&e.device, &config);
                targets = Some(t);
                continue;
            }
            _ => {
                // Occluded or timed out: don't spin.
                std::thread::sleep(Duration::from_millis(100));
                targets = Some(t);
                continue;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(format), ..Default::default() });
        let mut enc = e.device.create_command_encoder(&Default::default());
        e.draw_view(&mut enc, &rect, &t, &view, format);
        e.queue.submit([enc.finish()]);
        e.queue.present(frame);
        let _ = e.device.poll(wgpu::PollType::Poll);
        targets = Some(t);
    }
}
