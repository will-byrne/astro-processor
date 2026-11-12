//! The full light-frame pipeline: calibrate, register, normalise, stack.

use crate::calibrate::{self, CalibrationUse, MasterOptions, Masters, master};
use crate::combine::{CombineOptions, combine};
use crate::debayer;
use crate::frames::{FrameKind, FrameSet, FrameStats};
use crate::image::{CfaPattern, Image, Mono, Rect, cfa_color, median_in_place};
use crate::io::raw;
use crate::progress::Progress;
use crate::register::{self, Affine, Detection, stars::DetectOptions, warp};
use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackMethod {
    Average,
    KappaSigma,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StackOptions {
    pub calibration: CalibrationUse,
    pub method: StackMethod,
    /// Rejection threshold in standard deviations (kappa-sigma only).
    pub kappa: f32,
    /// Rejection passes (kappa-sigma only).
    pub iterations: u32,
    /// Frame everything is aligned to; `None` picks the best-scoring frame.
    pub reference: Option<PathBuf>,
    /// Keep only this percentage of frames, ranked by quality score.
    pub keep_best_percent: f32,
    /// Isolated pixels this many sigma above their neighbours are repaired
    /// before stacking. 0 disables hot-pixel repair.
    pub hot_pixel_sigma: f32,
    /// Match each frame's sky background and transparency to the reference.
    pub normalize: bool,
    /// Reuse master calibration frames between runs.
    pub use_cache: bool,
}

impl Default for StackOptions {
    fn default() -> Self {
        Self {
            calibration: CalibrationUse::default(),
            method: StackMethod::KappaSigma,
            kappa: 2.5,
            iterations: 1,
            reference: None,
            keep_best_percent: 100.0,
            hot_pixel_sigma: 8.0,
            normalize: true,
            use_cache: true,
        }
    }
}

pub struct StackResult {
    /// Linear RGB, 1.0 = sensor white point. Uncovered pixels are 0.
    pub image: Image,
    /// As-shot white balance of the lights (R, G, B multipliers).
    pub wb: [f32; 3],
    pub reference: PathBuf,
    /// Per-light measurements, for every ticked light.
    pub stats: Vec<(PathBuf, FrameStats)>,
    pub frames_stacked: usize,
    /// Fraction of samples rejected by clipping.
    pub rejected: f64,
    /// Region covered by (nearly) all frames.
    pub crop: Rect,
    pub notes: Vec<String>,
}

struct Registered {
    detection: Detection,
    /// Sky background per colour in the calibrated frame.
    background: [f32; 3],
}

fn channel_backgrounds(cfa: &Mono, pattern: &CfaPattern) -> [f32; 3] {
    let mut samples: [Vec<f32>; 3] = Default::default();
    for y in (0..cfa.height.saturating_sub(1)).step_by(16) {
        for x in (0..cfa.width.saturating_sub(1)).step_by(16) {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                samples[cfa_color(pattern, x + dx, y + dy)].push(cfa.data[(y + dy) * cfa.width + x + dx]);
            }
        }
    }
    samples.map(|mut s| median_in_place(&mut s))
}

/// Load and calibrate one light. `Ok(None)` if it does not match the others.
fn prepare(
    path: &std::path::Path,
    dims: (usize, usize),
    pattern: &CfaPattern,
    masters: &Masters,
    hot_sigma: f32,
) -> Result<Option<Mono>> {
    let frame = raw::load_cfa(path)?;
    if (frame.mono.width, frame.mono.height) != dims || frame.pattern != *pattern {
        return Ok(None);
    }
    Ok(Some(calibrate::calibrate(frame, masters, hot_sigma).0))
}

/// Largest-ish rectangle whose edges are almost fully covered.
fn covered_rect(covered: &[bool], w: usize, h: usize) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (0usize, 0usize, w, h);
    let row = |y: usize, x0: usize, x1: usize| {
        covered[y * w + x0..y * w + x1].iter().filter(|c| **c).count() as f32 / (x1 - x0) as f32
    };
    let col = |x: usize, y0: usize, y1: usize| {
        (y0..y1).filter(|&y| covered[y * w + x]).count() as f32 / (y1 - y0) as f32
    };
    const OK: f32 = 0.995;
    while x1 - x0 > 2 && y1 - y0 > 2 {
        let edges = [row(y0, x0, x1), row(y1 - 1, x0, x1), col(x0, y0, y1), col(x1 - 1, y0, y1)];
        let (worst, &frac) =
            edges.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).expect("four edges");
        if frac >= OK {
            break;
        }
        match worst {
            0 => y0 += 1,
            1 => y1 -= 1,
            2 => x0 += 1,
            _ => x1 -= 1,
        }
    }
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

