//! Runs the processing stages in order on an image or a region of one.

use super::curves::Lut;
use super::gradient::{self, GradientModel};
use super::ghs::GhsCurve;
use super::params::{ProcessParams, Stretch};
use super::{deconv, detail, geometry, local_contrast, stars, tone};
use crate::image::{Image, luma, median_in_place, median_sigma};
use rayon::prelude::*;

/// Part of the (geometry-applied) image, in fractions of its width/height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

impl Region {
    pub const FULL: Region = Region { u0: 0.0, v0: 0.0, u1: 1.0, v1: 1.0 };
}

/// Image-wide measurements the per-pixel stages need. Computed once from the
/// full image so previews and exports agree.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub gradient: Option<GradientModel>,
    /// Sky background per channel after gradient removal.
    pub background: [f32; 3],
    /// Luminance median and noise after the colour stage (what the stretch
    /// sees).
    pub lum_median: f32,
    pub lum_sigma: f32,
}

#[inline]
fn color_stage(p: &mut [f32; 3], params: &ProcessParams, bg: &[f32; 3]) {
    let wb = params.color.wb;
    if params.color.neutralize_background {
        let target = (bg[0] + bg[1] + bg[2]) / 3.0;
        for c in 0..3 {
            p[c] = (p[c] - bg[c]) * wb[c] + target;
        }
    } else {
        for c in 0..3 {
            p[c] *= wb[c];
        }
    }
}

/// Measure `img` (the geometry-applied stack at any resolution; full
/// resolution gives the most faithful noise estimate).
pub fn analyze(img: &Image, params: &ProcessParams) -> Model {
    let gradient = gradient::fit(img, &params.gradient);
    let (w, h) = (img.width, img.height);
    // Roughly 300k samples regardless of image size.
    let step = (((w * h) as f32 / 300_000.0).sqrt().ceil() as usize).max(1);
    let mut ch: [Vec<f32>; 3] = Default::default();
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let mut p = img.px(x, y);
            if p == [0.0; 3] {
                continue; // uncovered
            }
            if let Some(g) = &gradient {
                let d = g.eval((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                for c in 0..3 {
                    p[c] -= d[c];
                }
            }
            for c in 0..3 {
                ch[c].push(p[c]);
            }
        }
    }
    if ch[0].is_empty() {
        return Model { gradient, background: [0.0; 3], lum_median: 0.0, lum_sigma: 0.0 };
    }
    // The darker half of the image is mostly sky even when the target is
    // large, so use the lower quartile as the background level.
    let background = [0, 1, 2].map(|c| {
        let mut v = ch[c].clone();
        let q = v.len() / 4;
        *v.select_nth_unstable_by(q, |a, b| a.total_cmp(b)).1
    });
    let mut lum: Vec<f32> = (0..ch[0].len())
        .map(|i| {
            let mut p = [ch[0][i], ch[1][i], ch[2][i]];
            color_stage(&mut p, params, &background);
            luma(p[0], p[1], p[2])
        })
        .collect();
    // Noise from the sky only: the spread of the darker half about its top.
    let med = median_in_place(&mut lum);
    let mut below: Vec<f32> = lum.iter().copied().filter(|&l| l <= med).map(|l| med - l).collect();
    let half_sigma = 1.4826 * median_in_place(&mut below);
    let (_, full_sigma) = median_sigma(&mut lum);
    Model { gradient, background, lum_median: med, lum_sigma: half_sigma.min(full_sigma).max(1e-7) }
}

/// A stretch that makes the sky dark grey and the target visible.
pub fn auto_stretch(model: &Model) -> Stretch {
    tone::auto_stretch(model.lum_median, model.lum_sigma, 0.18, 2.8)
}

/// Sky level as the generalised hyperbolic stretch sees it: after the black
/// point and arcsinh of `stretch`. Use it as the GHS symmetry point.
pub fn sky_before_ghs(model: &Model, stretch: &Stretch) -> f32 {
    let s = Stretch { midtone: 0.5, ..stretch.clone() };
    let mut p = [model.lum_median; 3];
    tone::apply_stretch(&s, None, &mut p);
    p[1].clamp(0.0, 1.0)
}

/// An automatic generalised hyperbolic stretch: the black point of the
/// automatic stretch, the symmetry point on the sky, and the strongest
/// stretch that keeps the sky at `target` brightness. The midtones are left
/// linear.
pub fn auto_ghs(model: &Model, target: f32) -> Stretch {
    let mut s = Stretch { black: auto_stretch(model).black, ..Stretch::default() };
    let sky = sky_before_ghs(model, &s);
    s.ghs = super::params::Ghs { local_intensity: 2.0, symmetry: sky, ..Default::default() };
    // Brightness of the sky rises monotonically with the stretch factor.
    let sky_out = |stretch: f32| {
        let g = super::params::Ghs { stretch, ..s.ghs.clone() };
        GhsCurve::new(&g).map_or(sky, |c| c.apply(sky))
    };
    let (mut lo, mut hi) = (0.0f32, 12.0f32);
    if sky_out(hi) <= target {
        lo = hi;
    } else {
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if sky_out(mid) < target { lo = mid } else { hi = mid }
        }
    }
    s.ghs.stretch = lo;
    s
}

