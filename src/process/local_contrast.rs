//! Large-scale local contrast and dynamic-range compression on the stretched
//! luminance, in the spirit of StarTools' Contrast and HDR modules (or
//! CLAHE / wavelet layers in Siril).
//!
//! The luminance is split with the a trous wavelet transform into
//! - fine detail (the first ~4 px), left untouched so noise is not boosted,
//! - mid-scale structure up to `size`, which `amount` boosts,
//! - the large-scale residual, whose bright parts `hdr` compresses, so a
//!   galaxy core or the Trapezium region comes down while the detail riding
//!   on it stays.
//!
//! All of this is measured on a star-free copy of the luminance (a
//! morphological opening), and applied as a brightness ratio, so stars follow
//! their surroundings instead of being boosted into bloated discs.

use super::detail::smooth;
use super::params::LocalContrast;
use super::stars::morph;
use crate::image::{Image, luma};
use rayon::prelude::*;

/// Full-resolution scale of the fine detail that is never boosted.
const FINE: f32 = 4.0;
/// Full-resolution radius of the opening that removes stars from the
/// structure measurement (features narrower than ~2r + 1 px).
const STAR_RADIUS: f32 = 3.0;

fn star_radius(scale: f32) -> usize {
    (STAR_RADIUS / scale.max(1.0)).round() as usize
}

pub fn is_identity(lc: &LocalContrast) -> bool {
    !(lc.amount > 0.0) && !(lc.hdr > 0.0)
}

/// Number of a trous levels to reach `size` at this scale, or `None` if the
/// structures are too small to see.
fn levels(lc: &LocalContrast, scale: f32) -> Option<usize> {
    if is_identity(lc) {
        return None;
    }
    let size = lc.size.clamp(4.0, 1024.0) / scale.max(1.0);
    (size >= 2.0).then(|| (size.log2().round() as usize).clamp(1, 10))
}

/// Pixels of context the stage needs around a region, at `scale`.
pub fn context(lc: &LocalContrast, scale: f32) -> usize {
    // Support of n a trous levels with the 5-tap kernel: 2 * (2^n - 1).
    levels(lc, scale).map_or(0, |n| 2 * ((1usize << n) - 1) + 2 * star_radius(scale) + 4)
}

/// Pull down large-scale brightness above a knee; below it (the sky and faint
/// nebulosity) nothing changes. The slope eases from 1 at the knee down to
/// `1 - 0.8 * hdr` over `WIDTH`, so the curve is smooth and never flat.
#[inline]
fn compress(b: f32, hdr: f32) -> f32 {
    const KNEE: f32 = 0.25;
    const WIDTH: f32 = 0.3;
    // Integral of smoothstep((x - KNEE) / WIDTH) from KNEE to b.
    let ramp = if b <= KNEE {
        0.0
    } else if b < KNEE + WIDTH {
        let u = (b - KNEE) / WIDTH;
        WIDTH * (u * u * u - 0.5 * u * u * u * u)
    } else {
        b - KNEE - 0.5 * WIDTH
    };
    b - 0.8 * hdr * ramp
}

