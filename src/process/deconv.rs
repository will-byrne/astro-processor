//! Richardson-Lucy deconvolution of the linear luminance, as in Siril's
//! deconvolution tool or StarTools' Decon: undoes some of the blur that
//! seeing and optics add, using a Gaussian model of the star profile.
//!
//! Plain Richardson-Lucy amplifies noise and puts dark rings around stars,
//! so two safeguards are applied:
//! - an SNR mask, so the sky (where there is only noise to sharpen) is left
//!   alone and the effect fades in over faint signal;
//! - deringing, which keeps every pixel from going darker than the darkest
//!   original pixel nearby, which is where the rings would form.

use super::params::Deconvolution;
use super::stars::morph;
use crate::image::{Image, luma};
use crate::register::stars::{DetectOptions, detect};
use rayon::prelude::*;

/// Below this PSF sigma (in rendered pixels) there is nothing to resolve:
/// zoomed-out previews skip the stage.
const MIN_SIGMA: f32 = 0.4;

pub fn is_identity(d: &Deconvolution) -> bool {
    !(d.amount > 0.0) || !(d.psf_fwhm > 0.0) || d.iterations == 0
}

/// PSF sigma in pixels of an image with `scale` full-resolution pixels per
/// pixel, or `None` if the stage does nothing at that scale.
fn sigma_at(d: &Deconvolution, scale: f32) -> Option<f32> {
    if is_identity(d) {
        return None;
    }
    let sigma = d.psf_fwhm / 2.3548 / scale.max(1.0);
    (sigma >= MIN_SIGMA).then_some(sigma)
}

/// Pixels of context the stage needs around a region, at `scale`.
pub fn context(d: &Deconvolution, scale: f32) -> usize {
    match sigma_at(d, scale) {
        // Each iteration reaches ~3 sigma further, but the influence fades
        // quickly; this is plenty in practice.
        Some(s) => (s * 12.0).ceil() as usize + 4,
        None => 0,
    }
}

/// Median FWHM of the stars in `img` (full-resolution pixels), or `None` if
/// too few stars were found to trust it.
pub fn measure_fwhm(img: &Image) -> Option<f32> {
    let det = detect(&img.luminance(), &DetectOptions { sigma: 8.0, max_stars: 400 });
    (det.stars.len() >= 10 && det.fwhm.is_finite() && det.fwhm > 0.0).then_some(det.fwhm)
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as isize;
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    k
}

#[inline]
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i - 1;
    }
    if i >= n {
        i = 2 * n - 1 - i;
    }
    i.clamp(0, n - 1) as usize
}

/// Separable convolution with a symmetric kernel (so it is also its own
/// adjoint, as Richardson-Lucy needs).
fn convolve(src: &[f32], w: usize, h: usize, k: &[f32]) -> Vec<f32> {
    let r = (k.len() / 2) as isize;
    let mut tmp = vec![0f32; src.len()];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let xi = x as isize;
            let mut acc = 0.0;
            if xi >= r && xi + r < w as isize {
                let base = (xi - r) as usize;
                for (j, kv) in k.iter().enumerate() {
                    acc += kv * s[base + j];
                }
            } else {
                for (j, kv) in k.iter().enumerate() {
                    acc += kv * s[reflect(xi + j as isize - r, w)];
                }
            }
            *o = acc;
        }
    });
    let mut out = vec![0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let rows: Vec<&[f32]> =
            (0..k.len()).map(|j| reflect(y as isize + j as isize - r, h)).map(|yy| &tmp[yy * w..(yy + 1) * w]).collect();
        for (j, kv) in k.iter().enumerate() {
            for (o, v) in row.iter_mut().zip(rows[j]) {
                *o += kv * v;
            }
        }
    });
    out
}

