//! Files tab: the five frame slots and the frame list of the selected one.

use super::{App, WARN};
use astro_processor::frames::{self, FrameKind, fmt_exposure, fmt_iso};
use eframe::egui::{self, Align, Layout, RichText};

impl App {
    pub(super) fn files_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // Slot selector.
        ui.horizontal_wrapped(|ui| {
            for kind in FrameKind::ALL {
                let set = self.frames.get(kind);
                let on = set.iter().filter(|f| f.enabled).count();
                let text = if on == set.len() {
                    format!("{}  {}", kind.label(), on)
                } else {
                    format!("{}  {}/{}", kind.label(), on, set.len())
                };
                ui.selectable_value(&mut self.slot, kind, text);
            }
        });
        let kind = self.slot;
        ui.label(RichText::new(kind.hint()).weak());
        if kind != FrameKind::Light {
            ui.label(RichText::new("Optional.").weak().italics());
        }

        ui.horizontal(|ui| {
            if ui.button("Add files…").clicked() {
                self.pick_and_scan(&ctx, kind, false);
            }
            if ui.button("Add folder…").clicked() {
                self.pick_and_scan(&ctx, kind, true);
            }
            let set = self.frames.get_mut(kind);
            if !set.is_empty() {
                if ui.button("Clear").clicked() {
                    set.clear();
                }
                let all = set.iter().all(|f| f.enabled);
                if ui.button(if all { "Untick all" } else { "Tick all" }).clicked() {
                    for f in set.iter_mut() {
                        f.enabled = !all;
                    }
                }
            }
        });
        let hint = if self.can_drop {
            "You can also drop files or folders onto the window."
        } else {
            "Dropping files onto the window needs the --x11 option on Wayland."
        };
        ui.label(RichText::new(hint).weak().small());

        let warnings = frames::warnings(&self.frames);
        if !warnings.is_empty() {
            ui.add_space(2.0);
            egui::CollapsingHeader::new(RichText::new(format!("{} warning(s)", warnings.len())).color(WARN))
                .default_open(true)
                .show(ui, |ui| {
                    for w in &warnings {
                        ui.label(RichText::new(format!("• {w}")).color(WARN).small());
                    }
                });
        }
        ui.separator();

        let set = self.frames.get_mut(kind);
        if set.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(format!("No {} added.", kind.label().to_lowercase())).weak()));
            return;
        }
        let show_stats = kind == FrameKind::Light && set.iter().any(|f| f.stats.is_some());
        const W_EXP: f32 = 44.0;
        const W_ISO: f32 = 34.0;
        const W_STAT: f32 = 40.0;
        let row_h = ui.spacing().interact_size.y;
        let cell = |ui: &mut egui::Ui, w: f32, text: RichText| {
            ui.allocate_ui_with_layout(egui::vec2(w, row_h), Layout::right_to_left(Align::Center), |ui| {
                // Fixed width, so columns line up whatever the text.
                ui.set_min_width(w);
                ui.label(text);
            });
        };

        // Header.
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(14.0);
                if show_stats {
                    cell(ui, W_STAT, RichText::new("Score").weak().small());
                    cell(ui, W_STAT, RichText::new("FWHM").weak().small());
                    cell(ui, W_STAT, RichText::new("Stars").weak().small());
                }
                cell(ui, W_ISO, RichText::new("ISO").weak().small());
                cell(ui, W_EXP, RichText::new("Exp.").weak().small());
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(RichText::new("Use   File").weak().small());
                });
            });
        });

        let n = set.len();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            for f in &mut set[range] {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut f.enabled, "");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(14.0);
                        if show_stats {
                            match f.stats {
                                Some(s) if s.registered => {
                                    cell(ui, W_STAT, RichText::new(format!("{:.0}", s.score)));
                                    cell(ui, W_STAT, RichText::new(format!("{:.2}", s.fwhm)));
                                    cell(ui, W_STAT, RichText::new(s.stars.to_string()));
                                }
                                Some(_) => {
                                    cell(ui, W_STAT * 3.0 + 8.0, RichText::new("not aligned").color(WARN));
                                }
                                None => cell(ui, W_STAT * 3.0 + 8.0, RichText::new("–").weak()),
                            }
                        }
                        cell(ui, W_ISO, RichText::new(fmt_iso(f.meta.iso)));
                        cell(ui, W_EXP, RichText::new(fmt_exposure(f.meta.exposure)));
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let name = f.name();
                            let label = match &f.note {
                                Some(_) => RichText::new(format!("! {name}")).color(WARN),
                                None if !f.enabled => RichText::new(name).weak(),
                                None => RichText::new(name),
                            };
                            let r = ui.add(egui::Label::new(label).truncate());
                            let mut tip = f.path.display().to_string();
                            if let Some(d) = &f.meta.datetime {
                                tip += &format!("\n{d}");
                            }
                            if let Some(note) = &f.note {
                                tip += &format!("\nUnticked by default: {note}");
                            }
                            r.on_hover_text(tip);
                        });
                    });
                });
            }
        });
    }
}
