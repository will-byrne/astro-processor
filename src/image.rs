//! Basic in-memory image types used throughout the pipeline.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Bayer colour-filter layout: colour index (0 = R, 1 = G, 2 = B) at
/// `[y % 2][x % 2]`, flattened row-major.
pub type CfaPattern = [u8; 4];

#[inline]
pub fn cfa_color(pattern: &CfaPattern, x: usize, y: usize) -> usize {
    pattern[(y & 1) * 2 + (x & 1)] as usize
}

/// Single-channel f32 image (raw Bayer mosaic, luminance, masks...).
#[derive(Clone, Debug, PartialEq)]
pub struct Mono {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Mono {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, data: vec![0.0; width * height] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }
}

/// Interleaved RGB f32 image. Values are linear with 1.0 = sensor white point
/// until the processing pipeline stretches them into display range.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Image {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, data: vec![0.0; width * height * 3] }
    }

    #[inline]
    pub fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// Box-average downsample by an integer factor (1 returns a clone).
    /// Non-finite samples (uncovered stack pixels) are ignored.
    pub fn downsample(&self, factor: usize) -> Image {
        if factor <= 1 {
            return self.clone();
        }
        let w = (self.width / factor).max(1);
        let h = (self.height / factor).max(1);
        let mut out = Image::new(w, h);
        out.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let mut acc = [0.0f32; 3];
                let mut n = 0u32;
                for sy in y * factor..((y + 1) * factor).min(self.height) {
                    for sx in x * factor..((x + 1) * factor).min(self.width) {
                        let p = self.px(sx, sy);
                        if p[0].is_finite() {
                            acc[0] += p[0];
                            acc[1] += p[1];
                            acc[2] += p[2];
                            n += 1;
                        }
                    }
                }
                let inv = if n > 0 { 1.0 / n as f32 } else { 0.0 };
                row[x * 3] = acc[0] * inv;
                row[x * 3 + 1] = acc[1] * inv;
                row[x * 3 + 2] = acc[2] * inv;
            }
        });
        out
    }

    /// Copy out a rectangle (clamped to the image bounds).
    pub fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> Image {
        let x0 = x0.min(self.width.saturating_sub(1));
        let y0 = y0.min(self.height.saturating_sub(1));
        let w = w.min(self.width - x0).max(1);
        let h = h.min(self.height - y0).max(1);
        let mut out = Image::new(w, h);
        for y in 0..h {
            let s = ((y0 + y) * self.width + x0) * 3;
            out.data[y * w * 3..(y + 1) * w * 3].copy_from_slice(&self.data[s..s + w * 3]);
        }
        out
    }

    /// Rec.709 luminance plane.
    pub fn luminance(&self) -> Mono {
        let mut out = Mono::new(self.width, self.height);
        out.data
            .par_iter_mut()
            .zip(self.data.par_chunks_exact(3))
            .for_each(|(l, p)| *l = luma(p[0], p[1], p[2]));
        out
    }
}

#[inline]
pub fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Integer pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Median of a slice (reorders it). Returns 0 for an empty slice.
pub fn median_in_place(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    *m
}

/// Median and robust sigma (1.4826 * MAD) of a slice (reorders it).
pub fn median_sigma(v: &mut [f32]) -> (f32, f32) {
    let med = median_in_place(v);
    for x in v.iter_mut() {
        *x = (*x - med).abs();
    }
    let mad = median_in_place(v);
    (med, 1.4826 * mad)
}
