//! Process tab: histogram and one collapsible section per tool.

use super::{App, curve_editor, histogram_view};
use astro_processor::io::import;
use astro_processor::process::params::{self, Level, ProcessParams};
use astro_processor::process::pipeline;
use eframe::egui::{self, Color32, RichText};
use std::ops::RangeInclusive;

/// Slider that returns to `default` on double-click.
fn slider(ui: &mut egui::Ui, value: &mut f32, range: RangeInclusive<f32>, default: f32, label: &str) -> egui::Response {
    let r = ui.add(egui::Slider::new(value, range).text(label));
    if r.double_clicked() {
        *value = default;
    }
    r
}

/// Collapsible section with a "Reset" link when it differs from the default.
fn section<T: PartialEq + Default>(
    ui: &mut egui::Ui,
    title: &str,
    open: bool,
    value: &mut T,
    body: impl FnOnce(&mut egui::Ui, &mut T),
) {
    let changed = *value != T::default();
    let heading = if changed { RichText::new(format!("{title}  •")).strong() } else { RichText::new(title).strong() };
    egui::CollapsingHeader::new(heading).id_salt(title).default_open(open).show(ui, |ui| {
        body(ui, value);
        if changed && ui.small_button("Reset section").clicked() {
            *value = T::default();
        }
    });
}

const CHANNELS: [(&str, Color32); 4] = [
    ("RGB", Color32::from_gray(220)),
    ("Red", Color32::from_rgb(235, 80, 80)),
    ("Green", Color32::from_rgb(90, 210, 110)),
    ("Blue", Color32::from_rgb(95, 145, 250)),
];

fn channel_picker(ui: &mut egui::Ui, selected: &mut usize) {
    ui.horizontal(|ui| {
        for (i, (name, color)) in CHANNELS.iter().enumerate() {
            ui.selectable_value(selected, i, RichText::new(*name).color(*color));
        }
    });
}

