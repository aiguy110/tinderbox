//! The parameter panel, drawn with egui on the primary view.

use egui::{Align, Align2, Color32, CornerRadius, Frame, Layout, RichText, Slider, Stroke, Vec2};

use crate::params::{Params, Renderer};

const TITLE: Color32 = Color32::from_rgb(0xff, 0xb3, 0x47);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe2, 0xda);
const DIM: Color32 = Color32::from_rgb(0x9a, 0x92, 0x8a);
const VALUE: Color32 = Color32::from_rgb(0xff, 0xd0, 0x8a);
const ACCENT: Color32 = Color32::from_rgb(0xff, 0x8c, 0x32);
const BUTTON: Color32 = Color32::from_rgb(0x2a, 0x24, 0x20);
const BUTTON_HOVER: Color32 = Color32::from_rgb(0x3a, 0x30, 0x29);
const BORDER: Color32 = Color32::from_rgb(0x4a, 0x42, 0x3a);

#[derive(Default)]
pub struct Actions {
    pub step: bool,
    pub reseed: bool,
    pub fullscreen: bool,
}

pub fn style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT;
        v.slider_trailing_fill = true;
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
            w.corner_radius = CornerRadius::same(6);
            w.bg_stroke = Stroke::new(1.0, BORDER);
        }
        v.widgets.inactive.weak_bg_fill = BUTTON;
        v.widgets.inactive.bg_fill = BUTTON;
        v.widgets.hovered.weak_bg_fill = BUTTON_HOVER;
        v.widgets.hovered.bg_fill = BUTTON_HOVER;
        v.widgets.active.bg_fill = ACCENT;
        s.spacing.item_spacing = Vec2::new(6.0, 4.0);
    });
}

fn sci(v: f64) -> String {
    if v >= 0.01 { format!("{v:.2}") } else { format!("{v:.1e}") }
}

fn speed(v: f64) -> String {
    if v >= 1.0 { format!("{} steps/s", v.round()) } else { format!("{:.1} s/step", 1.0 / v) }
}

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    log: bool,
    step: f64,
    format: impl Fn(f64) -> String,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format(*value as f64)).color(VALUE).monospace());
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    let mut s = Slider::new(value, range).show_value(false).logarithmic(log);
    if step > 0.0 {
        s = s.step_by(step);
    }
    ui.add(s);
}

/// Draw the panel (or the button that brings it back). Changes params in
/// place; buttons that need the engine come back as actions.
pub fn panel(ctx: &egui::Context, p: &mut Params, stats: &str, hint: &str) -> Actions {
    let mut actions = Actions::default();
    if p.panel_hidden {
        egui::Area::new("show-panel".into()).anchor(Align2::RIGHT_TOP, Vec2::new(-12.0, 12.0)).show(ctx, |ui| {
            let b = egui::Button::new(RichText::new("☰").size(16.0))
                .fill(Color32::from_rgba_unmultiplied(16, 14, 12, 150))
                .min_size(Vec2::splat(34.0));
            if ui.add(b).on_hover_text("Show panel (H)").clicked() {
                p.panel_hidden = false;
            }
        });
        return actions;
    }
    egui::Area::new("panel".into()).anchor(Align2::RIGHT_TOP, Vec2::new(-12.0, 12.0)).show(ctx, |ui| {
        Frame::new()
            .fill(Color32::from_rgba_unmultiplied(16, 14, 12, 210))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(14, 12))
            .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(128) })
            .show(ui, |ui| {
                ui.set_width(280.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("tinderbox").size(16.0).strong().color(TITLE));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Hide panel (H)").clicked() {
                            p.panel_hidden = true;
                        }
                    });
                });
                ui.label(RichText::new(hint).small().color(DIM));
                ui.label(RichText::new(stats).small().color(DIM));
                ui.add_space(4.0);

                slider(ui, "Cell size", &mut p.cell_size, 1.0..=128.0, false, 1.0, |v| format!("{v}px"));
                slider(ui, "Growth p", &mut p.growth, 1e-5..=1.0, true, 0.0, sci);
                ui.add_enabled_ui(!p.critical, |ui| slider(ui, "Lightning f", &mut p.lightning, 1e-8..=0.1, true, 0.0, sci));
                slider(ui, "Speed", &mut p.steps_per_second, 0.1..=480.0, true, 0.0, speed);
                slider(ui, "Trees per side", &mut p.trees, 1.0..=8.0, false, 1.0, |v| format!("{} per cell", v * v));
                slider(ui, "Regrowth", &mut p.regrow, 1.0..=240.0, true, 1.0, |v| format!("{v} steps"));
                slider(ui, "Snag life", &mut p.snag, 1.0..=120.0, true, 1.0, |v| format!("{v} steps"));
                slider(ui, "Seed density", &mut p.initial_density, 0.0..=1.0, false, 0.01, |v| format!("{}%", (v * 100.0).round()));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut p.critical, "Lock to critical")
                        .on_hover_text("Set lightning from growth and the grid size, where fire sizes come closest to a power law. Caps growth too: faster regrowth lets fires burn back into their own wake.");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("p/f ≈ {}", (p.growth / p.lightning).round())).small().color(DIM));
                    });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Renderer");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let name = |r| if r == Renderer::Scene { "Procedural scene" } else { "Flat colors" };
                        egui::ComboBox::from_id_salt("renderer").selected_text(name(p.renderer)).show_ui(ui, |ui| {
                            for r in [Renderer::Scene, Renderer::Flat] {
                                ui.selectable_value(&mut p.renderer, r, name(r));
                            }
                        });
                    });
                });

                ui.add_space(6.0);
                egui::Grid::new("checks").num_columns(2).spacing(Vec2::new(12.0, 4.0)).show(ui, |ui| {
                    ui.checkbox(&mut p.paused, "Paused");
                    ui.checkbox(&mut p.moore, "8-neighbor spread");
                    ui.end_row();
                    ui.checkbox(&mut p.wrap, "Wrap edges");
                    ui.checkbox(&mut p.grid_lines, "Grid lines");
                    ui.end_row();
                    ui.checkbox(&mut p.night, "Night");
                    ui.checkbox(&mut p.river, "River");
                    ui.end_row();
                });
                if p.river {
                    ui.add_space(4.0);
                    slider(ui, "River width", &mut p.river_width, 4.0..=200.0, true, 1.0, |v| format!("{v}px"));
                    slider(ui, "Meander rate", &mut p.meander, 0.1..=50.0, true, 0.0, |v| format!("{v:.1}×"));
                    slider(ui, "Ember jumps", &mut p.spotting, 0.0..=0.05, false, 0.0005, |v| format!("{:.2}%", v * 100.0));
                    slider(ui, "Terrain", &mut p.terrain, 0.0..=2.0, false, 0.0, |v| format!("{v:.1}×"));
                }

                ui.add_space(8.0);
                ui.columns(4, |cols| {
                    let button = |ui: &mut egui::Ui, label| ui.add_sized([ui.available_width(), 24.0], egui::Button::new(label)).clicked();
                    if button(&mut cols[0], "Step") {
                        p.paused = true;
                        actions.step = true;
                    }
                    actions.reseed = button(&mut cols[1], "Reseed");
                    if button(&mut cols[2], "Defaults") {
                        *p = Params { paused: p.paused, panel_hidden: p.panel_hidden, ..Params::default() };
                    }
                    actions.fullscreen = button(&mut cols[3], "Full");
                });
            });
    });
    actions
}