pub fn stack(set: &FrameSet, opts: &StackOptions, progress: &Progress) -> Result<StackResult> {
    let lights = set.enabled_paths(FrameKind::Light);
    if lights.is_empty() {
        bail!("no light frames are ticked");
    }
    let threads = rayon::current_num_threads().max(1);
    let mut notes = Vec::new();

    progress.stage("Reading first light frame", 1);
    let probe = raw::load_cfa(&lights[0])?;
    let dims = (probe.mono.width, probe.mono.height);
    let pattern = probe.pattern;
    let wb = probe.wb;
    drop(probe);

    let mopts = MasterOptions {
        batch: threads.min(8),
        hot_sigma: if opts.hot_pixel_sigma > 0.0 { 10.0 } else { 0.0 },
        cache_dir: if opts.use_cache { master::default_cache_dir() } else { None },
        ..MasterOptions::default()
    };
    let masters = master::build(set, opts.calibration, dims, &pattern, &mopts, progress)?;
    notes.extend(masters.notes.iter().cloned());

    // --- Register: find stars in every light -----------------------------
    progress.stage("Detecting stars", lights.len());
    let mut regs: Vec<Option<Registered>> = Vec::with_capacity(lights.len());
    for chunk in lights.chunks(threads) {
        progress.check()?;
        let part: Vec<Option<Registered>> = chunk
            .par_iter()
            .map(|path| {
                let r = match prepare(path, dims, &pattern, &masters, opts.hot_pixel_sigma) {
                    Ok(Some(cfa)) => {
                        let background = channel_backgrounds(&cfa, &pattern);
                        let detection = register::detect(&debayer::bin2(&cfa), &DetectOptions::default());
                        Some(Registered { detection, background })
                    }
                    Ok(None) => {
                        progress.log(format!("{}: skipped, different sensor layout", path.display()));
                        None
                    }
                    Err(e) => {
                        progress.log(format!("skipped, could not read: {e}"));
                        None
                    }
                };
                progress.tick();
                r
            })
            .collect();
        regs.extend(part);
    }

    // --- Choose the reference and rank frames ----------------------------
    let score = |i: usize| regs[i].as_ref().map(|r| r.detection.score()).unwrap_or(-1.0);
    let readable: Vec<usize> = (0..lights.len()).filter(|&i| regs[i].is_some()).collect();
    if readable.is_empty() {
        bail!("none of the light frames could be read");
    }
    let reference = opts
        .reference
        .as_ref()
        .and_then(|p| readable.iter().copied().find(|&i| &lights[i] == p))
        .unwrap_or_else(|| {
            readable.iter().copied().max_by(|&a, &b| score(a).total_cmp(&score(b))).expect("non-empty")
        });
    let ref_reg = regs[reference].as_ref().expect("reference is readable");
    progress.log(format!(
        "Reference frame: {} ({} stars, FWHM {:.2} px)",
        lights[reference].file_name().unwrap_or_default().to_string_lossy(),
        ref_reg.detection.total,
        ref_reg.detection.fwhm * 2.0
    ));

    let mut ranked = readable.clone();
    ranked.sort_by(|&a, &b| score(b).total_cmp(&score(a)));
    let keep_n = ((opts.keep_best_percent.clamp(1.0, 100.0) / 100.0) * ranked.len() as f32).ceil() as usize;
    let mut keep = vec![false; lights.len()];
    for &i in ranked.iter().take(keep_n.max(1)) {
        keep[i] = true;
    }
    keep[reference] = true;

    // --- Solve transforms -------------------------------------------------
    progress.stage("Aligning frames", lights.len());
    let matches: Vec<Option<(Affine, f32)>> = (0..lights.len())
        .into_par_iter()
        .map(|i| {
            progress.tick();
            let reg = regs[i].as_ref()?;
            if i == reference {
                return Some((Affine::IDENTITY, 1.0));
            }
            let m = register::find_transform(&ref_reg.detection.stars, &reg.detection.stars)?;
            Some((m.transform.binned_to_full(), m.flux_ratio))
        })
        .collect();

    let mut stats = Vec::with_capacity(lights.len());
    for i in 0..lights.len() {
        let s = match &regs[i] {
            Some(r) => FrameStats {
                stars: r.detection.total,
                fwhm: r.detection.fwhm * 2.0,
                background: r.detection.background,
                score: r.detection.score(),
                registered: matches[i].is_some(),
            },
            None => FrameStats::default(),
        };
        stats.push((lights[i].clone(), s));
    }
    let unmatched = readable.iter().filter(|&&i| matches[i].is_none()).count();
    if unmatched > 0 {
        let msg = format!("{unmatched} frame(s) could not be aligned to the reference and were left out.");
        progress.log(msg.clone());
        notes.push(msg);
    }
    let dropped = readable.iter().filter(|&&i| matches[i].is_some() && !keep[i]).count();
    if dropped > 0 {
        progress.log(format!("{dropped} lowest-scoring frame(s) left out (keeping best {:.0}%).", opts.keep_best_percent));
    }
    let use_: Vec<usize> = readable.iter().copied().filter(|&i| keep[i] && matches[i].is_some()).collect();

    // --- Stack --------------------------------------------------------------
    let (w, h) = dims;
    let ref_bg = ref_reg.background;
    let copts = CombineOptions {
        kappa: match opts.method {
            StackMethod::Average => None,
            StackMethod::KappaSigma => Some(opts.kappa.max(0.5)),
        },
        iterations: opts.iterations.clamp(1, 5),
        // Each in-flight frame needs roughly 0.7 GB at 24 MP.
        batch: threads.min(6),
    };
    let combined = combine(use_.len(), w * h * 3, &copts, progress, "Stacking", |k| {
        let i = use_[k];
        let Some(cfa) = prepare(&lights[i], dims, &pattern, &masters, opts.hot_pixel_sigma)? else {
            return Ok(None);
        };
        let mut rgb = debayer::bilinear(&cfa, &pattern);
        drop(cfa);
        let (transform, flux_ratio) = matches[i].expect("only matched frames are stacked");
        if opts.normalize {
            let bg = regs[i].as_ref().expect("readable").background;
            let gain = if flux_ratio.is_finite() { flux_ratio.clamp(0.25, 4.0) } else { 1.0 };
            rgb.data.par_chunks_exact_mut(3).for_each(|p| {
                for c in 0..3 {
                    p[c] = (p[c] - bg[c]) * gain + ref_bg[c];
                }
            });
        }
        Ok(Some(warp::warp(&rgb, &transform, w, h).data))
    })?;

    progress.stage("Finishing", 1);
    let mut image = Image { width: w, height: h, data: combined.mean };
    image.data.par_iter_mut().for_each(|v| {
        if !v.is_finite() {
            *v = 0.0;
        }
    });
    // "Covered" means nearly every frame contributes; thinly covered edges
    // are noisier and sit at a slightly different level.
    let need = ((combined.frames as f32) * 0.9).ceil().max(1.0) as u16;
    let covered: Vec<bool> = combined.count.par_chunks_exact(3).map(|c| c[1] >= need).collect();
    let crop = covered_rect(&covered, w, h);
    progress.log(format!(
        "Stacked {} of {} frames; {:.2}% of samples rejected.",
        combined.frames,
        lights.len(),
        combined.rejected * 100.0
    ));
    progress.tick();

    Ok(StackResult {
        image,
        wb,
        reference: lights[reference].clone(),
        stats,
        frames_stacked: combined.frames,
        rejected: combined.rejected,
        crop,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_rect_trims_ragged_edges() {
        let (w, h) = (40, 30);
        let mut c = vec![true; w * h];
        for y in 0..h {
            for x in 0..w {
                // A slanted uncovered wedge on the left and a strip on top.
                if x < 3 + y / 10 || y < 4 {
                    c[y * w + x] = false;
                }
            }
        }
        let r = covered_rect(&c, w, h);
        assert_eq!(r, Rect { x: 5, y: 4, w: 35, h: 26 });
        assert_eq!(covered_rect(&[true; 12], 4, 3), Rect { x: 0, y: 0, w: 4, h: 3 });
    }

    #[test]
    fn backgrounds_are_measured_per_colour() {
        let pattern: CfaPattern = [0, 1, 1, 2];
        let mut cfa = Mono::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                cfa.data[y * 64 + x] = [0.01, 0.02, 0.03][cfa_color(&pattern, x, y)];
            }
        }
        assert_eq!(channel_backgrounds(&cfa, &pattern), [0.01, 0.02, 0.03]);
    }
}
