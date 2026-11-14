//! Background preview rendering. The UI submits the latest parameters; the
//! worker renders a screen-sized proxy first and then, if asked, the visible
//! region at full resolution. Only the newest request is ever processed.

use astro_processor::histogram::Histogram;
use astro_processor::image::Image;
use astro_processor::io::export::to_rgb8;
use astro_processor::process::params::{Color, Geometry, Gradient};
use astro_processor::process::{self, Model, ProcessParams, Region, geometry};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

/// Longest side of the proxy image, in pixels.
const PROXY_MAX: usize = 1600;
/// Extra border rendered around a detail region so spatial filters have
/// context, in output pixels. Large-scale stages ask for more.
const DETAIL_PAD: usize = 48;
/// Upper limit on the border, so a huge local-contrast size cannot make the
/// detail render many times larger than the screen.
const DETAIL_PAD_MAX: usize = 600;

/// Part of the geometry-applied image to render sharply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailRequest {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    /// Full-resolution pixels per rendered pixel.
    pub scale: usize,
}

#[derive(Clone)]
pub struct PreviewRequest {
    pub stack: Arc<Image>,
    /// Changes whenever `stack` is replaced.
    pub stack_id: u64,
    pub params: ProcessParams,
    pub detail: Option<DetailRequest>,
}

pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

pub enum PreviewOutput {
    Proxy {
        image: RgbImage,
        /// Size of the geometry-applied image the proxy represents.
        base_size: (usize, usize),
        histogram: Histogram,
        model: Model,
    },
    Detail {
        image: RgbImage,
        request: DetailRequest,
    },
}

type Slot = Arc<(Mutex<Option<PreviewRequest>>, Condvar)>;

pub struct PreviewWorker {
    slot: Slot,
    rx: Receiver<PreviewOutput>,
}

impl PreviewWorker {
    pub fn new(ctx: egui::Context) -> Self {
        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        let (tx, rx) = channel();
        let worker_slot = slot.clone();
        std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || run(worker_slot, tx, ctx))
            .expect("spawn preview thread");
        Self { slot, rx }
    }

    /// Replace any pending request with this one.
    pub fn request(&self, req: PreviewRequest) {
        *self.slot.0.lock().unwrap() = Some(req);
        self.slot.1.notify_one();
    }

    pub fn poll(&self) -> Vec<PreviewOutput> {
        self.rx.try_iter().collect()
    }
}

#[derive(Default)]
struct Cache {
    base: Option<(u64, Geometry, Arc<Image>)>,
    proxy: Option<(Image, usize)>,
    model: Option<((u64, Geometry, Gradient, Color), Model)>,
    /// What the last proxy sent to the UI was rendered from.
    sent: Option<(u64, ProcessParams)>,
}

fn run(slot: Slot, tx: Sender<PreviewOutput>, ctx: egui::Context) {
    let mut cache = Cache::default();
    loop {
        let req = {
            let mut guard = slot.0.lock().unwrap();
            loop {
                if let Some(r) = guard.take() {
                    break r;
                }
                guard = slot.1.wait(guard).unwrap();
            }
        };
        let superseded = || slot.0.lock().unwrap().is_some();

        // Geometry-applied base image.
        let base_ok = matches!(&cache.base, Some((id, g, _)) if *id == req.stack_id && *g == req.params.geometry);
        if !base_ok {
            let base = if geometry::is_identity(&req.params.geometry) {
                req.stack.clone()
            } else {
                Arc::new(geometry::apply(&req.stack, &req.params.geometry))
            };
            let factor = base.width.max(base.height).div_ceil(PROXY_MAX).max(1);
            cache.proxy = Some((base.downsample(factor), factor));
            cache.base = Some((req.stack_id, req.params.geometry.clone(), base));
            cache.model = None;
            cache.sent = None;
        }
        let base = cache.base.as_ref().expect("just set").2.clone();
        if superseded() {
            continue;
        }

        let key = (
            req.stack_id,
            req.params.geometry.clone(),
            req.params.gradient.clone(),
            req.params.color.clone(),
        );
        if cache.model.as_ref().map(|(k, _)| k) != Some(&key) {
            cache.model = Some((key, process::analyze(&base, &req.params)));
        }
        let model = cache.model.as_ref().expect("just set").1.clone();

        let proxy_current = matches!(&cache.sent, Some((id, p)) if *id == req.stack_id && *p == req.params);
        if !proxy_current {
            let (proxy, factor) = cache.proxy.as_ref().expect("built with base");
            let out = process::render(proxy, Region::FULL, *factor as f32, &req.params, &model);
            if superseded() {
                continue;
            }
            let histogram = Histogram::of(&out);
            let image = RgbImage { width: out.width, height: out.height, rgb: to_rgb8(&out) };
            cache.sent = Some((req.stack_id, req.params.clone()));
            let msg = PreviewOutput::Proxy { image, base_size: (base.width, base.height), histogram, model: model.clone() };
            if tx.send(msg).is_err() {
                return;
            }
            ctx.request_repaint();
        }

        if let Some(d) = req.detail {
            if superseded() {
                continue;
            }
            if let Some(image) = render_detail(&base, &d, &req.params, &model) {
                if tx.send(PreviewOutput::Detail { image, request: d }).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        }
    }
}

fn render_detail(base: &Image, d: &DetailRequest, params: &ProcessParams, model: &Model) -> Option<RgbImage> {
    let s = d.scale.max(1);
    if d.w == 0 || d.h == 0 || d.x >= base.width || d.y >= base.height {
        return None;
    }
    let pad = process::context_radius(params, s as f32).clamp(DETAIL_PAD, DETAIL_PAD_MAX) * s;
    // Padded source rectangle, clamped to the image and aligned to `s`.
    let x0 = d.x.saturating_sub(pad) / s * s;
    let y0 = d.y.saturating_sub(pad) / s * s;
    let x1 = (d.x + d.w + pad).min(base.width);
    let y1 = (d.y + d.h + pad).min(base.height);
    let src = base.crop(x0, y0, x1 - x0, y1 - y0).downsample(s);
    let region = Region {
        u0: x0 as f32 / base.width as f32,
        v0: y0 as f32 / base.height as f32,
        u1: (x0 + src.width * s) as f32 / base.width as f32,
        v1: (y0 + src.height * s) as f32 / base.height as f32,
    };
    let out = process::render(&src, region, s as f32, params, model);
    // Trim the padding back off.
    let (cx, cy) = ((d.x - x0) / s, (d.y - y0) / s);
    let (cw, ch) = ((d.w / s).min(out.width - cx), (d.h / s).min(out.height - cy));
    if cw == 0 || ch == 0 {
        return None;
    }
    let out = out.crop(cx, cy, cw, ch);
    Some(RgbImage { width: out.width, height: out.height, rgb: to_rgb8(&out) })
}
