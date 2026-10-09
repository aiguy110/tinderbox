//! Headless rendering to PNG, for checking a multi-monitor layout (or any
//! change to the renderers) without a display. Each virtual monitor renders
//! through its own view, exactly as the windowed app does, and the views are
//! also stitched into one image of the whole layout so seams show up.

use std::path::Path;

use crate::engine::{Engine, Layout};
use crate::params::{Params, Renderer};
use crate::render::{Rect, ViewTargets};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub fn run(args: &[String]) -> Result<(), String> {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str);
    let dir = Path::new(value("--shot").ok_or("--shot needs a directory")?);
    let rects: Vec<Rect> = value("--layout")
        .unwrap_or("1280x720+0+0")
        .split(',')
        .map(|s| Rect::parse(s.trim()).ok_or(format!("bad layout entry {s:?}; expected WxH+X+Y")))
        .collect::<Result<_, _>>()?;
    let num =
        |flag, default: f32| value(flag).map(|v| v.parse::<f32>().map_err(|_| format!("{flag} takes a number"))).unwrap_or(Ok(default));
    let params = Params {
        cell_size: num("--cell", 64.0)?,
        renderer: if args.iter().any(|a| a == "--flat") { Renderer::Flat } else { Renderer::Scene },
        night: args.iter().any(|a| a == "--night"),
        steps_per_second: 1.0,
        paused: true,
        ..Params::default()
    };
    let steps = num("--steps", 3.0)? as u32;
    let progress = num("--progress", 0.5)?;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU: {e}"))?;
    let info = adapter.get_info();
    eprintln!("adapter: {} ({:?})", info.name, info.backend);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;

    let layout = Layout { views: rects, scale: 1.0 }.normalized();
    let mut e = Engine::new(device, queue, layout.clone(), params);
    for r in &layout.views {
        e.ignite(r.x as f64 + r.w as f64 / 2.0, r.y as f64 + r.h as f64 / 2.0, false);
    }
    for _ in 0..steps {
        e.step_once();
    }
    e.debt = progress;

    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (lw, lh) = layout.size();
    let mut whole = vec![0u8; (lw * lh * 4) as usize];
    for (i, r) in layout.views.iter().enumerate() {
        let (tw, th) = e.target_size(r);
        let targets = ViewTargets::new(&e.device, &e.pipelines, tw, th);
        let out = e.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shot"),
            size: wgpu::Extent3d { width: r.w, height: r.h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let row = (r.w * 4).next_multiple_of(256);
        let buf = e.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shot"),
            size: (row * r.h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = e.device.create_command_encoder(&Default::default());
        e.draw_view(&mut enc, r, &targets, &out.create_view(&Default::default()), FORMAT);
        enc.copy_texture_to_buffer(
            out.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: r.w, height: r.h, depth_or_array_layers: 1 },
        );
        e.queue.submit([enc.finish()]);
        buf.map_async(wgpu::MapMode::Read, .., |_| {});
        e.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        let data = buf.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
        let mut pixels = Vec::with_capacity((r.w * r.h * 4) as usize);
        for y in 0..r.h {
            let line = &data[(y * row) as usize..][..(r.w * 4) as usize];
            pixels.extend_from_slice(line);
            let at = (((r.y as u32 + y) * lw + r.x as u32) * 4) as usize;
            whole[at..at + line.len()].copy_from_slice(line);
        }
        write_png(&dir.join(format!("view-{i}.png")), r.w, r.h, &pixels)?;
    }
    write_png(&dir.join("layout.png"), lw, lh, &whole)?;
    eprintln!("{}×{} cells; wrote {} views and layout.png to {}", e.sim.width, e.sim.height, layout.views.len(), dir.display());
    Ok(())
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}
