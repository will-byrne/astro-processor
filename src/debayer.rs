//! Bayer mosaic -> RGB.

use crate::image::{CfaPattern, Image, Mono, cfa_color};
use rayon::prelude::*;

/// Bilinear demosaic: each missing colour is the mean of the samples of that
/// colour in the surrounding 3x3 block.
pub fn bilinear(cfa: &Mono, pattern: &CfaPattern) -> Image {
    let (w, h) = (cfa.width, cfa.height);
    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        for x in 0..w {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let mut sum = [0f32; 3];
            let mut n = [0f32; 3];
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    let c = cfa_color(pattern, xx, yy);
                    sum[c] += cfa.data[yy * w + xx];
                    n[c] += 1.0;
                }
            }
            let own = cfa_color(pattern, x, y);
            for c in 0..3 {
                row[x * 3 + c] = if c == own {
                    cfa.data[y * w + x]
                } else if n[c] > 0.0 {
                    sum[c] / n[c]
                } else {
                    0.0
                };
            }
        }
    });
    out
}

/// Half-resolution luminance: the mean of each 2x2 Bayer cell. Cheap and
/// alias-free, used for star detection.
pub fn bin2(cfa: &Mono) -> Mono {
    let (w, h) = (cfa.width / 2, cfa.height / 2);
    let mut out = Mono::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let a = &cfa.data[2 * y * cfa.width..];
        let b = &cfa.data[(2 * y + 1) * cfa.width..];
        for x in 0..w {
            row[x] = 0.25 * (a[2 * x] + a[2 * x + 1] + b[2 * x] + b[2 * x + 1]);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_colour_is_reconstructed_exactly() {
        let pattern: CfaPattern = [0, 1, 1, 2];
        let rgb = [0.2f32, 0.5, 0.8];
        let (w, h) = (16, 12);
        let mut cfa = Mono::new(w, h);
        for y in 0..h {
            for x in 0..w {
                cfa.data[y * w + x] = rgb[cfa_color(&pattern, x, y)];
            }
        }
        let img = bilinear(&cfa, &pattern);
        for p in img.data.chunks_exact(3) {
            for c in 0..3 {
                assert!((p[c] - rgb[c]).abs() < 1e-6);
            }
        }
        let l = bin2(&cfa);
        assert_eq!((l.width, l.height), (8, 6));
        assert!(l.data.iter().all(|v| (v - 0.5).abs() < 1e-6));
    }
}
