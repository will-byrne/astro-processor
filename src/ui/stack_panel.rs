//! Stack tab: calibration toggles, the Stack button, progress and options.

use super::{ACCENT, App, WARN};
use astro_processor::calibrate::master;
use astro_processor::frames::{self, FrameKind};
use astro_processor::stack::StackMethod;
use eframe::egui::{self, Color32, RichText};

impl App {
    pub(super) fn stack_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let count = |app: &App, k: FrameKind| app.frames.get(k).iter().filter(|f| f.enabled).count();
        let lights = count(self, FrameKind::Light);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new("Calibration").strong());
            let cal = &mut self.stack_opts.calibration;
            let rows = [
                (FrameKind::Dark, &mut cal.darks),
                (FrameKind::Flat, &mut cal.flats),
                (FrameKind::DarkFlat, &mut cal.dark_flats),
                (FrameKind::Bias, &mut cal.bias),
            ];
            for (kind, flag) in rows {
                let n = self.frames.get(kind).iter().filter(|f| f.enabled).count();
                ui.add_enabled_ui(n > 0, |ui| {
                    let mut shown = *flag && n > 0;
                    let text = if n > 0 { format!("Use {} ({n})", kind.label().to_lowercase()) } else { format!("{}: none added", kind.label()) };
                    if ui.checkbox(&mut shown, text).changed() {
                        *flag = shown;
                    }
                });
            }

            ui.add_space(8.0);
            let running = self.stack_job.is_some();
            if let Some(job) = &self.stack_job {
                let s = job.progress.snapshot();
                let frac = if s.total > 0 { s.done as f32 / s.total as f32 } else { 0.0 };
                ui.label(RichText::new(&s.stage).strong());
                ui.add(egui::ProgressBar::new(frac).text(format!("{} / {}", s.done.min(s.total), s.total)));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:.0}s elapsed", job.started.elapsed().as_secs_f32())).weak());
                    if job.progress.is_cancelled() {
                        ui.label(RichText::new("cancelling…").weak());
                    } else if ui.button("Cancel").clicked() {
                        job.progress.cancel();
                    }
                });
                // Keep the elapsed time ticking.
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            } else {
                let ready = lights > 0 && !self.busy();
                let button = egui::Button::new(
                    RichText::new(format!("Stack {lights} light frame(s)")).size(15.0).color(Color32::BLACK).strong(),
                )
                .fill(if ready { ACCENT } else { Color32::from_gray(70) })
                .min_size(egui::vec2(ui.available_width(), 36.0));
                if ui.add_enabled(ready, button).clicked() {
                    self.start_stack(&ctx);
                }
                if lights == 0 {
                    ui.label(RichText::new("Add light frames in the Files tab first.").weak());
                } else if self.stack.is_some() {
                    ui.label(RichText::new("Stacking again replaces the current result and resets the processing settings.").weak().small());
                }
            }

            let warnings = frames::warnings(&self.frames);
            if !warnings.is_empty() {
                ui.add_space(6.0);
                for w in &warnings {
                    ui.label(RichText::new(format!("• {w}")).color(WARN).small());
                }
            }

            ui.add_space(8.0);
            ui.add_enabled_ui(!running, |ui| {
                egui::CollapsingHeader::new("Advanced").default_open(false).show(ui, |ui| {
                    let o = &mut self.stack_opts;
                    egui::ComboBox::from_label("Method")
                        .selected_text(match o.method {
                            StackMethod::KappaSigma => "Kappa-sigma clipping",
                            StackMethod::Average => "Average",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut o.method, StackMethod::KappaSigma, "Kappa-sigma clipping")
                                .on_hover_text("Rejects satellites, planes and hot pixels. Reads every frame twice.");
                            ui.selectable_value(&mut o.method, StackMethod::Average, "Average")
                                .on_hover_text("Fastest; keeps everything, including satellite trails.");
                        });
                    ui.add_enabled_ui(o.method == StackMethod::KappaSigma, |ui| {
                        ui.add(egui::Slider::new(&mut o.kappa, 1.5..=5.0).text("Kappa"))
                            .on_hover_text("Samples further than this many standard deviations from the mean are rejected. Lower rejects more.");
                        ui.add(egui::Slider::new(&mut o.iterations, 1..=3).text("Rejection passes"))
                            .on_hover_text("Each extra pass reads every frame again.");
                    });

                    let current = o
                        .reference
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Automatic (best frame)".into());
                    egui::ComboBox::from_label("Reference").selected_text(current).show_ui(ui, |ui| {
                        ui.selectable_value(&mut o.reference, None, "Automatic (best frame)");
                        for f in self.frames.lights.iter().filter(|f| f.enabled) {
                            ui.selectable_value(&mut o.reference, Some(f.path.clone()), f.name());
                        }
                    });
                    ui.add(egui::Slider::new(&mut o.keep_best_percent, 50.0..=100.0).suffix("%").fixed_decimals(0).text("Keep best"))
                        .on_hover_text("Drop the lowest-scoring frames (few stars or soft focus).");

                    let mut hot = o.hot_pixel_sigma > 0.0;
                    if ui.checkbox(&mut hot, "Repair hot pixels").changed() {
                        o.hot_pixel_sigma = if hot { 8.0 } else { 0.0 };
                    }
                    if hot {
                        ui.add(egui::Slider::new(&mut o.hot_pixel_sigma, 3.0..=20.0).fixed_decimals(1).text("Threshold (sigma)"))
                            .on_hover_text("Isolated pixels this far above their neighbours are replaced. Lower is more aggressive.");
                    }
                    ui.checkbox(&mut o.normalize, "Match brightness between frames")
                        .on_hover_text("Equalises sky background and transparency before combining, so rejection works when conditions change.");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut o.use_cache, "Cache master frames")
                            .on_hover_text("Reuse combined darks/flats/bias between runs.");
                        if ui.small_button("Clear cache").clicked() {
                            let n = master::default_cache_dir().map(|d| master::clear_cache(&d)).unwrap_or(0);
                            self.status = Some((format!("Removed {n} cached master frame(s)."), false));
                        }
                    });
                    if ui.small_button("Reset to defaults").clicked() {
                        let cal = o.calibration;
                        *o = Default::default();
                        o.calibration = cal;
                    }
                });
            });

            let log: Vec<String> = match &self.stack_job {
                Some(job) => job.progress.snapshot().log,
                None => self.log.clone(),
            };
            if !log.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Log").strong());
                for line in &log {
                    ui.label(RichText::new(line).small().weak());
                }
            }
        });
    }
}
