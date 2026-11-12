//! Background gradient modelling: fit a low-order polynomial surface to
//! sky-background samples, rejecting samples that sit on the target or stars.

use super::params::Gradient;
use crate::image::{Image, median_in_place, median_sigma};
use nalgebra::{DMatrix, DVector};

/// Polynomial in normalised image coordinates (u, v in 0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    degree: usize,
    coeffs: Vec<f64>,
}

fn basis(degree: usize, u: f64, v: f64, out: &mut Vec<f64>) {
    out.clear();
    let (x, y) = (2.0 * u - 1.0, 2.0 * v - 1.0);
    for total in 0..=degree {
        for j in 0..=total {
            out.push(x.powi((total - j) as i32) * y.powi(j as i32));
        }
    }
}

impl Surface {
    #[inline]
    pub fn eval(&self, u: f32, v: f32) -> f32 {
        // Horner would be faster, but degree is at most 4 (15 terms).
        let (x, y) = (2.0 * u as f64 - 1.0, 2.0 * v as f64 - 1.0);
        let mut k = 0;
        let mut acc = 0.0;
        let mut xp = [1.0f64; 5];
        let mut yp = [1.0f64; 5];
        for i in 1..=self.degree {
            xp[i] = xp[i - 1] * x;
            yp[i] = yp[i - 1] * y;
        }
        for total in 0..=self.degree {
            for j in 0..=total {
                acc += self.coeffs[k] * xp[total - j] * yp[j];
                k += 1;
            }
        }
        acc as f32
    }

    fn fit(degree: usize, samples: &[(f64, f64, f64)]) -> Option<Surface> {
        let terms = (degree + 1) * (degree + 2) / 2;
        if samples.len() < terms + 2 {
            return None;
        }
        let mut a = DMatrix::<f64>::zeros(samples.len(), terms);
        let mut b = DVector::<f64>::zeros(samples.len());
        let mut row = Vec::with_capacity(terms);
        for (i, &(u, v, val)) in samples.iter().enumerate() {
            basis(degree, u, v, &mut row);
            for (j, &t) in row.iter().enumerate() {
                a[(i, j)] = t;
            }
            b[i] = val;
        }
        let x = a.svd(true, true).solve(&b, 1e-10).ok()?;
        Some(Surface { degree, coeffs: x.iter().copied().collect() })
    }
}

/// Per-channel background model. `eval` returns what to subtract from each
/// channel at a position: the modelled surface minus its typical level, so the
/// overall sky brightness is unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct GradientModel {
    surfaces: [Surface; 3],
    pedestal: [f32; 3],
    strength: f32,
}

impl GradientModel {
    #[inline]
    pub fn eval(&self, u: f32, v: f32) -> [f32; 3] {
        [0, 1, 2].map(|c| (self.surfaces[c].eval(u, v) - self.pedestal[c]) * self.strength)
    }
}

const GRID_X: usize = 24;
const GRID_Y: usize = 16;

struct Sample {
    u: f64,
    v: f64,
    rgb: [f64; 3],
}

/// Median colour of each grid box, skipping boxes with missing data.
fn sample_grid(img: &Image) -> Vec<Sample> {
    let (bw, bh) = ((img.width / GRID_X).max(1), (img.height / GRID_Y).max(1));
    let mut out = Vec::new();
    for gy in 0..GRID_Y {
        for gx in 0..GRID_X {
            let (x0, y0) = (gx * bw, gy * bh);
            if x0 + bw > img.width || y0 + bh > img.height {
                continue;
            }
            let mut ch: [Vec<f32>; 3] = Default::default();
            let mut empty = 0;
            // At most ~24 samples per axis per box keeps this fast on
            // full-resolution images.
            let (sx, sy) = ((bw / 24).max(1), (bh / 24).max(1));
            let mut total = 0;
            for y in (y0..y0 + bh).step_by(sy) {
                for x in (x0..x0 + bw).step_by(sx) {
                    total += 1;
                    let p = img.px(x, y);
                    if p == [0.0; 3] {
                        empty += 1; // uncovered stack edge
                    } else {
                        for c in 0..3 {
                            ch[c].push(p[c]);
                        }
                    }
                }
            }
            if empty * 10 > total || ch[0].is_empty() {
                continue;
            }
            out.push(Sample {
                u: (x0 as f64 + bw as f64 / 2.0) / img.width as f64,
                v: (y0 as f64 + bh as f64 / 2.0) / img.height as f64,
                rgb: ch.map(|mut c| median_in_place(&mut c) as f64),
            });
        }
    }
    out
}

