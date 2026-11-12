//! Apply master frames to a light frame.

use super::hotpixel;
use super::master::Masters;
use crate::image::Mono;
use crate::io::raw::CfaFrame;
use rayon::prelude::*;

/// Calibrate a raw light frame: subtract the dark (or bias, or the camera's
/// black level), divide by the flat, repair hot pixels, and scale so that 1.0
/// is the sensor's white point. Values may be slightly negative (noise around
/// black); they are kept so the stack's mean stays unbiased.
///
/// `outlier_sigma` > 0 additionally repairs isolated bright pixels not listed
/// in the dark's hot-pixel map. Returns the frame and the number of pixels
/// repaired.
pub fn calibrate(frame: CfaFrame, masters: &Masters, outlier_sigma: f32) -> (Mono, usize) {
    let CfaFrame { mut mono, black, white, .. } = frame;
    let w = mono.width;
    let same_size = |m: &&Mono| (m.width, m.height) == (mono.width, mono.height);
    let offset = masters.dark.as_ref().filter(same_size).or(masters.bias.as_ref().filter(same_size));
    let flat = masters.flat.as_ref().filter(same_size);
    let mean_black = black.iter().sum::<f32>() / 4.0;
    let scale = 1.0 / (white - mean_black).max(1.0);

    mono.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let o = offset.map(|m| &m.data[y * w..(y + 1) * w]);
        let f = flat.map(|m| &m.data[y * w..(y + 1) * w]);
        for (x, v) in row.iter_mut().enumerate() {
            let sub = match o {
                Some(o) => o[x],
                None => black[(y & 1) * 2 + (x & 1)],
            };
            let mut val = (*v - sub) * scale;
            if let Some(f) = f {
                val /= f[x];
            }
            *v = val;
        }
    });

    let mut repaired = 0;
    if masters.dark.as_ref().filter(|m| (m.width, m.height) == (mono.width, mono.height)).is_some()
        && !masters.hot.is_empty()
    {
        hotpixel::repair_listed(&mut mono, &masters.hot);
        repaired += masters.hot.len();
    }
    if outlier_sigma > 0.0 {
        repaired += hotpixel::repair_outliers(&mut mono, outlier_sigma);
    }
    (mono, repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: Vec<f32>) -> CfaFrame {
        CfaFrame {
            mono: Mono { width: 4, height: 2, data },
            pattern: [0, 1, 1, 2],
            black: [100.0; 4],
            white: 1100.0,
            wb: [2.0, 1.0, 1.5],
        }
    }

    #[test]
    fn black_level_is_used_without_masters() {
        let (m, _) = calibrate(frame(vec![100.0, 600.0, 1100.0, 90.0, 100.0, 100.0, 100.0, 100.0]), &Masters::default(), 0.0);
        for (got, want) in m.data[..4].iter().zip([0.0, 0.5, 1.0, -0.01]) {
            assert!((got - want).abs() < 1e-6);
        }
    }

    #[test]
    fn dark_then_flat() {
        let masters = Masters {
            dark: Some(Mono { width: 4, height: 2, data: vec![120.0; 8] }),
            bias: Some(Mono { width: 4, height: 2, data: vec![999.0; 8] }), // ignored: dark wins
            flat: Some(Mono { width: 4, height: 2, data: vec![1.0, 0.5, 1.0, 2.0, 1.0, 1.0, 1.0, 1.0] }),
            ..Default::default()
        };
        let (m, _) = calibrate(frame(vec![620.0; 8]), &masters, 0.0);
        // (620 - 120) / 1000 = 0.5, then divided by the flat.
        assert_eq!(&m.data[..4], &[0.5, 1.0, 0.5, 0.25]);
    }

    #[test]
    fn wrong_size_masters_are_ignored() {
        let masters = Masters {
            dark: Some(Mono { width: 2, height: 2, data: vec![500.0; 4] }),
            ..Default::default()
        };
        let (m, _) = calibrate(frame(vec![600.0; 8]), &masters, 0.0);
        assert_eq!(m.data[0], 0.5);
    }
}
