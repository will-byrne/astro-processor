//! Generalised Hyperbolic Stretch (Payne & Cranfield, as in Siril and
//! PixInsight's GHS script).
//!
//! A family of stretches controlled by where the stretch is concentrated
//! (the symmetry point), how strongly (the stretch factor), how it is shaped
//! (local intensity `b`: -1 logarithmic, 0 exponential, 1 harmonic, larger is
//! more focused around the symmetry point) and which ends are protected (the
//! curve is continued linearly below the shadow protection point and above
//! the highlight protection point).

use super::params::Ghs;

/// Precomputed curve for one parameter set.
#[derive(Clone, Copy, Debug)]
pub struct GhsCurve {
    d: f32,
    b: f32,
    sp: f32,
    lp: f32,
    hp: f32,
    /// Value at 0 and 1/(f(1) - f(0)), for normalising to 0..1.
    f0: f32,
    inv_range: f32,
}

/// Base transform: maps 0 -> 0 with slope `d`, concave for `d > 0`.
#[inline]
fn t(d: f32, b: f32, x: f32) -> f32 {
    if b == -1.0 {
        (d * x).ln_1p()
    } else if b < 0.0 {
        (1.0 - (1.0 - b * d * x).max(0.0).powf((b + 1.0) / b)) / (d * (b + 1.0))
    } else if b == 0.0 {
        1.0 - (-d * x).exp()
    } else {
        1.0 - (1.0 + b * d * x).powf(-1.0 / b)
    }
}

/// Derivative of `t` with respect to `x`.
#[inline]
fn dt(d: f32, b: f32, x: f32) -> f32 {
    if b == -1.0 {
        d / (1.0 + d * x)
    } else if b < 0.0 {
        (1.0 - b * d * x).max(0.0).powf(1.0 / b)
    } else if b == 0.0 {
        d * (-d * x).exp()
    } else {
        d * (1.0 + b * d * x).powf(-(1.0 + b) / b)
    }
}

impl GhsCurve {
    /// `None` when the parameters describe no stretch at all.
    pub fn new(g: &Ghs) -> Option<GhsCurve> {
        if !(g.stretch > 0.0) {
            return None;
        }
        // The slider works in ln(D + 1), like Siril's.
        let d = g.stretch.min(20.0).exp_m1();
        let b = g.local_intensity.clamp(-5.0, 15.0);
        let sp = g.symmetry.clamp(0.0, 1.0);
        let lp = g.shadow_protect.clamp(0.0, sp);
        let hp = g.highlight_protect.clamp(sp, 1.0);
        let mut c = GhsCurve { d, b, sp, lp, hp, f0: 0.0, inv_range: 1.0 };
        let (f0, f1) = (c.raw(0.0), c.raw(1.0));
        if !(f1 - f0 > 1e-12) {
            return None;
        }
        c.f0 = f0;
        c.inv_range = 1.0 / (f1 - f0);
        Some(c)
    }

    /// The un-normalised piecewise curve.
    #[inline]
    fn raw(&self, x: f32) -> f32 {
        let GhsCurve { d, b, sp, lp, hp, .. } = *self;
        if x < lp {
            -t(d, b, sp - lp) + dt(d, b, sp - lp) * (x - lp)
        } else if x < sp {
            -t(d, b, sp - x)
        } else if x < hp {
            t(d, b, x - sp)
        } else {
            t(d, b, hp - sp) + dt(d, b, hp - sp) * (x - hp)
        }
    }

    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        ((self.raw(x.clamp(0.0, 1.0)) - self.f0) * self.inv_range).clamp(0.0, 1.0)
    }
}

/// Stretch a pixel in place, either each channel separately or (with
/// `preserve_color`) all channels by the factor that stretches their mean, so
/// hue and saturation survive.
#[inline]
pub fn apply(curve: &GhsCurve, preserve_color: bool, p: &mut [f32; 3]) {
    if preserve_color {
        let l = (p[0] + p[1] + p[2]) / 3.0;
        if l > 1e-9 {
            let k = curve.apply(l) / l;
            for v in p.iter_mut() {
                *v *= k;
            }
        }
    } else {
        for v in p.iter_mut() {
            *v = curve.apply(*v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ghs(stretch: f32, b: f32, sp: f32) -> Ghs {
        Ghs { stretch, local_intensity: b, symmetry: sp, ..Ghs::default() }
    }

    #[test]
    fn off_by_default() {
        assert!(GhsCurve::new(&Ghs::default()).is_none());
    }

    #[test]
    fn fixes_the_ends_and_is_monotone_for_every_shape() {
        for b in [-5.0, -1.0, -0.5, 0.0, 0.5, 1.0, 4.0, 15.0] {
            for sp in [0.0, 0.05, 0.5] {
                let g = Ghs { shadow_protect: sp * 0.5, highlight_protect: 0.9, ..ghs(5.0, b, sp) };
                let c = GhsCurve::new(&g).unwrap();
                assert!(c.apply(0.0).abs() < 1e-5, "b={b} sp={sp}");
                assert!((c.apply(1.0) - 1.0).abs() < 1e-5, "b={b} sp={sp}");
                let mut prev = -1.0;
                for i in 0..=1000 {
                    let y = c.apply(i as f32 / 1000.0);
                    assert!(y.is_finite() && y >= prev - 1e-6, "b={b} sp={sp} at {i}: {y} < {prev}");
                    prev = y;
                }
            }
        }
    }

    #[test]
    fn stretches_faint_data_around_the_symmetry_point() {
        let c = GhsCurve::new(&ghs(6.0, 2.0, 0.02)).unwrap();
        // Faint signal just above the sky is pulled well apart...
        let spread = c.apply(0.04) - c.apply(0.02);
        assert!(spread > 0.15, "spread {spread}");
        // ...while bright values are compressed toward white.
        assert!(c.apply(0.8) > 0.9);
    }

    #[test]
    fn colour_preserving_mode_keeps_ratios() {
        let c = GhsCurve::new(&ghs(5.0, 0.0, 0.0)).unwrap();
        let mut p = [0.02, 0.01, 0.005];
        apply(&c, true, &mut p);
        assert!((p[0] / p[1] - 2.0).abs() < 1e-4 && (p[1] / p[2] - 2.0).abs() < 1e-4);
        assert!(p[1] > 0.03);
    }

    #[test]
    fn tiny_stretch_is_nearly_linear() {
        let c = GhsCurve::new(&ghs(1e-3, 0.0, 0.0)).unwrap();
        for x in [0.1, 0.4, 0.7] {
            assert!((c.apply(x) - x).abs() < 1e-3);
        }
    }
}
