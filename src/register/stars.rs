//! Star detection with sub-pixel centroids and size estimates.

use crate::image::{Mono, median_in_place, median_sigma};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Star {
    pub x: f32,
    pub y: f32,
    /// Background-subtracted flux summed over the measurement aperture.
    pub flux: f32,
    pub fwhm: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Detection {
    /// Brightest first, capped at `DetectOptions::max_stars`.
    pub stars: Vec<Star>,
    /// Number of stars found before the cap was applied.
    pub total: usize,
    pub background: f32,
    pub noise: f32,
    /// Median FWHM of the detected stars, in pixels of the analysed image.
    pub fwhm: f32,
}

impl Detection {
    /// Higher is better: many stars, tightly focused.
    pub fn score(&self) -> f32 {
        if self.stars.is_empty() || !(self.fwhm > 0.0) {
            return 0.0;
        }
        self.total as f32 / (self.fwhm * self.fwhm)
    }
}

const TILE: usize = 64;

/// Coarse background and noise maps: per-tile median and robust sigma.
struct Background {
    cols: usize,
    rows: usize,
    level: Vec<f32>,
    noise: f32,
}

impl Background {
    fn measure(img: &Mono) -> Self {
        let cols = img.width.div_ceil(TILE).max(1);
        let rows = img.height.div_ceil(TILE).max(1);
        let stats: Vec<(f32, f32)> = (0..rows * cols)
            .into_par_iter()
            .map(|t| {
                let (tx, ty) = (t % cols, t / cols);
                let mut v = Vec::with_capacity(TILE * TILE / 4);
                for y in (ty * TILE..((ty + 1) * TILE).min(img.height)).step_by(2) {
                    for x in (tx * TILE..((tx + 1) * TILE).min(img.width)).step_by(2) {
                        let p = img.data[y * img.width + x];
                        if p.is_finite() {
                            v.push(p);
                        }
                    }
                }
                median_sigma(&mut v)
            })
            .collect();
        let level = stats.iter().map(|s| s.0).collect();
        let mut sig: Vec<f32> = stats.iter().map(|s| s.1).filter(|s| *s > 0.0).collect();
        let noise = median_in_place(&mut sig);
        Self { cols, rows, level, noise }
    }

    /// Bilinear interpolation between tile centres.
    #[inline]
    fn at(&self, x: usize, y: usize) -> f32 {
        let fx = ((x as f32 + 0.5) / TILE as f32 - 0.5).clamp(0.0, (self.cols - 1) as f32);
        let fy = ((y as f32 + 0.5) / TILE as f32 - 0.5).clamp(0.0, (self.rows - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(self.cols - 1), (y0 + 1).min(self.rows - 1));
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        let g = |x: usize, y: usize| self.level[y * self.cols + x];
        (g(x0, y0) * (1.0 - ax) + g(x1, y0) * ax) * (1.0 - ay)
            + (g(x0, y1) * (1.0 - ax) + g(x1, y1) * ax) * ay
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DetectOptions {
    /// Detection threshold in units of background noise.
    pub sigma: f32,
    pub max_stars: usize,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self { sigma: 6.0, max_stars: 600 }
    }
}

/// Half-size of the window used to measure each star.
const WIN: isize = 7;
/// Local-maximum radius.
const PEAK_R: isize = 3;

pub fn detect(img: &Mono, opts: &DetectOptions) -> Detection {
    let (w, h) = (img.width as isize, img.height as isize);
    if w < 4 * WIN || h < 4 * WIN {
        return Detection::default();
    }
    let bg = Background::measure(img);
    if !(bg.noise > 0.0) {
        return Detection::default();
    }
    let global_bg = {
        let mut l = bg.level.clone();
        median_in_place(&mut l)
    };

    // Background-subtracted, lightly smoothed copy for peak finding.
    let wu = w as usize;
    let mut sub = vec![0f32; img.data.len()];
    sub.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let p = img.data[y * wu + x];
            *v = if p.is_finite() { p - bg.at(x, y) } else { 0.0 };
        }
    });
    let mut smooth = vec![0f32; sub.len()];
    smooth.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
        if y == 0 || y as isize == h - 1 {
            return;
        }
        let (a, b, c) = (&sub[(y - 1) * wu..], &sub[y * wu..], &sub[(y + 1) * wu..]);
        for x in 1..wu - 1 {
            row[x] = (a[x - 1] + 2.0 * a[x] + a[x + 1]
                + 2.0 * b[x - 1]
                + 4.0 * b[x]
                + 2.0 * b[x + 1]
                + c[x - 1]
                + 2.0 * c[x]
                + c[x + 1])
                / 16.0;
        }
    });
    // The 1-2-1 kernel reduces white noise to 0.375 of its original sigma.
    let threshold = opts.sigma * bg.noise * 0.375;

    let mut stars: Vec<Star> = (WIN..h - WIN)
        .into_par_iter()
        .flat_map_iter(|y| {
            let mut found = Vec::new();
            for x in WIN..w - WIN {
                let v = smooth[(y * w + x) as usize];
                if v <= threshold {
                    continue;
                }
                // Strict local maximum (ties broken by scan order).
                let mut is_peak = true;
                'scan: for dy in -PEAK_R..=PEAK_R {
                    for dx in -PEAK_R..=PEAK_R {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let n = smooth[((y + dy) * w + x + dx) as usize];
                        if n > v || (n == v && (dy < 0 || (dy == 0 && dx < 0))) {
                            is_peak = false;
                            break 'scan;
                        }
                    }
                }
                if is_peak
                    && let Some(s) = measure(&sub, w, x, y, bg.noise) {
                        found.push(s);
                    }
            }
            found
        })
        .collect();

    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    // Drop near-duplicates (two peaks on one blob), keeping the brighter.
    let mut kept: Vec<Star> = Vec::with_capacity(stars.len().min(opts.max_stars));
    let mut total = 0;
    for s in stars {
        if kept.len() >= opts.max_stars {
            // Past the cap, just count (a rare duplicate does not matter).
            total += 1;
            continue;
        }
        if !kept.iter().any(|k| (k.x - s.x).abs() < 4.0 && (k.y - s.y).abs() < 4.0) {
            kept.push(s);
            total += 1;
        }
    }
    let mut f: Vec<f32> = kept.iter().map(|s| s.fwhm).collect();
    let fwhm = median_in_place(&mut f);
    Detection { stars: kept, total, background: global_bg, noise: bg.noise, fwhm }
}

