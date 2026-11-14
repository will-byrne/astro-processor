//! Point operations: stretch, levels, exposure, contrast, highlights,
//! shadows, saturation, green removal.

use super::ghs::GhsCurve;
use super::params::{Level, Stretch, Tone};
use crate::image::luma;

/// Midtones transfer function: maps 0 -> 0, `m` -> 0.5, 1 -> 1.
#[inline]
pub fn mtf(m: f32, x: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else {
        (m - 1.0) * x / ((2.0 * m - 1.0) * x - m)
    }
}

/// Stretch that puts the sky background at `target` brightness and clips
/// `shadow_sigma` standard deviations below it to black. `median`/`sigma`
/// describe the image luminance going into the stretch.
pub fn auto_stretch(median: f32, sigma: f32, target: f32, shadow_sigma: f32) -> Stretch {
    let black = (median - shadow_sigma * sigma).clamp(0.0, 0.95);
    let x0 = ((median - black) / (1.0 - black)).clamp(1e-6, 0.999);
    // mtf(m, x0) = target  <=>  m = mtf(target, x0)
    let midtone = mtf(target, x0).clamp(1e-5, 0.999);
    Stretch { black, midtone, ..Stretch::default() }
}

#[inline]
pub fn apply_stretch(s: &Stretch, ghs: Option<&GhsCurve>, p: &mut [f32; 3]) {
    let inv = 1.0 / (1.0 - s.black).max(1e-6);
    for v in p.iter_mut() {
        *v = ((*v - s.black) * inv).max(0.0);
    }
    if s.asinh > 0.0 {
        // Scale all channels by the same factor so star colours survive.
        let l = (p[0] + p[1] + p[2]) / 3.0;
        if l > 1e-9 {
            let k = (s.asinh * l).asinh() / (s.asinh.asinh() * l);
            for v in p.iter_mut() {
                *v *= k;
            }
        }
    }
    if let Some(curve) = ghs {
        super::ghs::apply(curve, s.ghs.preserve_color, p);
    }
    if s.midtone != 0.5 {
        for v in p.iter_mut() {
            *v = mtf(s.midtone, *v);
        }
    }
}

#[inline]
pub fn apply_level(l: &Level, x: f32) -> f32 {
    let range = (l.white - l.black).max(1e-6);
    let v = ((x - l.black) / range).clamp(0.0, 1.0);
    if l.gamma == 1.0 { v } else { v.powf(1.0 / l.gamma.max(0.01)) }
}

pub fn level_is_identity(l: &Level) -> bool {
    *l == Level::default()
}

/// Shadows/highlights response applied to luminance. Monotonic for all
/// amounts in -1..1, and fixes 0 and 1.
#[inline]
fn shadow_highlight(l: f32, shadows: f32, highlights: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    let ks = if shadows >= 0.0 { 2.0 } else { 0.95 };
    let kh = if highlights >= 0.0 { 0.95 } else { 2.0 };
    l + shadows * ks * l * (1.0 - l) * (1.0 - l) + highlights * kh * l * l * (1.0 - l)
}

#[inline]
fn contrast_curve(x: f32, c: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if c >= 0.0 {
        let s = x * x * (3.0 - 2.0 * x);
        x + (s - x) * c
    } else {
        x + (0.5 - x) * (-c) * 0.5
    }
}

pub fn tone_is_identity(t: &Tone) -> bool {
    *t == Tone::default()
}

