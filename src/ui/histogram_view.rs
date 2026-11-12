//! RGB histogram plot.

use astro_processor::histogram::{BINS, Histogram};
use eframe::egui::{self, Color32, Pos2, Sense, Stroke, StrokeKind, Vec2};

pub fn show(ui: &mut egui::Ui, hist: Option<&Histogram>, log_scale: bool, height: f32) {
    let (response, painter) = ui.allocate_painter(Vec2::new(ui.available_width(), height), Sense::hover());
    let rect = response.rect;
    painter.rect_filled(rect, 3.0, Color32::from_gray(14));
    for i in 1..4 {
        let x = rect.left() + rect.width() * i as f32 / 4.0;
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, Color32::from_gray(34)));
    }
    if let Some(h) = hist {
        let peak = h.peak() as f32;
        let norm = |v: u32| -> f32 {
            let v = v as f32;
            if log_scale { (1.0 + v).ln() / (1.0 + peak).ln() } else { v / peak }.min(1.0)
        };
        let colors = [
            Color32::from_rgb(235, 80, 80),
            Color32::from_rgb(90, 210, 110),
            Color32::from_rgb(95, 145, 250),
        ];
        for (c, color) in colors.iter().enumerate() {
            let points: Vec<Pos2> = (0..BINS)
                .map(|i| {
                    Pos2::new(
                        rect.left() + rect.width() * i as f32 / (BINS - 1) as f32,
                        rect.bottom() - 1.0 - (rect.height() - 4.0) * norm(h.channels[c][i]),
                    )
                })
                .collect();
            painter.line(points, Stroke::new(1.25, *color));
        }
        let (lo, hi) = h.clipped();
        let text = format!("clipped: {:.1}% black, {:.1}% white", lo * 100.0, hi * 100.0);
        response.on_hover_text(text);
    }
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, Color32::from_gray(50)), StrokeKind::Inside);
}