impl App {
    pub(super) fn process_panel(&mut self, ui: &mut egui::Ui) {
        if self.stack.is_none() {
            ui.add_space(12.0);
            ui.label(RichText::new("Nothing to process yet. Stack your frames in the Stack tab, or open a project or an already-stacked image.").weak());
            return;
        }
        histogram_view::show(ui, self.histogram.as_ref(), self.hist_log, 96.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.hist_log, "Log scale");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Reset all")
                    .on_hover_text("Back to the automatic stretch and colour balance (or, for an image that was opened already stretched, to the image as it is)")
                    .clicked()
                    && let Some(stack) = &self.stack {
                        let crop = self.params.geometry.clone();
                        self.params = if import::looks_stretched(stack) {
                            ProcessParams::default()
                        } else {
                            pipeline::auto_params(stack, self.wb, Some(crop.crop))
                        };
                        self.params.geometry = crop;
                        self.edit_label = Some("Reset all");
                    }
            });
        });
        ui.separator();

        let model = self.model.clone();
        let covered = self.covered;
        let stack_size = self.stack.as_ref().map(|s| (s.width as f32, s.height as f32)).unwrap_or((1.0, 1.0));
        let wb = self.wb;
        let hist = self.histogram.clone();
        let p: &mut ProcessParams = &mut self.params;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            section(ui, "Stretch", true, &mut p.stretch, |ui, s| {
                if ui
                    .add_enabled(model.is_some(), egui::Button::new("Auto stretch"))
                    .on_hover_text("Set the black point and midtones from the sky background")
                    .clicked()
                    && let Some(m) = &model {
                        *s = pipeline::auto_stretch(m);
                        self.edit_label = Some("Auto stretch");
                    }
                ui.add(egui::Slider::new(&mut s.black, 0.0..=0.2).logarithmic(true).smallest_positive(1e-4).fixed_decimals(4).text("Black point"))
                    .on_hover_text("Linear level that becomes black. Raise it until the sky is dark but not clipped.");
                ui.add(egui::Slider::new(&mut s.midtone, 0.0005..=0.5).logarithmic(true).fixed_decimals(4).text("Midtones"))
                    .on_hover_text("Level that maps to mid-grey. Lower brightens faint detail; 0.5 is no stretch.");
                ui.add(egui::Slider::new(&mut s.asinh, 0.0..=1000.0).logarithmic(true).smallest_positive(1.0).fixed_decimals(0).text("Arcsinh"))
                    .on_hover_text("Extra stretch that keeps star colours from washing out. 0 is off.");
            });

            section(ui, "Tone", true, &mut p.tone, |ui, t| {
                slider(ui, &mut t.exposure, -3.0..=3.0, 0.0, "Exposure (stops)");
                slider(ui, &mut t.contrast, -1.0..=1.0, 0.0, "Contrast");
                slider(ui, &mut t.highlights, -1.0..=1.0, 0.0, "Highlights");
                slider(ui, &mut t.shadows, -1.0..=1.0, 0.0, "Shadows");
                slider(ui, &mut t.saturation, 0.0..=3.0, 1.0, "Saturation");
                slider(ui, &mut t.green_removal, 0.0..=1.0, 0.0, "Green removal")
                    .on_hover_text("Removes the green cast typical of colour cameras.");
            });

            section(ui, "Levels", false, &mut p.levels, |ui, l| {
                channel_picker(ui, &mut self.levels_channel);
                let level: &mut Level = match self.levels_channel {
                    1 => &mut l.r,
                    2 => &mut l.g,
                    3 => &mut l.b,
                    _ => &mut l.master,
                };
                slider(ui, &mut level.black, 0.0..=1.0, 0.0, "Black");
                let r = ui.add(egui::Slider::new(&mut level.gamma, 0.1..=5.0).logarithmic(true).text("Gamma"));
                if r.double_clicked() {
                    level.gamma = 1.0;
                }
                slider(ui, &mut level.white, 0.0..=1.0, 1.0, "White");
                if level.white < level.black + 0.01 {
                    level.white = (level.black + 0.01).min(1.0);
                    level.black = level.black.min(level.white - 0.01);
                }
            });

            section(ui, "Curves", false, &mut p.curves, |ui, c| {
                channel_picker(ui, &mut self.curve_channel);
                let ch = self.curve_channel;
                let points = match ch {
                    1 => &mut c.r,
                    2 => &mut c.g,
                    3 => &mut c.b,
                    _ => &mut c.master,
                };
                let backdrop = hist.as_ref().map(|h| (h, if ch == 0 { 3 } else { ch - 1 }));
                curve_editor::show(ui, points, CHANNELS[ch].1, backdrop);
                ui.horizontal(|ui| {
                    if ui.small_button("Reset this curve").clicked() {
                        *points = params::identity_curve();
                    }
                    ui.label(RichText::new("Click to add a point, right-click to remove.").weak().small());
                });
            });

            section(ui, "Colour balance", false, &mut p.color, |ui, c| {
                ui.checkbox(&mut c.neutralize_background, "Neutral sky background")
                    .on_hover_text("Shifts each channel so the sky is grey rather than tinted by light pollution.");
                let names = ["Red", "Green", "Blue"];
                for i in 0..3 {
                    let r = ui.add(egui::Slider::new(&mut c.wb[i], 0.25..=4.0).logarithmic(true).text(names[i]));
                    if r.double_clicked() {
                        c.wb[i] = wb[i];
                    }
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Camera white balance").clicked() {
                        c.wb = wb;
                        self.edit_label = Some("Camera white balance");
                    }
                    if ui.small_button("Equal").clicked() {
                        c.wb = [1.0; 3];
                        self.edit_label = Some("Equal white balance");
                    }
                });
            });

            section(ui, "Background gradient", false, &mut p.gradient, |ui, g| {
                ui.checkbox(&mut g.enabled, "Remove gradient")
                    .on_hover_text("Models light-pollution gradients and vignetting as a smooth surface and subtracts it.");
                ui.add_enabled_ui(g.enabled, |ui| {
                    ui.add(egui::Slider::new(&mut g.degree, 1..=4).text("Complexity"))
                        .on_hover_text("1 is a simple tilt; higher follows more complicated gradients but may eat into large faint objects.");
                    slider(ui, &mut g.tolerance, 0.3..=3.0, 1.0, "Object protection")
                        .on_hover_text("Lower treats more of the image as object and leaves it out of the background model.");
                    slider(ui, &mut g.strength, 0.0..=1.0, 1.0, "Strength");
                });
                ui.label(RichText::new("Press Auto stretch again after changing this.").weak().small());
            });

            section(ui, "Noise reduction & sharpening", false, &mut p.detail, |ui, d| {
                slider(ui, &mut d.denoise_luma, 0.0..=1.0, 0.0, "Luminance noise");
                slider(ui, &mut d.denoise_chroma, 0.0..=1.0, 0.0, "Colour noise");
                slider(ui, &mut d.sharpen, 0.0..=2.0, 0.0, "Sharpen");
                ui.label(RichText::new("Zoom to 1:1 to judge these accurately.").weak().small());
            });

            section(ui, "Star reduction", false, &mut p.stars, |ui, s| {
                slider(ui, &mut s.reduce, 0.0..=1.0, 0.0, "Amount");
                slider(ui, &mut s.radius, 0.5..=4.0, 1.5, "Radius (px)");
                ui.label(RichText::new("Zoom to 1:1 to judge this accurately.").weak().small());
            });

            section(ui, "Crop & rotate", false, &mut p.geometry, |ui, g| {
                let pct = |ui: &mut egui::Ui, v: &mut f32, default: f32, label: &str| {
                    let mut shown = *v * 100.0;
                    let r = ui.add(egui::Slider::new(&mut shown, 0.0..=100.0).suffix("%").fixed_decimals(1).text(label));
                    // Only write on a real change: the round trip through
                    // percent is not exact and would look like an edit.
                    if r.double_clicked() {
                        *v = default;
                    } else if r.changed() {
                        *v = shown / 100.0;
                    }
                };
                pct(ui, &mut g.crop[0], 0.0, "Left edge");
                pct(ui, &mut g.crop[2], 1.0, "Right edge");
                pct(ui, &mut g.crop[1], 0.0, "Top edge");
                pct(ui, &mut g.crop[3], 1.0, "Bottom edge");
                g.crop[2] = g.crop[2].max(g.crop[0] + 0.02).min(1.0);
                g.crop[0] = g.crop[0].min(g.crop[2] - 0.02).max(0.0);
                g.crop[3] = g.crop[3].max(g.crop[1] + 0.02).min(1.0);
                g.crop[1] = g.crop[1].min(g.crop[3] - 0.02).max(0.0);
                ui.horizontal(|ui| {
                    if let Some(c) = covered
                        && ui
                            .small_button("Crop to stacked area")
                            .on_hover_text("Trim the edges that not every frame covers")
                            .clicked()
                        {
                            let (w, h) = stack_size;
                            g.crop = [c.x as f32 / w, c.y as f32 / h, (c.x + c.w) as f32 / w, (c.y + c.h) as f32 / h];
                            self.edit_label = Some("Crop to stacked area");
                        }
                    if ui.small_button("No crop").clicked() {
                        g.crop = [0.0, 0.0, 1.0, 1.0];
                        self.edit_label = Some("No crop");
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Rotate left").clicked() {
                        g.quarter_turns = (g.quarter_turns + 3) % 4;
                    }
                    if ui.button("Rotate right").clicked() {
                        g.quarter_turns = (g.quarter_turns + 1) % 4;
                    }
                    ui.checkbox(&mut g.flip_h, "Mirror");
                    ui.checkbox(&mut g.flip_v, "Flip");
                });
                slider(ui, &mut g.angle, -15.0..=15.0, 0.0, "Straighten (°)");
            });
            ui.add_space(12.0);
        });
    }
}
