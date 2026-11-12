//! Interactive tone-curve editor: drag points, click to add, right-click or
//! double-click a point to remove it.

use astro_processor::histogram::{BINS, Histogram};
use astro_processor::process::curves;
use astro_processor::process::params::CurvePoints;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

const HANDLE: f32 = 5.0;

/// Returns true if the curve was changed.
pub fn show(ui: &mut egui::Ui, points: &mut CurvePoints, color: Color32, hist: Option<(&Histogram, usize)>) -> bool {
    let side = ui.available_width().min(300.0);
    let (response, painter) = ui.allocate_painter(Vec2::splat(side), Sense::click_and_drag());
    let rect = response.rect.shrink(HANDLE);
    let mut changed = false;

    let to_screen = |p: [f32; 2]| Pos2::new(rect.left() + p[0] * rect.width(), rect.bottom() - p[1] * rect.height());
    let to_curve = |p: Pos2| {
        [((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0)]
    };

    painter.rect_filled(response.rect, 3.0, Color32::from_gray(14));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let grid = Stroke::new(1.0, Color32::from_gray(34));
        painter.line_segment([to_screen([t, 0.0]), to_screen([t, 1.0])], grid);
        painter.line_segment([to_screen([0.0, t]), to_screen([1.0, t])], grid);
    }
    painter.line_segment([to_screen([0.0, 0.0]), to_screen([1.0, 1.0])], Stroke::new(1.0, Color32::from_gray(48)));

    // Histogram backdrop (log scale) so the curve can be placed against the data.
    if let Some((h, channel)) = hist {
        let peak = (1.0 + h.peak() as f32).ln();
        for i in 0..BINS {
            let count = if channel < 3 {
                h.channels[channel][i]
            } else {
                h.channels.iter().map(|c| c[i]).max().unwrap_or(0)
            };
            let v = ((1.0 + count as f32).ln() / peak).min(1.0) * 0.6;
            let x = i as f32 / (BINS - 1) as f32;
            painter.line_segment([to_screen([x, 0.0]), to_screen([x, v])], Stroke::new(rect.width() / BINS as f32 + 0.5, Color32::from_gray(30)));
        }
    }

    // Keep the stored points tidy (sorted, in range) so indices are stable.
    let tidy = curves::normalized(points);
    if tidy.len() >= 2 && tidy != *points {
        *points = tidy;
    }
    if points.len() < 2 {
        *points = vec![[0.0, 0.0], [1.0, 1.0]];
    }

    // Point interaction.
    let mut remove = None;
    let n = points.len();
    for i in 0..n {
        let center = to_screen(points[i]);
        let hit = Rect::from_center_size(center, Vec2::splat(HANDLE * 3.2));
        let r = ui.interact(hit, response.id.with(i), Sense::click_and_drag());
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos() {
                let mut p = to_curve(pos);
                // Stay between the neighbours so the order never changes.
                let lo = if i > 0 { points[i - 1][0] + 0.005 } else { 0.0 };
                let hi = if i + 1 < n { points[i + 1][0] - 0.005 } else { 1.0 };
                p[0] = p[0].clamp(lo.min(hi), hi.max(lo));
                if points[i] != p {
                    points[i] = p;
                    changed = true;
                }
            }
        if (r.secondary_clicked() || r.double_clicked()) && n > 2 {
            remove = Some(i);
        }
        let hot = r.hovered() || r.dragged();
        painter.circle_filled(center, if hot { HANDLE + 1.5 } else { HANDLE }, if hot { Color32::WHITE } else { color });
        painter.circle_stroke(center, HANDLE, Stroke::new(1.0, Color32::BLACK));
    }
    if let Some(i) = remove {
        points.remove(i);
        changed = true;
    } else if response.clicked() {
        // Click on empty space adds a point there.
        if let Some(pos) = response.interact_pointer_pos() {
            let p = to_curve(pos);
            if points.iter().all(|q| (q[0] - p[0]).abs() > 0.02) {
                points.push(p);
                points.sort_by(|a, b| a[0].total_cmp(&b[0]));
                changed = true;
            }
        }
    }

    // The curve itself.
    let pts = curves::normalized(points);
    let tangents = curves::tangents(&pts);
    let line: Vec<Pos2> = (0..=96)
        .map(|i| {
            let x = i as f32 / 96.0;
            to_screen([x, curves::evaluate(&pts, &tangents, x).clamp(0.0, 1.0)])
        })
        .collect();
    painter.line(line, Stroke::new(1.75, color));
    painter.rect_stroke(response.rect, 3.0, Stroke::new(1.0, Color32::from_gray(50)), StrokeKind::Inside);
    changed
}
