//! Wavelet (a trous) noise reduction and sharpening on luminance, plus
//! chroma smoothing.

use super::params::Detail;
use crate::image::{Image, luma, median_in_place};
use rayon::prelude::*;

pub fn is_identity(d: &Detail) -> bool {
    d.denoise_luma <= 0.0 && d.denoise_chroma <= 0.0 && d.sharpen <= 0.0
}

/// One a trous smoothing step with the B3-spline kernel, holes of `step`.
pub(crate) fn smooth(src: &[f32], w: usize, h: usize, step: usize) -> Vec<f32> {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let reflect = |i: isize, n: usize| -> usize {
        let n = n as isize;
        let mut i = i;
        if i < 0 {
            i = -i;
        }
        if i >= n {
            i = 2 * n - 2 - i;
        }
        i.clamp(0, n - 1) as usize
    };
    let mut tmp = vec![0f32; src.len()];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (k, kv) in K.iter().enumerate() {
                acc += kv * s[reflect(x as isize + (k as isize - 2) * step as isize, w)];
            }
            *o = acc;
        }
    });
    let mut out = vec![0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let ys: [usize; 5] =
            std::array::from_fn(|k| reflect(y as isize + (k as isize - 2) * step as isize, h));
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (k, kv) in K.iter().enumerate() {
                acc += kv * tmp[ys[k] * w + x];
            }
            *o = acc;
        }
    });
    out
}

/// Noise threshold multipliers and sharpening gains per full-resolution
/// wavelet layer (layer 0 = 1 px detail, 1 = 2 px, ...).
const DENOISE: [f32; 4] = [3.0, 2.0, 1.0, 0.5];
const SHARPEN: [f32; 4] = [0.8, 0.6, 0.25, 0.0];