/// Process `img`, which shows `region` of the geometry-applied stack at
/// `scale` full-resolution pixels per pixel. Output is display-ready 0..1.
pub fn render(img: &Image, region: Region, scale: f32, params: &ProcessParams, model: &Model) -> Image {
    let (w, h) = (img.width, img.height);
    let mut out = Image::new(w, h);
    let lv = &params.levels;
    let levels_on = [lv.master, lv.r, lv.g, lv.b].iter().any(|l| !tone::level_is_identity(l));
    let chan_levels = [lv.r, lv.g, lv.b];
    let master_lut = Lut::new(&params.curves.master);
    let chan_luts = [Lut::new(&params.curves.r), Lut::new(&params.curves.g), Lut::new(&params.curves.b)];
    let tone_on = !tone::tone_is_identity(&params.tone);
    let (du, dv) = ((region.u1 - region.u0) / w as f32, (region.v1 - region.v0) / h as f32);

    let ghs = GhsCurve::new(&params.stretch.ghs);
    // Uncovered stack pixels stay black throughout.
    let covered: Vec<bool> = img.data.par_chunks_exact(3).map(|p| p != [0.0; 3]).collect();

    // Linear stage: gradient and colour.
    out.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let v = region.v0 + (y as f32 + 0.5) * dv;
        for x in 0..w {
            if !covered[y * w + x] {
                continue;
            }
            let mut p = img.px(x, y);
            if let Some(g) = &model.gradient {
                let d = g.eval(region.u0 + (x as f32 + 0.5) * du, v);
                for c in 0..3 {
                    p[c] -= d[c];
                }
            }
            color_stage(&mut p, params, &model.background);
            row[x * 3..x * 3 + 3].copy_from_slice(&p);
        }
    });

    deconv::apply(&mut out, &covered, &params.deconvolution, scale, model.lum_median, model.lum_sigma);

    // Display stage: stretch and tone.
    out.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            if !covered[y * w + x] {
                continue;
            }
            let mut p = [row[x * 3], row[x * 3 + 1], row[x * 3 + 2]];
            tone::apply_stretch(&params.stretch, ghs.as_ref(), &mut p);
            for c in 0..3 {
                p[c] = p[c].clamp(0.0, 1.0);
            }
            if levels_on {
                for c in 0..3 {
                    p[c] = tone::apply_level(&chan_levels[c], tone::apply_level(&lv.master, p[c]));
                }
            }
            for c in 0..3 {
                if let Some(l) = &master_lut {
                    p[c] = l.apply(p[c]);
                }
                if let Some(l) = &chan_luts[c] {
                    p[c] = l.apply(p[c]);
                }
            }
            if tone_on {
                tone::apply_tone(&params.tone, &mut p);
            }
            for c in 0..3 {
                row[x * 3 + c] = p[c].clamp(0.0, 1.0);
            }
        }
    });
    local_contrast::apply(&mut out, &covered, &params.local_contrast, scale);
    detail::apply(&mut out, &params.detail, scale);
    stars::apply(&mut out, &params.stars, scale);
    out
}

/// Pixels of surrounding context (at `scale`) a region needs for its
/// spatial filters to match a full render.
pub fn context_radius(params: &ProcessParams, scale: f32) -> usize {
    deconv::context(&params.deconvolution, scale).max(local_contrast::context(&params.local_contrast, scale))
}

/// Geometry + analysis + full-resolution render: what export does.
pub fn process_full(stack: &Image, params: &ProcessParams) -> Image {
    let base = geometry::apply(stack, &params.geometry);
    let model = analyze(&base, params);
    render(&base, Region::FULL, 1.0, params, &model)
}

