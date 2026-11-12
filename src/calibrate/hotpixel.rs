//! Hot-pixel detection and repair on Bayer data. Neighbours are taken two
//! pixels away so they share the pixel's colour filter.

use crate::image::{Mono, median_in_place, median_sigma};
use rayon::prelude::*;

/// The eight same-colour neighbours of (x, y), clamped at the borders by
/// reflecting back into the image.
#[inline]
fn neighbours(m: &Mono, x: usize, y: usize) -> [f32; 8] {
    let (w, h) = (m.width as isize, m.height as isize);
    let (x, y) = (x as isize, y as isize);
    let at = |dx: isize, dy: isize| {
        let mut xx = x + dx;
        let mut yy = y + dy;
        if xx < 0 || xx >= w {
            xx = x - dx;
        }
        if yy < 0 || yy >= h {
            yy = y - dy;
        }
        m.data[(yy.clamp(0, h - 1) * w + xx.clamp(0, w - 1)) as usize]
    };
    [at(-2, -2), at(0, -2), at(2, -2), at(-2, 0), at(2, 0), at(-2, 2), at(0, 2), at(2, 2)]
}

/// Indices of pixels in a master dark that are far above the typical level
/// for their Bayer position.
pub fn find_in_dark(dark: &Mono, sigma: f32) -> Vec<u32> {
    let (w, h) = (dark.width, dark.height);
    let mut thresholds = [0f32; 4];
    for (p, t) in thresholds.iter_mut().enumerate() {
        let (px, py) = (p % 2, p / 2);
        let mut sample: Vec<f32> = (py..h)
            .step_by(6)
            .flat_map(|y| (px..w).step_by(6).map(move |x| (x, y)))
            .map(|(x, y)| dark.data[y * w + x])
            .collect();
        let (med, sd) = median_sigma(&mut sample);
        *t = med + sigma * sd.max(1e-6);
    }
    dark.data
        .par_chunks(w)
        .enumerate()
        .flat_map_iter(|(y, row)| {
            row.iter().enumerate().filter_map(move |(x, &v)| {
                (v > thresholds[(y & 1) * 2 + (x & 1)]).then_some((y * w + x) as u32)
            })
        })
        .collect()
}

/// Replace the listed pixels with the median of their same-colour neighbours.
pub fn repair_listed(m: &mut Mono, hot: &[u32]) {
    let w = m.width;
    let fixes: Vec<(u32, f32)> = hot
        .par_iter()
        .map(|&i| {
            let mut n = neighbours(m, i as usize % w, i as usize / w);
            (i, median_in_place(&mut n))
        })
        .collect();
    for (i, v) in fixes {
        m.data[i as usize] = v;
    }
}

/// Noise estimate from differences between same-colour horizontal neighbours.
pub fn estimate_noise(m: &Mono) -> f32 {
    let (w, h) = (m.width, m.height);
    let mut d: Vec<f32> = (0..h)
        .step_by(5)
        .flat_map(|y| (0..w.saturating_sub(2)).step_by(5).map(move |x| (x, y)))
        .map(|(x, y)| m.data[y * w + x] - m.data[y * w + x + 2])
        .collect();
    let (_, sd) = median_sigma(&mut d);
    sd / std::f32::consts::SQRT_2
}

/// Same-colour samples on a ring six pixels out, used as the local background.
#[inline]
fn ring(m: &Mono, x: usize, y: usize) -> [f32; 8] {
    let (w, h) = (m.width as isize, m.height as isize);
    let (x, y) = (x as isize, y as isize);
    let at = |dx: isize, dy: isize| {
        let xx = if x + dx < 0 || x + dx >= w { x - dx } else { x + dx };
        let yy = if y + dy < 0 || y + dy >= h { y - dy } else { y + dy };
        m.data[(yy.clamp(0, h - 1) * w + xx.clamp(0, w - 1)) as usize]
    };
    [at(-6, 0), at(6, 0), at(0, -6), at(0, 6), at(-4, -4), at(4, -4), at(-4, 4), at(4, 4)]
}

/// Find and repair isolated bright pixels: well above the local background
/// while the nearest same-colour neighbours are not. Stars are several pixels
/// wide, so their neighbours are raised too and they are left alone. Returns
/// the number of pixels repaired.
pub fn repair_outliers(m: &mut Mono, sigma: f32) -> usize {
    let noise = estimate_noise(m);
    if !(noise > 0.0) {
        return 0;
    }
    let (w, h) = (m.width, m.height);
    let thr = sigma * noise;
    let src: &Mono = m;
    let fixes: Vec<(usize, f32)> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            let mut row = Vec::new();
            for x in 0..w {
                let v = src.data[y * w + x];
                // Cheap rejection before the full neighbourhood test.
                if x >= 2 && v - src.data[y * w + x - 2] <= 0.5 * thr {
                    continue;
                }
                let mut far = ring(src, x, y);
                let bg = median_in_place(&mut far);
                let excess = v - bg;
                if excess <= thr {
                    continue;
                }
                let mut near = neighbours(src, x, y);
                let max = near.iter().copied().fold(f32::MIN, f32::max);
                if max - bg < 0.1 * excess + 3.0 * noise {
                    row.push((y * w + x, median_in_place(&mut near)));
                }
            }
            row
        })
        .collect();
    for &(i, v) in &fixes {
        m.data[i] = v;
    }
    fixes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(w: usize, h: usize) -> Mono {
        let mut m = Mono::new(w, h);
        let mut s = 12345u32;
        for v in m.data.iter_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *v = 0.02 + ((s >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.004;
        }
        m
    }

    #[test]
    fn isolated_spike_is_repaired_but_star_is_kept() {
        let mut m = noisy(64, 64);
        m.data[20 * 64 + 20] = 0.9; // hot pixel
        // A star: gaussian blob, sigma 2 px.
        for y in 30..50 {
            for x in 30..50 {
                let r2 = ((x as f32 - 40.0).powi(2) + (y as f32 - 40.0).powi(2)) / (2.0 * 4.0);
                m.data[y * 64 + x] += 0.8 * (-r2).exp();
            }
        }
        let peak = m.data[40 * 64 + 40];
        let n = repair_outliers(&mut m, 6.0);
        assert_eq!(n, 1);
        assert!(m.data[20 * 64 + 20] < 0.03);
        assert_eq!(m.data[40 * 64 + 40], peak);
    }

    #[test]
    fn dark_hot_pixels_are_listed_and_repaired() {
        let mut dark = noisy(64, 64);
        dark.data[5 * 64 + 7] = 0.5;
        dark.data[63 * 64 + 63] = 0.5;
        let hot = find_in_dark(&dark, 10.0);
        assert_eq!(hot, vec![(5 * 64 + 7) as u32, (63 * 64 + 63) as u32]);
        let mut light = noisy(64, 64);
        light.data[5 * 64 + 7] = 1.0;
        light.data[63 * 64 + 63] = 1.0;
        repair_listed(&mut light, &hot);
        assert!(light.data[5 * 64 + 7] < 0.03 && light.data[63 * 64 + 63] < 0.03);
    }
}
