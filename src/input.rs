//! Feeds winit events to egui. egui-winit doesn't build for the web, and the
//! panel only needs the pointer (and the wheel, for the renderer menu), so
//! this is the small part of it we need, for both builds.

use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::window::{CursorIcon, Window};

pub struct EguiInput {
    start: web_time::Instant,
    events: Vec<egui::Event>,
    pointer: Option<egui::Pos2>,
    focused: bool,
}

impl EguiInput {
    pub fn new() -> Self {
        EguiInput { start: web_time::Instant::now(), events: Vec::new(), pointer: None, focused: true }
    }

    pub fn on_event(&mut self, window: &Window, event: &WindowEvent) {
        let ppp = window.scale_factor() as f32;
        let pos = |x: f64, y: f64| egui::pos2(x as f32 / ppp, y as f32 / ppp);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = pos(position.x, position.y);
                self.pointer = Some(p);
                self.events.push(egui::Event::PointerMoved(p));
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.events.push(egui::Event::PointerGone);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => egui::PointerButton::Primary,
                    MouseButton::Right => egui::PointerButton::Secondary,
                    MouseButton::Middle => egui::PointerButton::Middle,
                    _ => return,
                };
                if let Some(p) = self.pointer {
                    self.events.push(egui::Event::PointerButton {
                        pos: p,
                        button,
                        pressed: *state == ElementState::Pressed,
                        modifiers: Default::default(),
                    });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (unit, delta) = match *delta {
                    MouseScrollDelta::LineDelta(x, y) => (egui::MouseWheelUnit::Line, egui::vec2(x, y)),
                    MouseScrollDelta::PixelDelta(p) => (egui::MouseWheelUnit::Point, egui::vec2(p.x as f32, p.y as f32) / ppp),
                };
                self.events.push(egui::Event::MouseWheel { unit, delta, modifiers: Default::default(), phase: egui::TouchPhase::Move });
            }
            // A touch is a press, drags and a release of the primary button.
            WindowEvent::Touch(t) => {
                let p = pos(t.location.x, t.location.y);
                self.pointer = Some(p);
                self.events.push(egui::Event::PointerMoved(p));
                let pressed = match t.phase {
                    TouchPhase::Started => Some(true),
                    TouchPhase::Ended | TouchPhase::Cancelled => Some(false),
                    TouchPhase::Moved => None,
                };
                if let Some(pressed) = pressed {
                    self.events.push(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    });
                    if !pressed {
                        self.pointer = None;
                        self.events.push(egui::Event::PointerGone);
                    }
                }
            }
            WindowEvent::Focused(f) => {
                self.focused = *f;
                self.events.push(egui::Event::WindowFocused(*f));
            }
            _ => {}
        }
    }

    /// `surface_ppp` is the surface's pixels per point, which can be less
    /// than the window's.
    pub fn take(&mut self, window: &Window, surface_ppp: f32) -> egui::RawInput {
        let ppp = window.scale_factor() as f32;
        let size = window.inner_size();
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.width as f32, size.height as f32) / ppp)),
            time: Some(self.start.elapsed().as_secs_f64()),
            focused: self.focused,
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        if let Some(v) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
            v.native_pixels_per_point = Some(surface_ppp);
        }
        raw
    }
}

/// The canvas gets a crosshair, except over the panel.
pub fn cursor(window: &Window, icon: egui::CursorIcon) {
    window.set_cursor(match icon {
        egui::CursorIcon::Default => CursorIcon::Crosshair,
        egui::CursorIcon::PointingHand => CursorIcon::Pointer,
        egui::CursorIcon::ResizeHorizontal | egui::CursorIcon::ResizeColumn => CursorIcon::EwResize,
        egui::CursorIcon::Grab => CursorIcon::Grab,
        egui::CursorIcon::Grabbing => CursorIcon::Grabbing,
        _ => CursorIcon::Default,
    });
}