pub fn apply(img: &mut Image, covered: &[bool], lc: &LocalContrast, scale: f32) {
    let Some(n) = levels(lc, scale) else { return };
    let (w, h) = (img.width, img.height);
    if w < 8 || h < 8 {
        return;
    }
    let fine = ((FINE / scale.max(1.0)).log2().round().max(0.0) as usize).min(n);

    let lum: Vec<f32> = img.data.par_chunks_exact(3).map(|p| luma(p[0], p[1], p[2])).collect();
    let r = star_radius(scale);
    let structure = if r > 0 { morph(&morph(&lum, w, h, r, false), w, h, r, true) } else { lum.clone() };
    let mut current = structure.clone();
    let mut fine_smooth: Option<Vec<f32>> = (fine == 0).then(|| structure.clone());
    for level in 0..n {
        current = smooth(&current, w, h, 1 << level);
        if level + 1 == fine {
            fine_smooth = Some(current.clone());
        }
    }
    let base = current;
    let fine_smooth = fine_smooth.expect("fine <= n");

    let gain = 1.5 * lc.amount.clamp(0.0, 1.0);
    let hdr = lc.hdr.clamp(0.0, 1.0);
    img.data.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
        if !covered[i] {
            return;
        }
        let l = structure[i];
        let b = base[i].clamp(0.0, 1.0);
        let mut mid = fine_smooth[i] - base[i];
        // Darkening next to bright features is what makes halos; boost it
        // less than brightening.
        if mid < 0.0 {
            mid *= 0.5;
        }
        // Keep the dark sky calm: the boost fades in over the faintest tones.
        let fade = (l / 0.25).clamp(0.0, 1.0);
        let compressed = compress(b, hdr);
        let nl = (l + (compressed - b) + gain * mid * fade).max(0.0);
        if l > 1e-6 {
            let f = nl / l;
            for v in p.iter_mut() {
                *v = (*v * f).clamp(0.0, 1.0);
            }
        } else if lum[i] <= 1e-6 {
            for v in p.iter_mut() {
                *v = (*v + nl - l).clamp(0.0, 1.0);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dark sky, a broad bright "core" with small faint ripples on it.
    fn scene() -> Image {
        let (w, h) = (256, 192);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let d2 = ((x as f32 - 128.0).powi(2) + (y as f32 - 96.0).powi(2)) / (2.0 * 40f32.powi(2));
                let core = 0.75 * (-d2).exp();
                let ripple = 0.03 * (x as f32 / 6.0).sin() * (-d2).exp();
                let v = 0.1 + core + ripple;
                img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v, v * 0.9, v * 0.8]);
            }
        }
        img
    }

    fn all(img: &Image) -> Vec<bool> {
        vec![true; img.width * img.height]
    }

    #[test]
    fn off_by_default() {
        let img = scene();
        let mut out = img.clone();
        apply(&mut out, &all(&img), &LocalContrast::default(), 1.0);
        assert_eq!(out, img);
    }

    #[test]
    fn hdr_brings_the_core_down_and_keeps_its_detail() {
        let img = scene();
        let mut out = img.clone();
        apply(&mut out, &all(&img), &LocalContrast { hdr: 1.0, size: 32.0, ..LocalContrast::default() }, 1.0);
        assert!(out.px(128, 96)[0] < img.px(128, 96)[0] - 0.1, "core not compressed: {:?} -> {:?}", img.px(128, 96), out.px(128, 96));
        // Sky is essentially unchanged.
        assert!((out.px(5, 5)[0] - img.px(5, 5)[0]).abs() < 0.01);
        // Ripple contrast near the core survives.
        let ripple = |i: &Image| (115..141).map(|x| i.px(x, 96)[0]).fold(0.0, f32::max)
            - (115..141).map(|x| i.px(x, 96)[0]).fold(1.0, f32::min);
        assert!(ripple(&out) > 0.5 * ripple(&img));
        // Hue kept.
        let p = out.px(128, 96);
        assert!((p[1] / p[0] - 0.9).abs() < 1e-3);
    }

    #[test]
    fn amount_boosts_mid_scale_structure() {
        let img = scene();
        let mut out = img.clone();
        apply(&mut out, &all(&img), &LocalContrast { amount: 1.0, size: 32.0, ..LocalContrast::default() }, 1.0);
        let spread = |i: &Image| (100..156).map(|x| i.px(x, 96)[0]).fold(0.0, f32::max)
            - (100..156).map(|x| i.px(x, 96)[0]).fold(1.0, f32::min);
        assert!(spread(&out) > spread(&img) * 1.05, "{} vs {}", spread(&out), spread(&img));
    }

    #[test]
    fn stars_are_not_boosted() {
        // A small star on a smooth gradient of nebulosity.
        let (w, h) = (128, 96);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let neb = 0.3 + 0.2 * (x as f32 / 20.0).sin();
                let d2 = (x as f32 - 64.0).powi(2) + (y as f32 - 48.0).powi(2);
                let v = neb + 0.4 * (-d2 / 3.0).exp();
                img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(v);
            }
        }
        let mut out = img.clone();
        apply(&mut out, &all(&img), &LocalContrast { amount: 1.0, size: 32.0, ..LocalContrast::default() }, 1.0);
        // The star's height above its surroundings changes no more than the
        // surroundings themselves do (relative), i.e. it is not singled out.
        let bump = |i: &Image| i.px(64, 48)[0] / i.px(60, 48)[0];
        assert!(bump(&out) < bump(&img) + 0.02, "{} vs {}", bump(&out), bump(&img));
    }

    #[test]
    fn compression_curve_is_monotone_and_spares_the_sky() {
        for hdr in [0.0, 0.5, 1.0] {
            let mut prev = -1.0;
            for i in 0..=1000 {
                let b = i as f32 / 1000.0;
                let y = compress(b, hdr);
                assert!(y > prev, "hdr={hdr} at {b}");
                prev = y;
                if b <= 0.25 {
                    assert_eq!(y, b);
                }
            }
        }
        assert!(compress(0.9, 1.0) < 0.6);
    }

    #[test]
    fn small_previews_skip_it() {
        let img = scene();
        let mut out = img.clone();
        apply(&mut out, &all(&img), &LocalContrast { amount: 1.0, hdr: 1.0, size: 8.0 }, 16.0);
        assert_eq!(out, img);
    }
}