/// Intensity-weighted centroid and second moments in a window around a peak.
/// Rejects blobs that are too small (hot pixels), too large (galaxy cores,
/// nebulosity) or too elongated (trails).
fn measure(sub: &[f32], w: isize, px: isize, py: isize, noise: f32) -> Option<Star> {
    // Only pixels clearly above the background contribute, and the cut is
    // tied to the peak so bright and faint stars are measured alike.
    let peak = sub[(py * w + px) as usize];
    let floor = (3.0 * noise).max(0.1 * peak);
    let (mut m0, mut mx, mut my) = (0f64, 0f64, 0f64);
    // Plain aperture sum (no cut): unlike the thresholded moments it does not
    // depend on how sharp the star is, so fluxes compare across frames.
    let mut aperture = 0f64;
    for dy in -WIN..=WIN {
        for dx in -WIN..=WIN {
            if dx * dx + dy * dy > WIN * WIN {
                continue;
            }
            aperture += sub[((py + dy) * w + px + dx) as usize] as f64;
            let v = sub[((py + dy) * w + px + dx) as usize] - floor;
            if v > 0.0 {
                m0 += v as f64;
                mx += v as f64 * dx as f64;
                my += v as f64 * dy as f64;
            }
        }
    }
    if m0 <= 0.0 {
        return None;
    }
    let (cx, cy) = (mx / m0, my / m0);
    if cx.abs() > 2.5 || cy.abs() > 2.5 {
        return None; // the peak is not the centre of its blob
    }
    let (mut sxx, mut syy, mut sxy) = (0f64, 0f64, 0f64);
    let mut npix = 0;
    for dy in -WIN..=WIN {
        for dx in -WIN..=WIN {
            if dx * dx + dy * dy > WIN * WIN {
                continue;
            }
            let v = sub[((py + dy) * w + px + dx) as usize] - floor;
            if v > 0.0 {
                let (ex, ey) = (dx as f64 - cx, dy as f64 - cy);
                sxx += v as f64 * ex * ex;
                syy += v as f64 * ey * ey;
                sxy += v as f64 * ex * ey;
                npix += 1;
            }
        }
    }
    if npix < 4 {
        return None; // a single hot pixel or cosmic-ray hit
    }
    let (sxx, syy, sxy) = (sxx / m0, syy / m0, sxy / m0);
    // Eigenvalues of the covariance give the major/minor widths.
    let tr = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
    let (major, minor) = (tr / 2.0 + disc, (tr / 2.0 - disc).max(1e-6));
    let fwhm = 2.3548 * (tr / 2.0).sqrt();
    if !(0.9..=12.0).contains(&fwhm) || major / minor > 9.0 {
        return None;
    }
    if aperture <= 0.0 {
        return None;
    }
    Some(Star { x: px as f32 + cx as f32, y: py as f32 + cy as f32, flux: aperture as f32, fwhm: fwhm as f32 })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deterministic pseudo-random numbers in [0, 1).
    pub(crate) struct Lcg(pub u64);
    impl Lcg {
        pub fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32) / (1u64 << 24) as f32
        }
        /// Approximately normal (sum of uniforms).
        pub fn gauss(&mut self) -> f32 {
            (0..6).map(|_| self.next()).sum::<f32>() - 3.0
        }
    }

    /// Render gaussian stars `(x, y, amplitude)` on a noisy background.
    pub(crate) fn render(w: usize, h: usize, stars: &[(f32, f32, f32)], sigma: f32, seed: u64) -> Mono {
        let mut rng = Lcg(seed);
        let mut m = Mono::new(w, h);
        for v in m.data.iter_mut() {
            *v = 0.05 + 0.002 * rng.gauss();
        }
        for &(sx, sy, amp) in stars {
            let r = (4.0 * sigma).ceil() as isize;
            for dy in -r..=r {
                for dx in -r..=r {
                    let (x, y) = (sx.round() as isize + dx, sy.round() as isize + dy);
                    if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
                        continue;
                    }
                    let d2 = (x as f32 - sx).powi(2) + (y as f32 - sy).powi(2);
                    m.data[y as usize * w + x as usize] += amp * (-d2 / (2.0 * sigma * sigma)).exp();
                }
            }
        }
        m
    }

    pub(crate) fn random_field(n: usize, w: usize, h: usize, seed: u64) -> Vec<(f32, f32, f32)> {
        let mut rng = Lcg(seed);
        let mut out: Vec<(f32, f32, f32)> = Vec::new();
        while out.len() < n {
            let s = (
                20.0 + rng.next() * (w as f32 - 40.0),
                20.0 + rng.next() * (h as f32 - 40.0),
                0.05 + rng.next() * rng.next() * 0.8,
            );
            if out.iter().all(|o| (o.0 - s.0).abs() > 12.0 || (o.1 - s.1).abs() > 12.0) {
                out.push(s);
            }
        }
        out
    }

    #[test]
    fn finds_stars_with_subpixel_accuracy() {
        let truth = random_field(40, 400, 300, 7);
        let img = render(400, 300, &truth, 1.6, 1);
        let det = detect(&img, &DetectOptions::default());
        assert!(det.stars.len() >= 38 && det.stars.len() <= 41, "found {}", det.stars.len());
        for t in &truth {
            let best = det
                .stars
                .iter()
                .map(|s| ((s.x - t.0).powi(2) + (s.y - t.1).powi(2)).sqrt())
                .fold(f32::MAX, f32::min);
            if t.2 > 0.08 {
                assert!(best < 0.25, "star at ({}, {}) off by {best}", t.0, t.1);
            }
        }
        // Brightest first, and a plausible FWHM (true value 2.355 * 1.6 = 3.77;
        // thresholded moments read a little low).
        assert!(det.stars.windows(2).all(|p| p[0].flux >= p[1].flux));
        assert!((2.5..4.2).contains(&det.fwhm), "fwhm {}", det.fwhm);
        assert!((det.background - 0.05).abs() < 0.002);
    }

    #[test]
    fn ignores_hot_pixels_and_blank_frames() {
        let mut img = render(200, 200, &[], 1.5, 3);
        assert!(detect(&img, &DetectOptions::default()).stars.is_empty());
        img.data[100 * 200 + 100] = 1.0;
        assert!(detect(&img, &DetectOptions::default()).stars.is_empty());
    }

    #[test]
    fn sharper_frames_score_higher() {
        let truth = random_field(30, 300, 300, 9);
        let sharp = detect(&render(300, 300, &truth, 1.4, 1), &DetectOptions::default());
        let soft = detect(&render(300, 300, &truth, 2.6, 1), &DetectOptions::default());
        assert!(sharp.score() > soft.score());
    }
}