/// Fit the model on a (small) copy of the image. Returns `None` if disabled
/// or there is not enough background to fit.
pub fn fit(img: &Image, params: &Gradient) -> Option<GradientModel> {
    if !params.enabled || params.strength == 0.0 {
        return None;
    }
    let degree = params.degree.clamp(1, 4) as usize;
    let samples = sample_grid(img);
    let mut keep = vec![true; samples.len()];
    let lum = |s: &Sample| (s.rgb[0] + s.rgb[1] + s.rgb[2]) / 3.0;

    // Iteratively fit the luminance and drop samples well above the model
    // (galaxy, nebula, bright stars) or far below it.
    for _ in 0..12 {
        let pts: Vec<(f64, f64, f64)> =
            samples.iter().zip(&keep).filter(|(_, k)| **k).map(|(s, _)| (s.u, s.v, lum(s))).collect();
        let surface = Surface::fit(degree, &pts)?;
        let resid: Vec<f32> =
            samples.iter().map(|s| (lum(s) - surface.eval(s.u as f32, s.v as f32) as f64) as f32).collect();
        let mut kept: Vec<f32> = resid.iter().zip(&keep).filter(|(_, k)| **k).map(|(r, _)| *r).collect();
        let (med, sigma) = median_sigma(&mut kept);
        let sigma = sigma.max(1e-9);
        let mut changed = false;
        for (k, r) in keep.iter_mut().zip(&resid) {
            let ok = (*r - med) <= params.tolerance.max(0.1) * sigma && (*r - med) >= -3.0 * sigma;
            // Once rejected, a sample stays rejected: the model only ever
            // settles down onto the background.
            if *k && !ok {
                *k = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut surfaces = Vec::with_capacity(3);
    let mut pedestal = [0f32; 3];
    for c in 0..3 {
        let pts: Vec<(f64, f64, f64)> =
            samples.iter().zip(&keep).filter(|(_, k)| **k).map(|(s, _)| (s.u, s.v, s.rgb[c])).collect();
        let s = Surface::fit(degree, &pts)?;
        let mut levels: Vec<f32> = samples.iter().map(|p| s.eval(p.u as f32, p.v as f32)).collect();
        pedestal[c] = median_in_place(&mut levels);
        surfaces.push(s);
    }
    let surfaces: [Surface; 3] = surfaces.try_into().ok()?;
    Some(GradientModel { surfaces, pedestal, strength: params.strength })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat sky of 0.1 plus a tilted plane, with a bright "galaxy" blob.
    fn scene(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                let sky = 0.1 + 0.06 * u - 0.03 * v;
                let d2 = ((u - 0.5) / 0.2).powi(2) + ((v - 0.5) / 0.15).powi(2);
                let galaxy = 0.5 * (-d2).exp();
                let i = (y * w + x) * 3;
                img.data[i] = sky * 1.2 + galaxy;
                img.data[i + 1] = sky + galaxy;
                img.data[i + 2] = sky * 0.8 + galaxy;
            }
        }
        img
    }

    #[test]
    fn removes_a_linear_gradient_but_keeps_the_object() {
        let img = scene(480, 320);
        let m = fit(&img, &Gradient { enabled: true, degree: 1, tolerance: 1.0, strength: 1.0 }).unwrap();
        // Corners become equal after subtraction (the gradient is gone)...
        let corrected = |u: f32, v: f32| {
            let p = img.px((u * 479.0) as usize, (v * 319.0) as usize);
            let g = m.eval(u, v);
            [p[0] - g[0], p[1] - g[1], p[2] - g[2]]
        };
        let (a, b) = (corrected(0.02, 0.02), corrected(0.98, 0.98));
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() < 0.003, "channel {c}: {} vs {}", a[c], b[c]);
        }
        // ...the sky keeps roughly its original level...
        assert!((a[1] - 0.115).abs() < 0.02, "{}", a[1]);
        // ...and the galaxy core is still ~0.5 above the sky.
        let core = corrected(0.5, 0.5);
        assert!((core[1] - a[1] - 0.5).abs() < 0.02, "{}", core[1] - a[1]);
    }

    #[test]
    fn disabled_or_starved_returns_none() {
        let img = scene(480, 320);
        assert!(fit(&img, &Gradient::default()).is_none());
        let blank = Image::new(480, 320); // all uncovered
        assert!(fit(&blank, &Gradient { enabled: true, ..Gradient::default() }).is_none());
    }

    #[test]
    fn surface_fit_recovers_a_quadratic() {
        let f = |u: f64, v: f64| 0.3 + 0.2 * u - 0.1 * v + 0.05 * u * v + 0.07 * u * u;
        let pts: Vec<_> = (0..100).map(|i| ((i % 10) as f64 / 9.0, (i / 10) as f64 / 9.0)).map(|(u, v)| (u, v, f(u, v))).collect();
        let s = Surface::fit(2, &pts).unwrap();
        assert!((s.eval(0.37, 0.81) as f64 - f(0.37, 0.81)).abs() < 1e-5);
    }
}
