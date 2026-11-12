//! Tone curves through user control points (monotone cubic interpolation, so
//! the curve never overshoots between points).

use super::params::CurvePoints;

pub const LUT_SIZE: usize = 4096;

/// Sorted, de-duplicated copy of the points, clamped to 0..1.
pub fn normalized(points: &CurvePoints) -> Vec<[f32; 2]> {
    let mut p: Vec<[f32; 2]> = points
        .iter()
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
        .collect();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-4);
    p
}

pub fn is_identity(points: &CurvePoints) -> bool {
    let p = normalized(points);
    p.len() < 2 || p.iter().all(|p| (p[0] - p[1]).abs() < 1e-6)
}

/// Evaluate the curve at `x`. Outside the first/last point the curve is flat.
pub fn evaluate(points: &[[f32; 2]], tangents: &[f32], x: f32) -> f32 {
    let n = points.len();
    if n == 0 {
        return x;
    }
    if x <= points[0][0] {
        return points[0][1];
    }
    if x >= points[n - 1][0] {
        return points[n - 1][1];
    }
    let i = points.partition_point(|p| p[0] <= x) - 1;
    let (x0, y0, x1, y1) = (points[i][0], points[i][1], points[i + 1][0], points[i + 1][1]);
    let h = x1 - x0;
    let t = (x - x0) / h;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * y0
        + (t3 - 2.0 * t2 + t) * h * tangents[i]
        + (-2.0 * t3 + 3.0 * t2) * y1
        + (t3 - t2) * h * tangents[i + 1]
}

/// Fritsch-Carlson tangents: monotone wherever the data is monotone.
pub fn tangents(points: &[[f32; 2]]) -> Vec<f32> {
    let n = points.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let d: Vec<f32> = (0..n - 1)
        .map(|i| (points[i + 1][1] - points[i][1]) / (points[i + 1][0] - points[i][0]))
        .collect();
    let mut m = vec![0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    m
}

/// A sampled curve for fast per-pixel lookup.
#[derive(Clone, Debug)]
pub struct Lut(Vec<f32>);

impl Lut {
    pub fn new(points: &CurvePoints) -> Option<Lut> {
        if is_identity(points) {
            return None;
        }
        let p = normalized(points);
        let m = tangents(&p);
        Some(Lut(
            (0..=LUT_SIZE)
                .map(|i| evaluate(&p, &m, i as f32 / LUT_SIZE as f32).clamp(0.0, 1.0))
                .collect(),
        ))
    }

    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        let f = x.clamp(0.0, 1.0) * LUT_SIZE as f32;
        let i = (f as usize).min(LUT_SIZE - 1);
        let t = f - i as f32;
        self.0[i] * (1.0 - t) + self.0[i + 1] * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_detected() {
        assert!(Lut::new(&vec![[0.0, 0.0], [1.0, 1.0]]).is_none());
        assert!(Lut::new(&vec![[0.0, 0.0], [0.3, 0.3], [1.0, 1.0]]).is_none());
        assert!(Lut::new(&vec![]).is_none());
    }

    #[test]
    fn passes_through_points_and_stays_monotone() {
        let pts = vec![[1.0, 1.0], [0.0, 0.0], [0.25, 0.5], [0.6, 0.62]]; // unsorted on purpose
        let lut = Lut::new(&pts).unwrap();
        for p in &pts {
            assert!((lut.apply(p[0]) - p[1]).abs() < 2e-3, "at {}", p[0]);
        }
        let mut prev = -1.0;
        for i in 0..=1000 {
            let y = lut.apply(i as f32 / 1000.0);
            assert!(y >= prev - 1e-6, "not monotone at {i}");
            assert!((0.0..=1.0).contains(&y));
            prev = y;
        }
    }

    #[test]
    fn flat_beyond_end_points() {
        let lut = Lut::new(&vec![[0.2, 0.1], [0.8, 0.9]]).unwrap();
        assert!((lut.apply(0.0) - 0.1).abs() < 1e-6);
        assert!((lut.apply(1.0) - 0.9).abs() < 1e-6);
    }
}