#[inline]
pub fn apply_tone(t: &Tone, p: &mut [f32; 3]) {
    if t.exposure != 0.0 {
        let g = 2f32.powf(t.exposure);
        for v in p.iter_mut() {
            *v *= g;
        }
    }
    if t.contrast != 0.0 {
        for v in p.iter_mut() {
            *v = contrast_curve(*v, t.contrast);
        }
    }
    if t.shadows != 0.0 || t.highlights != 0.0 {
        let l = luma(p[0], p[1], p[2]);
        if l > 1e-6 {
            let k = shadow_highlight(l, t.shadows, t.highlights) / l.min(1.0);
            for v in p.iter_mut() {
                *v *= k;
            }
        }
    }
    if t.green_removal > 0.0 {
        let limit = 0.5 * (p[0] + p[2]);
        if p[1] > limit {
            p[1] += (limit - p[1]) * t.green_removal.min(1.0);
        }
    }
    if t.saturation != 1.0 {
        let l = luma(p[0], p[1], p[2]);
        for v in p.iter_mut() {
            *v = l + (*v - l) * t.saturation;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtf_fixed_points() {
        for m in [0.01, 0.2, 0.5, 0.9] {
            assert_eq!(mtf(m, 0.0), 0.0);
            assert_eq!(mtf(m, 1.0), 1.0);
            assert!((mtf(m, m) - 0.5).abs() < 1e-6);
        }
        assert!((mtf(0.5, 0.3) - 0.3).abs() < 1e-6, "0.5 is linear");
    }

    #[test]
    fn auto_stretch_places_the_background_at_the_target() {
        let s = auto_stretch(0.016, 0.001, 0.2, 2.8);
        assert!((s.black - 0.0132).abs() < 1e-5);
        let mut p = [0.016; 3];
        apply_stretch(&s, None, &mut p);
        assert!((p[0] - 0.2).abs() < 1e-4, "{}", p[0]);
        let mut dark = [0.012; 3];
        apply_stretch(&s, None, &mut dark);
        assert_eq!(dark, [0.0; 3]);
    }

    #[test]
    fn asinh_preserves_colour_ratios() {
        let s = Stretch { asinh: 200.0, ..Stretch::default() };
        let mut p = [0.02, 0.01, 0.005];
        apply_stretch(&s, None, &mut p);
        assert!(p[0] > 0.1, "stretched: {p:?}");
        assert!((p[0] / p[1] - 2.0).abs() < 1e-4 && (p[1] / p[2] - 2.0).abs() < 1e-4);
        let mut white = [1.0; 3];
        apply_stretch(&s, None, &mut white);
        assert!((white[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn defaults_are_identity() {
        let mut p = [0.1, 0.5, 0.9];
        apply_stretch(&Stretch::default(), None, &mut p);
        apply_tone(&Tone::default(), &mut p);
        assert_eq!(p, [0.1, 0.5, 0.9]);
        assert_eq!(apply_level(&Level::default(), 0.37), 0.37);
    }

    #[test]
    fn levels_map_black_and_white_points() {
        let l = Level { black: 0.1, gamma: 1.0, white: 0.6 };
        assert_eq!(apply_level(&l, 0.1), 0.0);
        assert_eq!(apply_level(&l, 0.6), 1.0);
        assert!((apply_level(&l, 0.35) - 0.5).abs() < 1e-6);
        let g = Level { black: 0.0, gamma: 2.0, white: 1.0 };
        assert!((apply_level(&g, 0.25) - 0.5).abs() < 1e-6, "gamma 2 brightens midtones");
    }

    #[test]
    fn shadows_and_highlights_are_monotone_and_targeted() {
        for (s, h) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
            let mut prev = -1.0;
            for i in 0..=200 {
                let y = shadow_highlight(i as f32 / 200.0, s, h);
                assert!(y >= prev, "s={s} h={h} not monotone at {i}");
                prev = y;
            }
            assert_eq!(shadow_highlight(0.0, s, h), 0.0);
            assert!((shadow_highlight(1.0, s, h) - 1.0).abs() < 1e-6);
        }
        // Lifting shadows moves dark tones much more than bright ones.
        let dark = shadow_highlight(0.2, 1.0, 0.0) - 0.2;
        let bright = shadow_highlight(0.9, 1.0, 0.0) - 0.9;
        assert!(dark > 0.2 && bright < 0.02);
        // Pulling highlights down does the opposite.
        assert!(0.8 - shadow_highlight(0.8, 0.0, -1.0) > 0.2);
        assert!(0.1 - shadow_highlight(0.1, 0.0, -1.0) < 0.02);
    }

    #[test]
    fn exposure_contrast_saturation_green() {
        let mut p = [0.1, 0.2, 0.3];
        apply_tone(&Tone { exposure: 1.0, ..Tone::default() }, &mut p);
        assert_eq!(p, [0.2, 0.4, 0.6]);

        let c = Tone { contrast: 1.0, ..Tone::default() };
        let (mut lo, mut mid, mut hi) = ([0.25; 3], [0.5; 3], [0.75; 3]);
        apply_tone(&c, &mut lo);
        apply_tone(&c, &mut mid);
        apply_tone(&c, &mut hi);
        assert!(lo[0] < 0.25 && (mid[0] - 0.5).abs() < 1e-6 && hi[0] > 0.75);

        let mut p = [0.2, 0.6, 0.4];
        apply_tone(&Tone { saturation: 0.0, ..Tone::default() }, &mut p);
        assert!((p[0] - p[1]).abs() < 1e-6 && (p[1] - p[2]).abs() < 1e-6);

        let mut p = [0.2, 0.6, 0.4];
        apply_tone(&Tone { green_removal: 1.0, ..Tone::default() }, &mut p);
        assert!((p[1] - 0.3).abs() < 1e-6);
        let mut p = [0.5, 0.2, 0.5]; // not green-dominant: untouched
        apply_tone(&Tone { green_removal: 1.0, ..Tone::default() }, &mut p);
        assert_eq!(p, [0.5, 0.2, 0.5]);
    }
}
