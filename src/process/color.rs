//! Colour calibration for stacks that arrive without a white balance.
//!
//! Stacks made elsewhere from DSLR or one-shot-colour data are often left in
//! the sensor's native colour, where green is several times more sensitive
//! than red. Neutralising the background only shifts the channels, so stars
//! and nebulae stay green. Scaling the channels so that the typical star is
//! white fixes that, the way "white reference from stars" does in other
//! astro software.

use crate::image::{Image, median_in_place};
use crate::register::stars::{DetectOptions, detect};

/// Fewer usable stars than this and the result is too noisy to trust.
const MIN_STARS: usize = 12;

/// White balance multipliers (R, G, B; green is 1) that make the median star
/// white, or `None` if too few unsaturated stars could be measured.
///
/// `img` must be linear: on a stretched picture star colours are distorted.
pub fn star_white_balance(img: &Image) -> Option<[f32; 3]> {
    let (w, h) = (img.width, img.height);
    let det = detect(&img.luminance(), &DetectOptions { sigma: 10.0, max_stars: 500 });
    if det.stars.len() < MIN_STARS {
        return None;
    }
    // A star touching a channel's clipping level has lost its true colour.
    let mut peak = [0f32; 3];
    for p in img.data.chunks_exact(3) {
        for c in 0..3 {
            if p[c].is_finite() {
                peak[c] = peak[c].max(p[c]);
            }
        }
    }
    let clip = peak.map(|m| m * 0.9);

    let mut rg = Vec::new();
    let mut bg = Vec::new();
    for s in &det.stars {
        let r_ap = (1.5 * s.fwhm).clamp(2.0, 10.0);
        let (r_in, r_out) = (r_ap + 3.0, r_ap + 7.0);
        let (cx, cy) = (s.x, s.y);
        let x0 = (cx - r_out).floor();
        let y0 = (cy - r_out).floor();
        let x1 = (cx + r_out).ceil();
        let y1 = (cy + r_out).ceil();
        if x0 < 0.0 || y0 < 0.0 || x1 >= w as f32 || y1 >= h as f32 {
            continue;
        }
        let mut sum = [0f32; 3];
        let mut n_ap = 0usize;
        let mut ring: [Vec<f32>; 3] = Default::default();
        let mut bad = false;
        for y in y0 as usize..=y1 as usize {
            for x in x0 as usize..=x1 as usize {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let p = img.px(x, y);
                if d <= r_ap {
                    if p == [0.0; 3] || (0..3).any(|c| !p[c].is_finite() || p[c] >= clip[c]) {
                        bad = true;
                    }
                    for c in 0..3 {
                        sum[c] += p[c];
                    }
                    n_ap += 1;
                } else if d >= r_in && d <= r_out && p != [0.0; 3] {
                    for c in 0..3 {
                        ring[c].push(p[c]);
                    }
                }
            }
        }
        if bad || ring[0].len() < 16 {
            continue;
        }
        let flux: [f32; 3] = [0, 1, 2].map(|c| sum[c] - n_ap as f32 * median_in_place(&mut ring[c]));
        if flux.iter().all(|f| f.is_finite() && *f > 0.0) {
            rg.push(flux[1] / flux[0]);
            bg.push(flux[1] / flux[2]);
        }
    }
    if rg.len() < MIN_STARS {
        return None;
    }
    // The white balance slider's range.
    let limit = |v: f32| v.clamp(0.25, 4.0);
    Some([limit(median_in_place(&mut rg)), 1.0, limit(median_in_place(&mut bg))])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sky with Gaussian stars of mixed colour, seen through a sensor that
    /// records red at `gain[0]` and blue at `gain[2]` of green.
    fn field(gain: [f32; 3]) -> Image {
        let (w, h) = (400, 300);
        let mut img = Image::new(w, h);
        let mut s = 11u32;
        let mut rnd = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            (s >> 8) as f32 / (1 << 24) as f32
        };
        for px in img.data.chunks_exact_mut(3) {
            let n = (rnd() - 0.5) * 0.0004;
            for c in 0..3 {
                px[c] = (0.02 + n) * gain[c];
            }
        }
        let mut stars = Vec::new();
        for _ in 0..60 {
            let (x, y) = (20.0 + rnd() * (w as f32 - 40.0), 20.0 + rnd() * (h as f32 - 40.0));
            // Colours spread around white: warm and cool stars alike.
            let t = rnd() * 0.6 - 0.3;
            stars.push((x, y, 0.05 + rnd() * 0.2, [1.0 + t, 1.0, 1.0 - t]));
        }
        for (sx, sy, amp, col) in stars {
            for y in (sy as usize - 8)..(sy as usize + 8) {
                for x in (sx as usize - 8)..(sx as usize + 8) {
                    let d2 = (x as f32 + 0.5 - sx).powi(2) + (y as f32 + 0.5 - sy).powi(2);
                    let v = amp * (-d2 / (2.0 * 1.3f32.powi(2))).exp();
                    for c in 0..3 {
                        img.data[(y * w + x) * 3 + c] += v * col[c] * gain[c];
                    }
                }
            }
        }
        img
    }

    #[test]
    fn green_sensor_is_balanced() {
        let wb = star_white_balance(&field([0.4, 1.0, 0.7])).unwrap();
        assert!((wb[0] - 2.5).abs() < 0.15, "{wb:?}");
        assert_eq!(wb[1], 1.0);
        assert!((wb[2] - 1.0 / 0.7).abs() < 0.1, "{wb:?}");
    }

    #[test]
    fn balanced_image_is_left_alone() {
        let wb = star_white_balance(&field([1.0; 3])).unwrap();
        assert!(wb.iter().all(|v| (v - 1.0).abs() < 0.06), "{wb:?}");
    }

    #[test]
    fn starless_image_gives_nothing() {
        let mut img = Image::new(200, 200);
        img.data.fill(0.1);
        assert_eq!(star_white_balance(&img), None);
    }
}
