//! Streaming frame combination: mean or kappa-sigma clipped mean over frames
//! that are produced on demand, so memory use is independent of frame count.

use crate::progress::Progress;
use anyhow::{Result, bail};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug)]
pub struct CombineOptions {
    /// Reject samples further than `kappa` standard deviations from the mean.
    /// `None` gives a plain average.
    pub kappa: Option<f32>,
    /// Number of rejection passes (each one re-reads every frame).
    pub iterations: u32,
    /// Frames loaded concurrently.
    pub batch: usize,
}

pub struct Combined {
    pub mean: Vec<f32>,
    /// Samples that contributed to each output value.
    pub count: Vec<u16>,
    /// Fraction of samples discarded by clipping.
    pub rejected: f64,
    /// Frames that were actually combined (loader returned data).
    pub frames: usize,
}

const CHUNK: usize = 1 << 16;

/// Combine `n` frames of `len` samples each. `load(i)` produces frame `i`
/// (`Ok(None)` skips it). NaN samples are treated as missing.
pub fn combine<F>(
    n: usize,
    len: usize,
    opts: &CombineOptions,
    progress: &Progress,
    stage: &str,
    load: F,
) -> Result<Combined>
where
    F: Fn(usize) -> Result<Option<Vec<f32>>> + Sync,
{
    let clip_passes = match opts.kappa {
        Some(_) if n >= 3 => opts.iterations.max(1) as usize,
        _ => 0,
    };
    let passes = 1 + clip_passes;
    let batch = opts.batch.max(1);
    let indices: Vec<usize> = (0..n).collect();

    let mut prev: Option<(Vec<f32>, Vec<f32>)> = None;
    let mut first_total = 0u64;
    let mut result = None;

    for pass in 0..passes {
        if passes > 1 {
            progress.stage(&format!("{stage} (pass {} of {passes})", pass + 1), n);
        } else {
            progress.stage(stage, n);
        }
        let mut sum = vec![0f32; len];
        let mut sumsq = vec![0f32; len];
        let mut count = vec![0u16; len];
        let mut frames = 0usize;

        for chunk in indices.chunks(batch) {
            progress.check()?;
            let loaded: Vec<Result<Option<Vec<f32>>>> = chunk
                .par_iter()
                .map(|&i| {
                    let r = load(i);
                    progress.tick();
                    r
                })
                .collect();
            for f in loaded {
                let Some(f) = f? else { continue };
                if f.len() != len {
                    bail!("frame has {} samples, expected {len}", f.len());
                }
                frames += 1;
                match (&prev, opts.kappa) {
                    (Some((mean, sigma)), Some(kappa)) => {
                        (
                            sum.par_chunks_mut(CHUNK),
                            sumsq.par_chunks_mut(CHUNK),
                            count.par_chunks_mut(CHUNK),
                            f.par_chunks(CHUNK),
                            mean.par_chunks(CHUNK),
                            sigma.par_chunks(CHUNK),
                        )
                            .into_par_iter()
                            .for_each(|(s, q, c, v, m, sd)| {
                                for i in 0..v.len() {
                                    let x = v[i];
                                    // NaN fails the comparison and is skipped.
                                    if (x - m[i]).abs() <= kappa * sd[i] + 1e-6 * m[i].abs() + 1e-12 {
                                        s[i] += x;
                                        q[i] += x * x;
                                        c[i] += 1;
                                    }
                                }
                            });
                    }
                    _ => {
                        (
                            sum.par_chunks_mut(CHUNK),
                            sumsq.par_chunks_mut(CHUNK),
                            count.par_chunks_mut(CHUNK),
                            f.par_chunks(CHUNK),
                        )
                            .into_par_iter()
                            .for_each(|(s, q, c, v)| {
                                for i in 0..v.len() {
                                    let x = v[i];
                                    if x.is_finite() {
                                        s[i] += x;
                                        q[i] += x * x;
                                        c[i] += 1;
                                    }
                                }
                            });
                    }
                }
            }
        }
        if frames == 0 {
            bail!("no frames could be loaded");
        }

        let total: u64 = count.par_iter().map(|&c| c as u64).sum();
        if pass == 0 {
            first_total = total;
        }
        // Turn the sums into mean and sigma in place.
        let prev_mean = prev.as_ref().map(|(m, _)| m.as_slice());
        (sum.par_iter_mut(), sumsq.par_iter_mut(), count.par_iter())
            .into_par_iter()
            .enumerate()
            .for_each(|(i, (s, q, &c))| {
                if c == 0 {
                    // Everything was rejected (or never covered): keep the
                    // previous estimate rather than leaving a hole.
                    *s = prev_mean.map(|m| m[i]).unwrap_or(f32::NAN);
                    *q = 0.0;
                } else {
                    let n = c as f32;
                    let mean = *s / n;
                    *q = (*q / n - mean * mean).max(0.0).sqrt();
                    *s = mean;
                }
            });
        let rejected = if first_total > 0 { 1.0 - total as f64 / first_total as f64 } else { 0.0 };
        if pass + 1 == passes {
            result = Some(Combined { mean: sum, count, rejected, frames });
        } else {
            prev = Some((sum, sumsq));
        }
    }
    Ok(result.expect("at least one pass"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<Vec<f32>> {
        // 20 frames of 4 samples; deterministic small noise around 10.
        (0..20)
            .map(|i| (0..4).map(|j| 10.0 + ((i * 7 + j * 3) % 5) as f32 * 0.1 - 0.2).collect())
            .collect()
    }

    #[test]
    fn plain_mean() {
        let fr = frames();
        let opts = CombineOptions { kappa: None, iterations: 1, batch: 3 };
        let c = combine(fr.len(), 4, &opts, &Progress::new(), "t", |i| Ok(Some(fr[i].clone()))).unwrap();
        let expect: f32 = fr.iter().map(|f| f[0]).sum::<f32>() / 20.0;
        assert!((c.mean[0] - expect).abs() < 1e-4);
        assert_eq!(c.count, vec![20; 4]);
        assert_eq!(c.rejected, 0.0);
    }

    #[test]
    fn clipping_removes_a_satellite_trail() {
        let mut fr = frames();
        let clean: f32 = fr.iter().enumerate().filter(|(i, _)| *i != 5).map(|(_, f)| f[2]).sum::<f32>() / 19.0;
        fr[5][2] = 500.0; // bright outlier in one frame
        let avg = CombineOptions { kappa: None, iterations: 1, batch: 4 };
        let c = combine(fr.len(), 4, &avg, &Progress::new(), "t", |i| Ok(Some(fr[i].clone()))).unwrap();
        assert!(c.mean[2] > 30.0, "plain average is contaminated");
        let clip = CombineOptions { kappa: Some(3.0), iterations: 1, batch: 4 };
        let c = combine(fr.len(), 4, &clip, &Progress::new(), "t", |i| Ok(Some(fr[i].clone()))).unwrap();
        assert!((c.mean[2] - clean).abs() < 1e-3, "{} vs {clean}", c.mean[2]);
        assert_eq!(c.count[2], 19);
        assert!(c.rejected > 0.0);
    }

    #[test]
    fn nan_marks_missing_samples_and_frames_can_be_skipped() {
        let fr = [vec![1.0, f32::NAN], vec![3.0, 4.0], vec![100.0, 100.0]];
        let opts = CombineOptions { kappa: None, iterations: 1, batch: 2 };
        let c = combine(3, 2, &opts, &Progress::new(), "t", |i| {
            Ok(if i == 2 { None } else { Some(fr[i].clone()) })
        })
        .unwrap();
        assert_eq!(c.mean, vec![2.0, 4.0]);
        assert_eq!(c.count, vec![2, 1]);
        assert_eq!(c.frames, 2);
    }

    #[test]
    fn cancellation_stops_the_run() {
        let p = Progress::new();
        p.cancel();
        let opts = CombineOptions { kappa: None, iterations: 1, batch: 2 };
        assert!(combine(2, 1, &opts, &p, "t", |_| Ok(Some(vec![0.0]))).is_err());
    }
}
