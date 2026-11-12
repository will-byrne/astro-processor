//! Resample a frame into the reference frame's pixel grid.

use super::matching::Affine;
use crate::image::Image;
use rayon::prelude::*;
use std::sync::OnceLock;

const LANCZOS_A: usize = 3;
const LUT_RES: usize = 1024;

fn lanczos_lut() -> &'static [f32] {
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=LANCZOS_A * LUT_RES)
            .map(|i| {
                let x = i as f64 / LUT_RES as f64;
                if x == 0.0 {
                    1.0
                } else if x >= LANCZOS_A as f64 {
                    0.0
                } else {
                    let px = std::f64::consts::PI * x;
                    (LANCZOS_A as f64 * px.sin() * (px / LANCZOS_A as f64).sin() / (px * px)) as f32
                }
            })
            .collect()
    })
}

#[inline]
fn weights(frac: f32, lut: &[f32]) -> [f32; 6] {
    let mut w = [0f32; 6];
    let mut sum = 0.0;
    for (k, wk) in w.iter_mut().enumerate() {
        // Tap k sits at offset (k - 2) from the base pixel.
        let d = (k as f32 - 2.0 - frac).abs();
        *wk = lut[((d * LUT_RES as f32) as usize).min(LANCZOS_A * LUT_RES)];
        sum += *wk;
    }
    for wk in &mut w {
        *wk /= sum;
    }
    w
}

/// Resample `src` so that output pixel `(x, y)` takes the value of `src` at
/// `t.apply(x, y)`. Uses Lanczos-3, clamped to the range of the four nearest
/// source pixels so bright stars do not grow dark rings. Output pixels that
/// fall outside the source are NaN.
pub fn warp(src: &Image, t: &Affine, out_w: usize, out_h: usize) -> Image {
    if t.is_identity() && (src.width, src.height) == (out_w, out_h) {
        return src.clone();
    }
    let lut = lanczos_lut();
    let (sw, sh) = (src.width as isize, src.height as isize);
    let mut out = Image::new(out_w, out_h);
    out.data.par_chunks_mut(out_w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..out_w {
            let (sx, sy) = t.apply(x as f64, y as f64);
            let (fx, fy) = (sx.floor(), sy.floor());
            let (ix, iy) = (fx as isize, fy as isize);
            let o = &mut row[x * 3..x * 3 + 3];
            if ix < 0 || iy < 0 || ix + 1 >= sw || iy + 1 >= sh {
                o.fill(f32::NAN);
                continue;
            }
            let (ax, ay) = ((sx - fx) as f32, (sy - fy) as f32);
            let at = |xx: isize, yy: isize| -> &[f32] {
                let i = ((yy * sw + xx) * 3) as usize;
                &src.data[i..i + 3]
            };
            let (p00, p10, p01, p11) = (at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1));
            if ix < 2 || iy < 2 || ix + 3 >= sw || iy + 3 >= sh {
                // Too close to the edge for the full kernel: bilinear.
                for c in 0..3 {
                    o[c] = (p00[c] * (1.0 - ax) + p10[c] * ax) * (1.0 - ay)
                        + (p01[c] * (1.0 - ax) + p11[c] * ax) * ay;
                }
                continue;
            }
            let (wx, wy) = (weights(ax, lut), weights(ay, lut));
            let mut acc = [0f32; 3];
            for (j, &wyj) in wy.iter().enumerate() {
                let base = (((iy - 2 + j as isize) * sw + ix - 2) * 3) as usize;
                let line = &src.data[base..base + 18];
                let mut r = [0f32; 3];
                for (i, &wxi) in wx.iter().enumerate() {
                    r[0] += wxi * line[i * 3];
                    r[1] += wxi * line[i * 3 + 1];
                    r[2] += wxi * line[i * 3 + 2];
                }
                acc[0] += wyj * r[0];
                acc[1] += wyj * r[1];
                acc[2] += wyj * r[2];
            }
            for c in 0..3 {
                let lo = p00[c].min(p10[c]).min(p01[c]).min(p11[c]);
                let hi = p00[c].max(p10[c]).max(p01[c]).max(p11[c]);
                o[c] = acc[c].clamp(lo, hi);
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                img.data[i] = x as f32;
                img.data[i + 1] = y as f32;
                img.data[i + 2] = (x + y) as f32;
            }
        }
        img
    }

    #[test]
    fn identity_is_a_copy() {
        let img = gradient(20, 10);
        assert_eq!(warp(&img, &Affine::IDENTITY, 20, 10), img);
    }

    #[test]
    fn shifts_sample_the_right_place() {
        let img = gradient(40, 30);
        // Whole-pixel shifts are exact.
        let out = warp(&img, &Affine { c: 3.0, f: -2.0, ..Affine::IDENTITY }, 40, 30);
        assert_eq!(out.px(10, 12), [13.0, 10.0, 23.0]);
        // Sub-pixel: source position is (x + 2.25, y - 1.5). Lanczos is not
        // exactly linear-preserving, hence the tolerance.
        let t = Affine { c: 2.25, f: -1.5, ..Affine::IDENTITY };
        let out = warp(&img, &t, 40, 30);
        let p = out.px(10, 12);
        assert!((p[0] - 12.25).abs() < 0.03 && (p[1] - 10.5).abs() < 0.03, "{p:?}");
        // Shifted off the top: no data.
        assert!(out.px(10, 0)[0].is_nan());
        assert!(out.px(39, 12)[0].is_nan());
    }

    #[test]
    fn no_overshoot_next_to_a_bright_pixel() {
        let mut img = Image::new(21, 21);
        let c = (10 * 21 + 10) * 3;
        img.data[c..c + 3].fill(1.0);
        let t = Affine { c: 0.5, f: 0.5, ..Affine::IDENTITY };
        let out = warp(&img, &t, 21, 21);
        assert!(out.data.iter().filter(|v| v.is_finite()).all(|&v| (0.0..=1.0).contains(&v)));
    }
}
