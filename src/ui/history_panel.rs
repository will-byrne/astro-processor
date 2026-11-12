//! History panel: every edit made in the Process tab, newest first. Clicking
//! a step returns the image to how it was at that point.

use super::{ACCENT, App};
use astro_processor::process::history;
use eframe::egui::{self, Align, Layout, RichText, Sense};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl App {
    pub(super) fn history_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("History").strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo").small()).on_hover_text("Ctrl+Shift+Z").clicked() {
                    self.redo();
                }
                if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo").small()).on_hover_text("Ctrl+Z").clicked() {
                    self.undo();
                }
            });
        });
        ui.separator();

        let now = unix_now();
        let current = self.history.current();
        let mut jump = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, step) in self.history.steps().iter().enumerate().rev() {
                let undone = i > current;
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), Sense::click());
                if i == current {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
                        1.0,
                        ACCENT,
                    );
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
                }
                let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 3.0))));
                row.spacing_mut().item_spacing.y = 0.0;
                let label = RichText::new(&step.label);
                let label = if undone { label.weak() } else if i == current { label.strong() } else { label };
                row.add(egui::Label::new(label).truncate().selectable(false));
                row.add(
                    egui::Label::new(RichText::new(history::ago(now, step.time)).small().weak()).selectable(false),
                );
                let response = response.on_hover_text(if undone {
                    format!("{}\nUndone. Click to bring it back.", step.label)
                } else if i == current {
                    format!("{}\nThe image as shown now.", step.label)
                } else {
                    format!("{}\nClick to go back to this point.", step.label)
                });
                if response.clicked() && i != current {
                    jump = Some(i);
                }
                if i == current && self.scroll_to_step {
                    ui.scroll_to_rect(rect, None);
                }
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Click a step to go back to it. Later steps stay until you make a new change.")
                    .small()
                    .weak(),
            );
        });
        self.scroll_to_step = false;
        if let Some(i) = jump
            && let Some(p) = self.history.jump(i)
        {
            self.params = p.clone();
        }
        // Keep the "5 min ago" labels moving.
        ui.ctx().request_repaint_after(Duration::from_secs(20));
    }

    pub(super) fn undo(&mut self) {
        if let Some(p) = self.history.undo() {
            self.params = p.clone();
            self.scroll_to_step = true;
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(p) = self.history.redo() {
            self.params = p.clone();
            self.scroll_to_step = true;
        }
    }
}
