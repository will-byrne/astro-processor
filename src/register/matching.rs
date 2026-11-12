//! Match star lists between two frames and solve for the affine transform.
//!
//! Triangles formed by the brightest stars are compared by shape (which is
//! invariant to shift, rotation and scale); matching triangles vote for star
//! correspondences, and the best-supported correspondences seed a transform
//! that is then verified and refined against all stars.

use super::stars::Star;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// `x' = a*x + b*y + c`, `y' = d*x + e*y + f`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

type Pt = (f64, f64);

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 0.0, e: 1.0, f: 0.0 };

    #[inline]
    pub fn apply(&self, x: f64, y: f64) -> Pt {
        (self.a * x + self.b * y + self.c, self.d * x + self.e * y + self.f)
    }

    pub fn det(&self) -> f64 {
        self.a * self.e - self.b * self.d
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// Least-squares fit mapping each `from` point onto its `to` point.
    pub fn fit(pairs: &[(Pt, Pt)]) -> Option<Affine> {
        if pairs.len() < 3 {
            return None;
        }
        // Centre the inputs for conditioning.
        let n = pairs.len() as f64;
        let (mx, my) = pairs.iter().fold((0.0, 0.0), |s, p| (s.0 + p.0.0 / n, s.1 + p.0.1 / n));
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        let (mut sxu, mut syu, mut su) = (0.0, 0.0, 0.0);
        let (mut sxv, mut syv, mut sv) = (0.0, 0.0, 0.0);
        for &((x, y), (u, v)) in pairs {
            let (x, y) = (x - mx, y - my);
            sxx += x * x;
            sxy += x * y;
            syy += y * y;
            sxu += x * u;
            syu += y * u;
            su += u;
            sxv += x * v;
            syv += y * v;
            sv += v;
        }
        let det = sxx * syy - sxy * sxy;
        if det.abs() < 1e-9 * (sxx * syy).max(1e-12) {
            return None; // collinear points
        }
        let a = (sxu * syy - syu * sxy) / det;
        let b = (syu * sxx - sxu * sxy) / det;
        let d = (sxv * syy - syv * sxy) / det;
        let e = (syv * sxx - sxv * sxy) / det;
        Some(Affine { a, b, c: su / n - a * mx - b * my, d, e, f: sv / n - d * mx - e * my })
    }

    /// Convert a transform measured on the 2x2-binned image to full-resolution
    /// pixel coordinates (binned pixel `i` covers full pixels `2i` and `2i+1`).
    pub fn binned_to_full(&self) -> Affine {
        Affine {
            c: 2.0 * self.c + 0.5 - 0.5 * (self.a + self.b),
            f: 2.0 * self.f + 0.5 - 0.5 * (self.d + self.e),
            ..*self
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Match {
    /// Maps reference-frame coordinates to this frame's coordinates.
    pub transform: Affine,
    pub inliers: usize,
    /// RMS residual of the inliers, in pixels.
    pub rms: f32,
    /// Median reference/frame flux ratio of matched stars (transparency).
    pub flux_ratio: f32,
}

struct Triangle {
    /// Vertices ordered: opposite the longest, middle, shortest side.
    v: [usize; 3],
    r1: f32,
    r2: f32,
    longest: f32,
}

/// Brightest stars used for all-pairs triangles.
const GLOBAL_STARS: usize = 30;
/// Stars used for neighbourhood triangles, and neighbours per star.
const LOCAL_STARS: usize = 160;
const LOCAL_NEIGHBOURS: usize = 6;
const SHAPE_TOL: f32 = 0.004;

/// Triangles from two sources: every triple of the very brightest stars
/// (works when the frames overlap almost fully), and each star with pairs of
/// its nearest neighbours (still works when the overlap is partial, because
/// a neighbourhood is either wholly in both frames or not).
fn triangles(stars: &[Star]) -> Vec<Triangle> {
    let mut out = Vec::new();
    let dist = |i: usize, j: usize| {
        ((stars[i].x - stars[j].x).powi(2) + (stars[i].y - stars[j].y).powi(2)).sqrt()
    };
    let mut seen = std::collections::HashSet::new();
    let mut add = |i: usize, j: usize, k: usize, out: &mut Vec<Triangle>| {
        let mut key = [i, j, k];
        key.sort_unstable();
        if key[0] == key[1] || key[1] == key[2] || !seen.insert(key) {
            return;
        }
        // (side length, opposite vertex)
        let mut s = [(dist(j, k), i), (dist(i, k), j), (dist(i, j), k)];
        s.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (a, b, c) = (s[0].0, s[1].0, s[2].0);
        // Skip tiny triangles (imprecise) and near-isosceles ones (ambiguous
        // vertex order).
        if a < 40.0 || b / a > 0.96 || c / b > 0.96 || c / a < 0.15 {
            return;
        }
        out.push(Triangle { v: [s[0].1, s[1].1, s[2].1], r1: b / a, r2: c / a, longest: a });
    };

    let n = stars.len().min(GLOBAL_STARS);
    for i in 0..n {
        for j in i + 1..n {
            for k in j + 1..n {
                add(i, j, k, &mut out);
            }
        }
    }
    let n = stars.len().min(LOCAL_STARS);
    for i in 0..n {
        let mut near: Vec<(f32, usize)> = (0..n).filter(|&j| j != i).map(|j| (dist(i, j), j)).collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.truncate(LOCAL_NEIGHBOURS);
        for a in 0..near.len() {
            for b in a + 1..near.len() {
                add(i, near[a].1, near[b].1, &mut out);
            }
        }
    }
    out.sort_by(|a, b| a.r1.total_cmp(&b.r1));
    out
}

/// For each reference star, the nearest frame star to its predicted position
/// if within `tol` pixels. Each frame star is used at most once.
fn pair_up(t: &Affine, reference: &[Star], frame: &[Star], tol: f64) -> Vec<(usize, usize, f64)> {
    let mut best: HashMap<usize, (usize, f64)> = HashMap::new();
    for (ri, r) in reference.iter().enumerate() {
        let (px, py) = t.apply(r.x as f64, r.y as f64);
        let mut hit: Option<(usize, f64)> = None;
        for (fi, f) in frame.iter().enumerate() {
            let (dx, dy) = (f.x as f64 - px, f.y as f64 - py);
            if dx.abs() > tol || dy.abs() > tol {
                continue;
            }
            let d2 = dx * dx + dy * dy;
            if d2 <= tol * tol && hit.is_none_or(|h| d2 < h.1) {
                hit = Some((fi, d2));
            }
        }
        if let Some((fi, d2)) = hit {
            let e = best.entry(fi).or_insert((ri, d2));
            if d2 < e.1 {
                *e = (ri, d2);
            }
        }
    }
    let mut v: Vec<_> = best.into_iter().map(|(fi, (ri, d2))| (ri, fi, d2)).collect();
    v.sort_by_key(|p| p.0);
    v
}

fn pts(pairs: &[(usize, usize, f64)], reference: &[Star], frame: &[Star]) -> Vec<(Pt, Pt)> {
    pairs
        .iter()
        .map(|&(ri, fi, _)| {
            ((reference[ri].x as f64, reference[ri].y as f64), (frame[fi].x as f64, frame[fi].y as f64))
        })
        .collect()
}

/// Find the transform taking `reference` star positions to `frame` star
/// positions. Both lists must be sorted brightest-first. Returns `None` when
/// the frames cannot be matched confidently.
pub fn find_transform(reference: &[Star], frame: &[Star]) -> Option<Match> {
    if reference.len() < 5 || frame.len() < 5 {
        return None;
    }
    let tr = triangles(reference);
    let tf = triangles(frame);

    // Vote for star correspondences from similar triangles.
    let mut votes: HashMap<(usize, usize), u32> = HashMap::new();
    for f in &tf {
        let lo = tr.partition_point(|t| t.r1 < f.r1 - SHAPE_TOL);
        for r in tr[lo..].iter().take_while(|t| t.r1 <= f.r1 + SHAPE_TOL) {
            let scale = f.longest / r.longest;
            if (r.r2 - f.r2).abs() <= SHAPE_TOL && (0.9..=1.1).contains(&scale) {
                for k in 0..3 {
                    *votes.entry((r.v[k], f.v[k])).or_insert(0) += 1;
                }
            }
        }
    }
    let mut ranked: Vec<((usize, usize), u32)> = votes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // Keep the best-voted pairing for each star on either side.
    let mut seeds: Vec<(usize, usize)> = Vec::new();
    for ((ri, fi), _) in ranked {
        if seeds.len() >= 12 {
            break;
        }
        if seeds.iter().all(|s| s.0 != ri && s.1 != fi) {
            seeds.push((ri, fi));
        }
    }
    if seeds.len() < 3 {
        return None;
    }

    // Try every seed triple; keep the transform most stars agree with.
    let reference = &reference[..reference.len().min(300)];
    let p = |s: (usize, usize)| {
        ((reference[s.0].x as f64, reference[s.0].y as f64), (frame[s.1].x as f64, frame[s.1].y as f64))
    };
    let mut best: Option<(Affine, usize)> = None;
    for i in 0..seeds.len() {
        for j in i + 1..seeds.len() {
            for k in j + 1..seeds.len() {
                let Some(t) = Affine::fit(&[p(seeds[i]), p(seeds[j]), p(seeds[k])]) else { continue };
                if !(0.8..=1.25).contains(&t.det()) {
                    continue; // not a rigid-ish transform (or mirrored)
                }
                let n = pair_up(&t, reference, frame, 3.0).len();
                if best.is_none_or(|b| n > b.1) {
                    best = Some((t, n));
                }
            }
        }
    }
    let (mut t, _) = best?;

    // Refine on all agreeing stars with a tightening tolerance.
    for tol in [3.0, 2.0, 1.5, 1.5] {
        let pairs = pair_up(&t, reference, frame, tol);
        if pairs.len() < 3 {
            return None;
        }
        t = Affine::fit(&pts(&pairs, reference, frame))?;
    }
    let pairs = pair_up(&t, reference, frame, 1.5);

    let usable = reference.len().min(frame.len());
    let needed = if usable < 16 { 5 } else { (usable / 8).clamp(8, 25) };
    if pairs.len() < needed {
        return None;
    }
    let rms = (pairs.iter().map(|p| p.2).sum::<f64>() / pairs.len() as f64).sqrt() as f32;
    // Bright stars only: their fluxes are measured well above the noise.
    let mut by_flux = pairs.clone();
    by_flux.sort_by(|a, b| reference[b.0].flux.total_cmp(&reference[a.0].flux));
    by_flux.truncate(60);
    let mut ratios: Vec<f32> = by_flux
        .iter()
        .map(|&(ri, fi, _)| reference[ri].flux / frame[fi].flux.max(1e-12))
        .collect();
    let flux_ratio = crate::image::median_in_place(&mut ratios);
    Some(Match { transform: t, inliers: pairs.len(), rms, flux_ratio })
}

#[cfg(test)]
mod tests {
    use super::super::stars::tests::{Lcg, random_field};
    use super::*;

    fn stars(field: &[(f32, f32, f32)]) -> Vec<Star> {
        let mut v: Vec<Star> =
            field.iter().map(|&(x, y, a)| Star { x, y, flux: a, fwhm: 3.0 }).collect();
        v.sort_by(|a, b| b.flux.total_cmp(&a.flux));
        v
    }

    fn transformed(field: &[(f32, f32, f32)], t: &Affine, jitter: f32, gain: f32, seed: u64) -> Vec<(f32, f32, f32)> {
        let mut rng = Lcg(seed);
        field
            .iter()
            .map(|&(x, y, a)| {
                let (u, v) = t.apply(x as f64, y as f64);
                (u as f32 + jitter * rng.gauss(), v as f32 + jitter * rng.gauss(), a * gain)
            })
            .collect()
    }

    fn rotation(deg: f64, cx: f64, cy: f64, dx: f64, dy: f64) -> Affine {
        let (s, c) = deg.to_radians().sin_cos();
        Affine { a: c, b: -s, c: cx - c * cx + s * cy + dx, d: s, e: c, f: cy - s * cx - c * cy + dy }
    }

    fn assert_close(found: &Affine, truth: &Affine, tol: f64) {
        for (x, y) in [(0.0, 0.0), (1500.0, 0.0), (0.0, 1000.0), (1500.0, 1000.0)] {
            let (a, b) = (found.apply(x, y), truth.apply(x, y));
            let err = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
            assert!(err < tol, "corner ({x}, {y}) off by {err}");
        }
    }

    #[test]
    fn recovers_shift_and_rotation() {
        let field = random_field(120, 1500, 1000, 11);
        let truth = rotation(1.7, 750.0, 500.0, 63.0, -41.0);
        let mut moved = transformed(&field, &truth, 0.05, 0.8, 5);
        moved.truncate(110); // some stars missing
        moved.extend(random_field(15, 1500, 1000, 99)); // and some spurious ones
        let m = find_transform(&stars(&field), &stars(&moved)).expect("should match");
        assert_close(&m.transform, &truth, 0.2);
        assert!(m.inliers >= 60, "inliers {}", m.inliers);
        assert!(m.rms < 0.3);
        assert!((m.flux_ratio - 1.25).abs() < 0.01);
    }

    #[test]
    fn recovers_a_large_shift_with_partial_overlap() {
        // The frame is shifted by 45% of the field: only about half of each
        // star list is shared.
        let sky = random_field(400, 3000, 1000, 31);
        let visible = |x0: f32| -> Vec<(f32, f32, f32)> {
            sky.iter().filter(|s| s.0 >= x0 && s.0 < x0 + 1500.0).map(|&(x, y, a)| (x - x0, y, a)).collect()
        };
        let (a, b) = (visible(0.0), visible(675.0));
        let m = find_transform(&stars(&a), &stars(&b)).expect("should match");
        let truth = Affine { c: -675.0, ..Affine::IDENTITY };
        assert_close(&m.transform, &truth, 0.05);
    }

    #[test]
    fn recovers_meridian_flip() {
        let field = random_field(80, 1500, 1000, 21);
        let truth = rotation(180.4, 750.0, 500.0, -12.0, 9.0);
        let moved = transformed(&field, &truth, 0.05, 1.0, 6);
        let m = find_transform(&stars(&field), &stars(&moved)).expect("should match");
        assert_close(&m.transform, &truth, 0.2);
    }

    #[test]
    fn unrelated_fields_do_not_match() {
        let a = stars(&random_field(100, 1500, 1000, 1));
        let b = stars(&random_field(100, 1500, 1000, 2));
        assert!(find_transform(&a, &b).is_none());
    }

    #[test]
    fn fit_is_exact_for_exact_data_and_rejects_collinear() {
        let t = Affine { a: 0.99, b: -0.02, c: 12.5, d: 0.021, e: 1.01, f: -7.25 };
        let src = [(0.0, 0.0), (100.0, 5.0), (40.0, 80.0), (250.0, 300.0)];
        let pairs: Vec<_> = src.iter().map(|&p| (p, t.apply(p.0, p.1))).collect();
        let got = Affine::fit(&pairs).unwrap();
        assert_close(&got, &t, 1e-6);
        let line: Vec<_> = (0..5).map(|i| ((i as f64, 2.0 * i as f64), (0.0, 0.0))).collect();
        assert!(Affine::fit(&line).is_none());
    }

    #[test]
    fn binned_transform_maps_full_resolution_pixels() {
        // A star at binned (10, 20) -> (13.5, 18.25): its full-res centre is
        // at 2*x + 0.5 on both sides.
        let t = Affine { a: 1.0, b: 0.0, c: 3.5, d: 0.0, e: 1.0, f: -1.75 };
        let full = t.binned_to_full();
        let (x, y) = full.apply(20.5, 40.5);
        assert!((x - 27.5).abs() < 1e-9 && (y - 37.0).abs() < 1e-9);
    }
}