/// Deconvolve linear RGB `img` in place. `covered` marks pixels with data;
/// `bg` and `noise` are the luminance sky level and noise (at full
/// resolution) from the analysis.
pub fn apply(img: &mut Image, covered: &[bool], d: &Deconvolution, scale: f32, bg: f32, noise: f32) {
    let Some(sigma) = sigma_at(d, scale) else { return };
    let (w, h) = (img.width, img.height);
    if w < 8 || h < 8 {
        return;
    }
    let kernel = gaussian_kernel(sigma);
    const FLOOR: f32 = 1e-6;

    let lum: Vec<f32> = img.data.par_chunks_exact(3).map(|p| luma(p[0], p[1], p[2])).collect();
    // Richardson-Lucy needs positive data.
    let observed: Vec<f32> = lum.par_iter().map(|l| l.max(FLOOR)).collect();

    // Deringing floor: the darkest original value within ~2 FWHM.
    let ring_r = ((2.0 * 2.3548 * sigma).ceil() as usize).max(1);
    let floor: Option<Vec<f32>> = (d.deringing > 0.0).then(|| {
        let local_min = morph(&observed, w, h, ring_r, false);
        let k = d.deringing.clamp(0.0, 1.0);
        // Partial deringing lets the floor sit a little lower.
        local_min.par_iter().map(|m| m * k).collect()
    });

    let mut est = observed.clone();
    for _ in 0..d.iterations.min(200) {
        let blurred = convolve(&est, w, h, &kernel);
        let ratio: Vec<f32> = observed.par_iter().zip(&blurred).map(|(o, b)| o / b.max(FLOOR)).collect();
        let correction = convolve(&ratio, w, h, &kernel);
        est.par_iter_mut().zip(&correction).enumerate().for_each(|(i, (e, c))| {
            // Limit the per-iteration change so one noisy pixel cannot run
            // away.
            *e = (*e * c.clamp(0.5, 2.0)).max(FLOOR);
            if let Some(f) = &floor {
                *e = e.max(f[i]);
            }
        });
    }

    let amount = d.amount.clamp(0.0, 1.0);
    let noise = noise.max(1e-7);
    let k = d.noise_protection.max(0.0);
    img.data.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
        if !covered[i] {
            return;
        }
        // SNR mask: off at the sky, full strength k sigma above it (and
        // ramping in over the next k sigma).
        let snr = (lum[i] - bg) / noise;
        let t = if k > 0.0 { ((snr - k) / k).clamp(0.0, 1.0) } else { 1.0 };
        let m = t * t * (3.0 - 2.0 * t) * amount;
        if m <= 0.0 {
            return;
        }
        let dl = (est[i] - observed[i]) * m;
        let l = lum[i];
        if l > FLOOR {
            // Scale the channels together so star colour is kept.
            let f = (l + dl) / l;
            for v in p.iter_mut() {
                *v *= f;
            }
        } else {
            for v in p.iter_mut() {
                *v += dl;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sky at 0.01 with noise, Gaussian stars blurred to `fwhm`.
    fn field(fwhm: f32, noise: f32) -> Image {
        let (w, h) = (160, 120);
        let mut img = Image::new(w, h);
        let sigma = fwhm / 2.3548;
        let stars = [(40.0, 40.0, 0.3), (110.0, 70.0, 0.15), (80.0, 95.0, 0.5)];
        let mut s = 5u32;
        for y in 0..h {
            for x in 0..w {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = ((s >> 8) as f32 / (1 << 24) as f32 - 0.5) * noise;
                let mut v = 0.01 + n;
                for (sx, sy, a) in stars {
                    let d2 = (x as f32 + 0.5 - sx).powi(2) + (y as f32 + 0.5 - sy).powi(2);
                    v += a * (-d2 / (2.0 * sigma * sigma)).exp();
                }
                img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v * 1.1, v, v * 0.9]);
            }
        }
        img
    }

    fn params(fwhm: f32) -> Deconvolution {
        Deconvolution { amount: 1.0, psf_fwhm: fwhm, ..Deconvolution::default() }
    }

    fn half_max_width(img: &Image, y: usize, x0: usize, x1: usize) -> usize {
        let row: Vec<f32> = (x0..x1).map(|x| img.px(x, y)[1] - 0.01).collect();
        let peak = row.iter().copied().fold(0.0, f32::max);
        row.iter().filter(|v| **v > peak / 2.0).count()
    }

    #[test]
    fn sharpens_stars_without_rings_or_sky_noise() {
        let img = field(5.0, 0.0004);
        let mut out = img.clone();
        let covered = vec![true; img.width * img.height];
        apply(&mut out, &covered, &params(5.0), 1.0, 0.01, 0.0004 / 12f32.sqrt());
        let (before, after) = (half_max_width(&img, 40, 20, 60), half_max_width(&out, 40, 20, 60));
        assert!(after + 1 < before, "FWHM {before} -> {after}");
        // Star got brighter (flux concentrated), colour ratio kept.
        let (a, b) = (img.px(40, 40), out.px(40, 40));
        assert!(b[1] > a[1] * 1.3, "{a:?} -> {b:?}");
        assert!((b[0] / b[1] - 1.1).abs() < 1e-3);
        // No dark ring: nothing near the star drops below the sky's floor.
        for x in 25..55 {
            assert!(out.px(x, 40)[1] > 0.0085, "ring at x={x}: {}", out.px(x, 40)[1]);
        }
        // Empty sky is untouched by the SNR mask.
        assert_eq!(out.px(5, 5), img.px(5, 5));
    }

    #[test]
    fn off_and_unresolvable_scales_do_nothing() {
        let img = field(5.0, 0.0004);
        let covered = vec![true; img.width * img.height];
        let mut out = img.clone();
        apply(&mut out, &covered, &Deconvolution::default(), 1.0, 0.01, 0.0001);
        assert_eq!(out, img);
        apply(&mut out, &covered, &params(5.0), 8.0, 0.01, 0.0001);
        assert_eq!(out, img);
    }

    #[test]
    fn convolution_preserves_flux() {
        let (w, h) = (40, 30);
        let mut v = vec![0f32; w * h];
        v[15 * w + 20] = 1.0;
        let out = convolve(&v, w, h, &gaussian_kernel(2.0));
        assert!((out.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        assert!(out[15 * w + 20] > out[15 * w + 22] && out[15 * w + 22] > out[15 * w + 25]);
    }
}
