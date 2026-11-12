//! Zoomable, pannable image view backed by a proxy texture, with a sharper
//! "detail" texture overlaid for the visible region when zoomed in.

use super::worker::{DetailRequest, RgbImage};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};

pub struct Viewer {
    proxy: Option<TextureHandle>,
    proxy_size: (usize, usize),
    /// Size of the full (geometry-applied) image.
    pub base_size: (usize, usize),
    detail: Option<(TextureHandle, DetailRequest)>,
    /// Physical screen pixels per image pixel.
    pub zoom: f32,
    /// Image coordinates shown at the centre of the view.
    center: Vec2,
    pub fit: bool,
}

impl Default for Viewer {
    fn default() -> Self {
        Self { proxy: None, proxy_size: (0, 0), base_size: (0, 0), detail: None, zoom: 1.0, center: Vec2::ZERO, fit: true }
    }
}

fn texture(ctx: &egui::Context, name: &str, img: &RgbImage, options: TextureOptions) -> TextureHandle {
    let color = egui::ColorImage::from_rgb([img.width, img.height], &img.rgb);
    ctx.load_texture(name, color, options)
}

impl Viewer {
    pub fn has_image(&self) -> bool {
        self.proxy.is_some()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn set_proxy(&mut self, ctx: &egui::Context, img: &RgbImage, base_size: (usize, usize)) {
        if base_size != self.base_size {
            // Different image shape (new stack, crop, rotation): re-centre.
            self.fit = true;
        }
        self.proxy = Some(texture(ctx, "proxy", img, TextureOptions::LINEAR));
        self.proxy_size = (img.width, img.height);
        self.base_size = base_size;
        // The old detail tile shows the previous settings.
        self.detail = None;
    }

    pub fn set_detail(&mut self, ctx: &egui::Context, img: &RgbImage, request: DetailRequest) {
        if request.x + request.w <= self.base_size.0 && request.y + request.h <= self.base_size.1 {
            self.detail = Some((texture(ctx, "detail", img, TextureOptions::NEAREST), request));
        }
    }

    pub fn zoom_to(&mut self, zoom: f32) {
        self.fit = false;
        self.zoom = zoom;
    }

    /// Draw the view. Returns the detail region to render, if the current
    /// zoom shows more than the proxy can resolve.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<DetailRequest> {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let view = response.rect;
        painter.rect_filled(view, 0.0, Color32::from_gray(10));
        let Some(proxy) = &self.proxy else {
            painter.text(
                view.center(),
                egui::Align2::CENTER_CENTER,
                "Add light frames and press Stack,\nor open a project or an already-stacked image.",
                egui::FontId::proportional(16.0),
                Color32::from_gray(110),
            );
            return None;
        };
        let ppp = ui.ctx().pixels_per_point();
        let (bw, bh) = (self.base_size.0 as f32, self.base_size.1 as f32);
        let fit_zoom = ((view.width() * ppp) / bw).min((view.height() * ppp) / bh).max(1e-4);

        if response.double_clicked() {
            self.fit = !self.fit;
            if !self.fit {
                self.zoom = 1.0;
                if let Some(p) = response.interact_pointer_pos() {
                    self.center = self.to_image(p, view, ppp);
                }
            }
        }
        if self.fit {
            self.zoom = fit_zoom;
            self.center = Vec2::new(bw / 2.0, bh / 2.0);
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.005).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let anchor = response.hover_pos().unwrap_or(view.center());
                let before = self.to_image(anchor, view, ppp);
                self.zoom = (self.zoom * factor).clamp(fit_zoom * 0.5, 16.0);
                self.fit = false;
                // Keep the image point under the cursor fixed.
                let after = self.to_image(anchor, view, ppp);
                self.center += before - after;
            }
        }
        if response.dragged() {
            self.center -= response.drag_delta() * ppp / self.zoom;
            self.fit = false;
        }
        self.center.x = self.center.x.clamp(0.0, bw);
        self.center.y = self.center.y.clamp(0.0, bh);

        let painter = painter.with_clip_rect(view);
        let image_rect = Rect::from_min_max(
            self.to_screen(Vec2::ZERO, view, ppp),
            self.to_screen(Vec2::new(bw, bh), view, ppp),
        );
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(proxy.id(), image_rect, uv, Color32::WHITE);
        if let Some((tex, d)) = &self.detail {
            let r = Rect::from_min_max(
                self.to_screen(Vec2::new(d.x as f32, d.y as f32), view, ppp),
                self.to_screen(Vec2::new((d.x + d.w) as f32, (d.y + d.h) as f32), view, ppp),
            );
            painter.image(tex.id(), r, uv, Color32::WHITE);
        }

        // Ask for a sharper tile when the proxy is being magnified.
        let proxy_factor = bw / self.proxy_size.0.max(1) as f32;
        if self.zoom * proxy_factor <= 1.15 {
            self.detail = None;
            return None;
        }
        let scale = ((1.0 / self.zoom).floor() as usize).max(1);
        let tl = self.to_image(view.min, view, ppp);
        let br = self.to_image(view.max, view, ppp);
        let x0 = (tl.x.max(0.0) as usize) / scale * scale;
        let y0 = (tl.y.max(0.0) as usize) / scale * scale;
        let x1 = (br.x.ceil().max(0.0) as usize).min(self.base_size.0);
        let y1 = (br.y.ceil().max(0.0) as usize).min(self.base_size.1);
        if x1 <= x0 + scale || y1 <= y0 + scale {
            return None;
        }
        Some(DetailRequest {
            x: x0,
            y: y0,
            w: (x1 - x0) / scale * scale,
            h: (y1 - y0) / scale * scale,
            scale,
        })
    }

    fn to_image(&self, p: Pos2, view: Rect, ppp: f32) -> Vec2 {
        self.center + (p - view.center()) * ppp / self.zoom
    }

    fn to_screen(&self, img: Vec2, view: Rect, ppp: f32) -> Pos2 {
        view.center() + (img - self.center) * self.zoom / ppp
    }
}