/// Apply detail processing in place. `scale` is how many full-resolution
/// pixels one pixel of `img` spans (1 at full size, more for previews): fine
/// layers that the preview cannot resolve are skipped, so a zoomed-out
/// preview approximates the full-size result.
pub fn apply(img: &mut Image, d: &Detail, scale: f32) {
    if is_identity(d) {
        return;
    }
    let (w, h) = (img.width, img.height);
    if w < 8 || h < 8 {
        return;
    }
    let shift = scale.max(1.0).log2().round() as usize;

    let lum: Vec<f32> = img.data.par_chunks_exact(3).map(|p| luma(p[0], p[1], p[2])).collect();
    let mut new_lum = lum.clone();

    if (d.denoise_luma > 0.0 || d.sharpen > 0.0) && shift < DENOISE.len() {
        let mut current = lum.clone();
        let mut detail_sum = vec![0f32; lum.len()];
        for (view_layer, full_layer) in (shift..DENOISE.len()).enumerate() {
            let next = smooth(&current, w, h, 1 << view_layer);
            // Robust noise estimate for this layer.
            let mut sample: Vec<f32> =
                current.iter().zip(&next).step_by(7).map(|(c, n)| (c - n).abs()).collect();
            let sigma = 1.4826 * median_in_place(&mut sample);
            let thr = DENOISE[full_layer] * d.denoise_luma.clamp(0.0, 1.0) * sigma;
            let gain = 1.0 + SHARPEN[full_layer] * d.sharpen.clamp(0.0, 2.0);
            (detail_sum.par_iter_mut(), current.par_iter(), next.par_iter()).into_par_iter().for_each(
                |(acc, c, n)| {
                    let wv = c - n;
                    // Soft threshold, then boost what survives. Coefficients
                    // within the noise are never amplified.
                    let mag = (wv.abs() - thr).max(0.0);
                    let t = ((wv.abs() - sigma) / (2.0 * sigma).max(1e-12)).clamp(0.0, 1.0);
                    let confidence = t * t * (3.0 - 2.0 * t);
                    *acc += (mag * (1.0 + (gain - 1.0) * confidence)).copysign(wv);
                },
            );
            current = next;
        }
        (new_lum.par_iter_mut(), detail_sum.par_iter(), current.par_iter())
            .into_par_iter()
            .for_each(|(o, dsum, residual)| *o = residual + dsum);
    }

    // Chroma: blend the colour-difference planes toward a smoothed copy.
    let chroma_amount = d.denoise_chroma.clamp(0.0, 1.0);
    let chroma_layers = 3usize.saturating_sub(shift);
    let (cr, cb) = if chroma_amount > 0.0 && chroma_layers > 0 {
        let blur = |plane: Vec<f32>| {
            let mut s = plane.clone();
            for l in 0..chroma_layers {
                s = smooth(&s, w, h, 1 << l);
            }
            (plane, s)
        };
        let r: Vec<f32> = img.data.par_chunks_exact(3).zip(&lum).map(|(p, l)| p[0] - l).collect();
        let b: Vec<f32> = img.data.par_chunks_exact(3).zip(&lum).map(|(p, l)| p[2] - l).collect();
        (Some(blur(r)), Some(blur(b)))
    } else {
        (None, None)
    };

    img.data.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
        let (l0, l1) = (lum[i], new_lum[i]);
        match (&cr, &cb) {
            (Some((r, rs)), Some((b, bs))) => {
                let dr = r[i] + (rs[i] - r[i]) * chroma_amount;
                let db = b[i] + (bs[i] - b[i]) * chroma_amount;
                let (nr, nb) = (l1 + dr, l1 + db);
                p[0] = nr;
                p[2] = nb;
                p[1] = (l1 - 0.2126 * nr - 0.0722 * nb) / 0.7152;
            }
            _ => {
                let delta = l1 - l0;
                p[0] += delta;
                p[1] += delta;
                p[2] += delta;
            }
        }
        for v in p.iter_mut() {
            *v = v.clamp(0.0, 1.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::median_sigma;

    fn noisy_with_edge(w: usize, h: usize, chroma_noise: bool) -> Image {
        let mut img = Image::new(w, h);
        let mut s = 99u32;
        let mut rnd = move || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            (s >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        for y in 0..h {
            for x in 0..w {
                let base = if x < w / 2 { 0.3 } else { 0.6 };
                let n = rnd() * 0.06;
                let i = (y * w + x) * 3;
                if chroma_noise {
                    img.data[i] = base + rnd() * 0.06;
                    img.data[i + 1] = base;
                    img.data[i + 2] = base + rnd() * 0.06;
                } else {
                    img.data[i..i + 3].fill(base + n);
                }
            }
        }
        img
    }

    fn flat_sigma(img: &Image, ch: usize) -> f32 {
        let mut v: Vec<f32> = (10..img.height - 10)
            .flat_map(|y| (10..img.width / 2 - 10).map(move |x| (x, y)))
            .map(|(x, y)| img.px(x, y)[ch])
            .collect();
        median_sigma(&mut v).1
    }

    #[test]
    fn defaults_do_nothing() {
        let img = noisy_with_edge(64, 48, false);
        let mut out = img.clone();
        apply(&mut out, &Detail::default(), 1.0);
        assert_eq!(out, img);
    }

    #[test]
    fn luma_denoise_cuts_noise_and_keeps_the_edge() {
        let img = noisy_with_edge(128, 96, false);
        let mut out = img.clone();
        apply(&mut out, &Detail { denoise_luma: 1.0, ..Detail::default() }, 1.0);
        assert!(flat_sigma(&out, 1) < 0.4 * flat_sigma(&img, 1));
        let step = out.px(96, 48)[1] - out.px(32, 48)[1];
        assert!((step - 0.3).abs() < 0.02, "edge height {step}");
        // The transition is still sharp: fully switched within 3 px.
        assert!(out.px(60, 48)[1] < 0.36 && out.px(67, 48)[1] > 0.54);
    }

    #[test]
    fn chroma_denoise_only_touches_colour() {
        let img = noisy_with_edge(128, 96, true);
        let mut out = img.clone();
        apply(&mut out, &Detail { denoise_chroma: 1.0, ..Detail::default() }, 1.0);
        assert!(flat_sigma(&out, 0) < 0.4 * flat_sigma(&img, 0));
        let l_before = img.luminance();
        let l_after = out.luminance();
        let max_dl = l_before.data.iter().zip(&l_after.data).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(max_dl < 1e-4, "luminance changed by {max_dl}");
    }

    #[test]
    fn sharpening_steepens_edges_without_amplifying_flat_noise_much() {
        // A soft edge (blurred step), no noise.
        let (w, h) = (64, 32);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 0.3 + 0.3 / (1.0 + (-(x as f32 - 32.0) / 2.0).exp());
                img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(v);
            }
        }
        let mut out = img.clone();
        apply(&mut out, &Detail { sharpen: 1.5, ..Detail::default() }, 1.0);
        let slope = |i: &Image| i.px(33, 16)[1] - i.px(31, 16)[1];
        assert!(slope(&out) > 1.15 * slope(&img));
        // Far from the edge nothing changes.
        assert!((out.px(5, 16)[1] - img.px(5, 16)[1]).abs() < 1e-4);
    }

    #[test]
    fn coarse_previews_skip_unresolvable_layers() {
        let img = noisy_with_edge(64, 48, false);
        let mut out = img.clone();
        apply(&mut out, &Detail { denoise_luma: 1.0, sharpen: 1.0, ..Detail::default() }, 16.0);
        assert_eq!(out, img);
    }
}