/// Sensible starting point for a fresh stack: camera white balance, neutral
/// background, automatic stretch.
pub fn auto_params(stack: &Image, wb: [f32; 3], crop: Option<[f32; 4]>) -> ProcessParams {
    let mut p = ProcessParams::default();
    if let Some(c) = crop {
        p.geometry.crop = c;
    }
    p.color.wb = wb;
    p.color.neutralize_background = true;
    let base = geometry::apply(stack, &p.geometry);
    p.stretch = auto_stretch(&analyze(&base, &p));
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::params::{Gradient, Level};

    /// Linear "stack": tinted noisy sky with a brighter blob.
    fn stack() -> Image {
        let (w, h) = (240, 160);
        let mut img = Image::new(w, h);
        let mut s = 7u32;
        for y in 0..h {
            for x in 0..w {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = ((s >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.002;
                let d2 = ((x as f32 - 120.0) / 30.0).powi(2) + ((y as f32 - 80.0) / 20.0).powi(2);
                let obj = 0.05 * (-d2).exp();
                let i = (y * w + x) * 3;
                img.data[i] = 0.010 + n + obj;
                img.data[i + 1] = 0.020 + n + obj;
                img.data[i + 2] = 0.015 + n + obj;
            }
        }
        img
    }

    #[test]
    fn default_params_are_the_identity() {
        let img = stack();
        assert_eq!(process_full(&img, &ProcessParams::default()), img);
    }

    #[test]
    fn auto_params_give_a_neutral_dark_sky_and_a_visible_target() {
        let img = stack();
        let p = auto_params(&img, [1.0; 3], None);
        let out = process_full(&img, &p);
        let sky = out.px(10, 10);
        assert!((0.08..0.30).contains(&sky[1]), "sky level {sky:?}");
        assert!((sky[0] - sky[1]).abs() < 0.06 && (sky[2] - sky[1]).abs() < 0.06, "sky tint {sky:?}");
        let core = out.px(120, 80);
        assert!(core[1] > sky[1] + 0.4, "core {core:?} vs sky {sky:?}");
        assert!(out.data.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn region_render_matches_the_full_render() {
        let img = stack();
        let mut p = auto_params(&img, [1.2, 1.0, 1.4], None);
        p.gradient = Gradient { enabled: true, degree: 2, tolerance: 1.0, strength: 1.0 };
        p.levels.master = Level { black: 0.02, gamma: 1.1, white: 0.95 };
        p.curves.master = vec![[0.0, 0.0], [0.3, 0.4], [1.0, 1.0]];
        p.tone.shadows = 0.3;
        p.tone.saturation = 1.3;
        let model = analyze(&img, &p);
        let full = render(&img, Region::FULL, 1.0, &p, &model);
        // Render the middle third on its own.
        let (x0, y0, w, h) = (80, 40, 80, 80);
        let part = img.crop(x0, y0, w, h);
        let region = Region { u0: x0 as f32 / 240.0, v0: y0 as f32 / 160.0, u1: (x0 + w) as f32 / 240.0, v1: (y0 + h) as f32 / 160.0 };
        let got = render(&part, region, 1.0, &p, &model);
        let want = full.crop(x0, y0, w, h);
        let worst = got.data.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst < 1e-5, "max difference {worst}");
    }

    #[test]
    fn padded_region_matches_the_full_render_with_spatial_stages() {
        let img = stack();
        let mut p = auto_params(&img, [1.0; 3], None);
        p.deconvolution = crate::process::params::Deconvolution { amount: 1.0, psf_fwhm: 3.0, ..Default::default() };
        p.local_contrast = crate::process::params::LocalContrast { amount: 0.5, hdr: 0.5, size: 16.0 };
        p.stretch.ghs = crate::process::params::Ghs { stretch: 2.0, local_intensity: 1.0, ..Default::default() };
        let model = analyze(&img, &p);
        let full = render(&img, Region::FULL, 1.0, &p, &model);
        let pad = context_radius(&p, 1.0);
        assert!(pad > 0);
        let (x0, y0, w, h) = (100, 60, 40, 40);
        let (px0, py0) = (x0 - pad.min(x0), y0 - pad.min(y0));
        let (px1, py1) = ((x0 + w + pad).min(240), (y0 + h + pad).min(160));
        let part = img.crop(px0, py0, px1 - px0, py1 - py0);
        let region = Region { u0: px0 as f32 / 240.0, v0: py0 as f32 / 160.0, u1: px1 as f32 / 240.0, v1: py1 as f32 / 160.0 };
        let got = render(&part, region, 1.0, &p, &model).crop(x0 - px0, y0 - py0, w, h);
        let want = full.crop(x0, y0, w, h);
        let worst = got.data.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst < 2e-3, "max difference {worst}");
        assert_ne!(full, process_full(&img, &auto_params(&img, [1.0; 3], None)), "stages had an effect");
    }

    #[test]
    fn auto_ghs_puts_the_sky_at_the_target_and_shows_the_target() {
        let img = stack();
        let mut p = auto_params(&img, [1.0; 3], None);
        p.stretch = auto_ghs(&analyze(&img, &p), 0.15);
        assert!(p.stretch.ghs.stretch > 0.0 && p.stretch.midtone == 0.5);
        let out = process_full(&img, &p);
        // The sky median sits near the target (individual pixels are noisy).
        let mut sky: Vec<f32> = (0..40).flat_map(|y| (0..40).map(move |x| (x, y))).map(|(x, y)| out.px(x, y)[1]).collect();
        let m = crate::image::median_in_place(&mut sky);
        assert!((m - 0.15).abs() < 0.04, "sky {m}");
        assert!(out.px(120, 80)[1] > m + 0.3, "core {:?}", out.px(120, 80));
    }

    #[test]
    fn uncovered_pixels_stay_black() {
        let mut img = stack();
        img.data[..3].fill(0.0);
        let out = process_full(&img, &auto_params(&img, [1.0; 3], None));
        assert_eq!(out.px(0, 0), [0.0; 3]);
    }

    #[test]
    fn crop_changes_output_size() {
        let img = stack();
        let mut p = ProcessParams::default();
        p.geometry.crop = [0.25, 0.25, 0.75, 0.75];
        let out = process_full(&img, &p);
        assert_eq!((out.width, out.height), (120, 80));
    }
}
