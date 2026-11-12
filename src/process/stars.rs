//! Star reduction: shrink small bright features while leaving extended
//! objects alone.

use super::params::Stars;
use crate::image::{Image, luma};
use rayon::prelude::*;

pub fn is_identity(s: &Stars) -> bool {
    s.reduce <= 0.0 || s.radius <= 0.0
}

/// Separable square min (or max) filter of the given radius on one plane.
fn morph(src: &[f32], w: usize, h: usize, radius: usize, take_max: bool) -> Vec<f32> {
    let pick = |a: f32, b: f32| if take_max { a.max(b) } else { a.min(b) };
    let mut tmp = vec![0f32; src.len()];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let (a, b) = (x.saturating_sub(radius), (x + radius).min(w - 1));
            *o = s[a..=b].iter().copied().reduce(pick).unwrap_or(s[x]);
        }
    });
    let mut out = vec![0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(radius), (y + radius).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = tmp[a * w + x];
            for yy in a + 1..=b {
                acc = pick(acc, tmp[yy * w + x]);
            }
            *o = acc;
        }
    });
    out
}

fn box3(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            let mut n = 0.0;
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    acc += src[yy * w + xx];
                    n += 1.0;
                }
            }
            *o = acc / n;
        }
    });
    out
}

/// Shrink stars in place. `scale` is full-resolution pixels per image pixel;
/// when stars are too small to resolve at this scale the effect fades out.
pub fn apply(img: &mut Image, s: &Stars, scale: f32) {
    if is_identity(s) {
        return;
    }
    let (w, h) = (img.width, img.height);
    let r = s.radius / scale.max(1.0);
    if r < 0.3 || w < 8 || h < 8 {
        return;
    }
    let radius = (r.round() as usize).max(1);
    // A fractional radius is approximated by a weaker blend.
    let amount = s.reduce.clamp(0.0, 1.0) * (r / radius as f32).min(1.0);

    // Star mask: bright features that vanish under a morphological opening
    // wider than any star (white top-hat of the luminance).
    let lum: Vec<f32> = img.data.par_chunks_exact(3).map(|p| luma(p[0], p[1], p[2])).collect();
    let open_r = (((4.0 * s.radius + 4.0) / scale.max(1.0)).round() as usize).max(radius + 1);
    let opened = morph(&morph(&lum, w, h, open_r, false), w, h, open_r, true);
    let tophat: Vec<f32> = lum
        .par_iter()
        .zip(&opened)
        .map(|(l, o)| {
            let t = ((l - o - 0.02) / 0.10).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        })
        .collect();
    // Grow the mask slightly so star edges blend smoothly.
    let mask = box3(&morph(&tophat, w, h, 1, true), w, h);

    for c in 0..3 {
        let plane: Vec<f32> = img.data.par_chunks_exact(3).map(|p| p[c]).collect();
        let eroded = box3(&morph(&plane, w, h, radius, false), w, h);
        img.data.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
            let target = eroded[i].min(p[c]);
            p[c] += (target - p[c]) * mask[i] * amount;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dark sky, one star (sigma 2 px) and one broad "galaxy" (sigma 30 px).
    fn scene() -> Image {
        let (w, h) = (200, 120);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let star = 0.8 * (-((x as f32 - 40.0).powi(2) + (y as f32 - 60.0).powi(2)) / 8.0).exp();
                let gal = 0.5 * (-((x as f32 - 140.0).powi(2) + (y as f32 - 60.0).powi(2)) / 1800.0).exp();
                img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(0.1 + star + gal);
            }
        }
        img
    }

    fn width_at(img: &Image, y: usize, x0: usize, x1: usize, level: f32) -> usize {
        (x0..x1).filter(|&x| img.px(x, y)[1] > level).count()
    }

    #[test]
    fn stars_shrink_and_galaxies_do_not() {
        let img = scene();
        let mut out = img.clone();
        apply(&mut out, &Stars { reduce: 1.0, radius: 1.5 }, 1.0);
        let before = width_at(&img, 60, 20, 60, 0.3);
        let after = width_at(&out, 60, 20, 60, 0.3);
        assert!(after + 2 <= before, "star width {before} -> {after}");
        assert!(after >= 1, "star must not vanish");
        // Galaxy profile is essentially untouched.
        for x in [110, 125, 140, 155, 170] {
            assert!((out.px(x, 60)[1] - img.px(x, 60)[1]).abs() < 0.01, "galaxy changed at {x}");
        }
        // Nothing gets brighter.
        assert!(out.data.iter().zip(&img.data).all(|(a, b)| a <= b));
    }

    #[test]
    fn off_by_default_and_fades_when_zoomed_out() {
        let img = scene();
        let mut out = img.clone();
        apply(&mut out, &Stars::default(), 1.0);
        assert_eq!(out, img);
        apply(&mut out, &Stars { reduce: 1.0, radius: 1.5 }, 8.0);
        assert_eq!(out, img);
    }
}
