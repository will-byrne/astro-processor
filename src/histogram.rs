//! Per-channel histograms of display-range images.

use crate::image::Image;
use rayon::prelude::*;

pub const BINS: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    /// Counts for R, G, B.
    pub channels: [Vec<u32>; 3],
    pub pixels: u64,
}

impl Histogram {
    pub fn of(img: &Image) -> Histogram {
        let rows = img.data.par_chunks(img.width.max(1) * 3).map(|row| {
            let mut h = [[0u32; BINS]; 3];
            for p in row.chunks_exact(3) {
                for c in 0..3 {
                    h[c][((p[c].clamp(0.0, 1.0) * (BINS - 1) as f32) + 0.5) as usize] += 1;
                }
            }
            h
        });
        let total = rows.reduce(
            || [[0u32; BINS]; 3],
            |mut a, b| {
                for c in 0..3 {
                    for i in 0..BINS {
                        a[c][i] += b[c][i];
                    }
                }
                a
            },
        );
        Histogram { channels: total.map(|c| c.to_vec()), pixels: (img.width * img.height) as u64 }
    }

    /// Largest bin count, ignoring the first and last bins (clipped pixels
    /// would otherwise flatten the rest of the plot).
    pub fn peak(&self) -> u32 {
        self.channels.iter().flat_map(|c| c[1..BINS - 1].iter().copied()).max().unwrap_or(0).max(1)
    }

    /// Fraction of samples in the lowest / highest bin.
    pub fn clipped(&self) -> (f32, f32) {
        let n = (self.pixels * 3).max(1) as f32;
        let lo: u32 = self.channels.iter().map(|c| c[0]).sum();
        let hi: u32 = self.channels.iter().map(|c| c[BINS - 1]).sum();
        (lo as f32 / n, hi as f32 / n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_land_in_the_right_bins() {
        let mut img = Image::new(2, 2);
        img.data.copy_from_slice(&[0.0, 0.5, 1.0, 0.0, 0.5, 1.0, 0.0, 0.5, 2.0, -1.0, 0.5, 1.0]);
        let h = Histogram::of(&img);
        assert_eq!(h.channels[0][0], 4);
        assert_eq!(h.channels[1][128], 4);
        assert_eq!(h.channels[2][255], 4);
        assert_eq!(h.pixels, 4);
        assert_eq!(h.peak(), 4);
        let (lo, hi) = h.clipped();
        assert!((lo - 1.0 / 3.0).abs() < 1e-6 && (hi - 1.0 / 3.0).abs() < 1e-6);
    }
}
