//! Master calibration frames, built with a clipped mean and cached on disk.

use super::hotpixel;
use crate::combine::{CombineOptions, combine};
use crate::frames::{FrameKind, FrameSet};
use crate::image::{CfaPattern, Mono, cfa_color, median_in_place};
use crate::io::fits::{self, FitsImage};
use crate::io::raw;
use crate::progress::Progress;
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Which calibration sets to apply (each also needs ticked frames).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationUse {
    pub darks: bool,
    pub flats: bool,
    pub dark_flats: bool,
    pub bias: bool,
}

impl Default for CalibrationUse {
    fn default() -> Self {
        Self { darks: true, flats: true, dark_flats: true, bias: true }
    }
}

#[derive(Clone, Debug)]
pub struct MasterOptions {
    pub kappa: f32,
    pub batch: usize,
    /// Pixels this many sigma above the master dark's typical level are
    /// treated as hot. 0 disables the map.
    pub hot_sigma: f32,
    /// Directory for cached masters; `None` disables caching.
    pub cache_dir: Option<PathBuf>,
}

impl Default for MasterOptions {
    fn default() -> Self {
        Self { kappa: 3.0, batch: 8, hot_sigma: 10.0, cache_dir: default_cache_dir() }
    }
}

pub fn default_cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))?;
    Some(base.join("astro-processor").join("masters"))
}

/// Everything needed to calibrate a light frame. All frames are in raw
/// sensor units.
#[derive(Clone, Debug, Default)]
pub struct Masters {
    /// Master dark (still contains the offset/black level).
    pub dark: Option<Mono>,
    /// Master bias; subtracted from lights only when there is no dark.
    pub bias: Option<Mono>,
    /// Master flat, normalised to a mean of 1 for each Bayer colour.
    pub flat: Option<Mono>,
    /// Indices of hot pixels found in the master dark.
    pub hot: Vec<u32>,
    /// Things the user should know (skipped sets, clipped flats...).
    pub notes: Vec<String>,
}

/// FNV-1a, so cache keys are stable across Rust versions.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
}

fn cache_key(tag: &str, paths: &[PathBuf], kappa: f32, extra: u64) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    fnv(&mut h, b"astro-master-v1");
    fnv(&mut h, tag.as_bytes());
    fnv(&mut h, &kappa.to_bits().to_le_bytes());
    fnv(&mut h, &extra.to_le_bytes());
    for p in paths {
        fnv(&mut h, p.to_string_lossy().as_bytes());
        if let Ok(md) = std::fs::metadata(p) {
            fnv(&mut h, &md.len().to_le_bytes());
            if let Ok(t) = md.modified().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)
            }) {
                fnv(&mut h, &t.as_secs().to_le_bytes());
            }
        }
    }
    h
}

fn cache_load(dir: &Option<PathBuf>, tag: &str, key: u64, dims: (usize, usize)) -> Option<Mono> {
    let path = dir.as_ref()?.join(format!("{tag}-{key:016x}.fits"));
    let f = fits::read(&path).ok()?;
    (f.channels == 1 && (f.width, f.height) == dims)
        .then_some(Mono { width: f.width, height: f.height, data: f.data })
}

fn cache_store(dir: &Option<PathBuf>, tag: &str, key: u64, m: &Mono) {
    let Some(dir) = dir else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let img = FitsImage {
        width: m.width,
        height: m.height,
        channels: 1,
        bitpix: -32,
        data: m.data.clone(),
        cards: vec![("MASTER".into(), tag.into())],
    };
    // A failed cache write only costs time on the next run.
    let _ = fits::write(&dir.join(format!("{tag}-{key:016x}.fits")), &img);
}

struct Builder<'a> {
    dims: (usize, usize),
    opts: &'a MasterOptions,
    progress: &'a Progress,
}

impl Builder<'_> {
    /// Clipped mean of the frames after `prep` has been applied to each.
    /// Frames whose size differs from the lights are skipped.
    fn build(
        &self,
        tag: &str,
        label: &str,
        paths: &[PathBuf],
        extra_key: u64,
        prep: impl Fn(&mut raw::CfaFrame) + Sync,
    ) -> Result<Option<(Mono, u64)>> {
        if paths.is_empty() {
            return Ok(None);
        }
        let key = cache_key(tag, paths, self.opts.kappa, extra_key);
        if let Some(m) = cache_load(&self.opts.cache_dir, tag, key, self.dims) {
            self.progress.log(format!("{label}: using cached master ({} frames)", paths.len()));
            return Ok(Some((m, key)));
        }
        let (w, h) = self.dims;
        let wrong_size = Mutex::new(0usize);
        let opts = CombineOptions { kappa: Some(self.opts.kappa), iterations: 1, batch: self.opts.batch };
        let stage = format!("Building {label}");
        let combined = combine(paths.len(), w * h, &opts, self.progress, &stage, |i| {
            let mut f = raw::load_cfa(&paths[i])?;
            if (f.mono.width, f.mono.height) != (w, h) {
                *wrong_size.lock().unwrap() += 1;
                return Ok(None);
            }
            prep(&mut f);
            Ok(Some(f.mono.data))
        });
        let combined = match combined {
            Ok(c) => c,
            Err(e) if self.progress.is_cancelled() => return Err(e),
            Err(e) => {
                self.progress.log(format!("{label}: skipped ({e})"));
                return Ok(None);
            }
        };
        let skipped = *wrong_size.lock().unwrap();
        if skipped > 0 {
            self.progress.log(format!("{label}: {skipped} frame(s) skipped, wrong image size"));
        }
        self.progress.log(format!(
            "{label}: combined {} frames, {:.2}% of samples rejected",
            combined.frames,
            combined.rejected * 100.0
        ));
        let m = Mono { width: w, height: h, data: combined.mean };
        cache_store(&self.opts.cache_dir, tag, key, &m);
        Ok(Some((m, key)))
    }
}

/// Median of a sparse sample of the frame.
fn sample_median(m: &Mono) -> f32 {
    let mut s: Vec<f32> = m.data.iter().step_by(97).copied().collect();
    median_in_place(&mut s)
}

/// Scale a flat so each Bayer colour averages 1, and guard against division
/// by near-zero in dead corners.
fn normalise_flat(flat: &mut Mono, pattern: &CfaPattern) {
    let w = flat.width;
    let (sum, n) = flat
        .data
        .par_chunks(w)
        .enumerate()
        .map(|(y, row)| {
            let mut s = [0f64; 3];
            let mut n = [0f64; 3];
            for (x, &v) in row.iter().enumerate() {
                let c = cfa_color(pattern, x, y);
                s[c] += v as f64;
                n[c] += 1.0;
            }
            (s, n)
        })
        .reduce(
            || ([0f64; 3], [0f64; 3]),
            |a, b| {
                (
                    [a.0[0] + b.0[0], a.0[1] + b.0[1], a.0[2] + b.0[2]],
                    [a.1[0] + b.1[0], a.1[1] + b.1[1], a.1[2] + b.1[2]],
                )
            },
        );
    let mean: Vec<f32> = (0..3).map(|c| (sum[c] / n[c].max(1.0)).max(1e-6) as f32).collect();
    flat.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = (*v / mean[cfa_color(pattern, x, y)]).max(0.05);
        }
    });
}

/// Build (or load from cache) every master the frame set and toggles call for.
/// `dims`/`pattern` describe the light frames.
pub fn build(
    set: &FrameSet,
    use_: CalibrationUse,
    dims: (usize, usize),
    pattern: &CfaPattern,
    opts: &MasterOptions,
    progress: &Progress,
) -> Result<Masters> {
    let b = Builder { dims, opts, progress };
    let mut out = Masters::default();
    let paths = |kind: FrameKind, on: bool| if on { set.enabled_paths(kind) } else { Vec::new() };

    let bias = b.build("bias", "master bias", &paths(FrameKind::Bias, use_.bias), 0, |_| {})?;
    let dark = b.build("dark", "master dark", &paths(FrameKind::Dark, use_.darks), 0, |_| {})?;

    let flat_paths = paths(FrameKind::Flat, use_.flats);
    if !flat_paths.is_empty() {
        let dark_flat = b.build(
            "darkflat",
            "master dark-flat",
            &paths(FrameKind::DarkFlat, use_.dark_flats),
            0,
            |_| {},
        )?;
        // Offset to remove from each flat: dark-flat, else bias, else the
        // camera's nominal black level.
        let offset: Option<(&Mono, u64)> = dark_flat
            .as_ref()
            .map(|(m, k)| (m, *k))
            .or(bias.as_ref().map(|(m, k)| (m, *k)));
        let clipped = Mutex::new(0f32);
        let flat = b.build("flat", "master flat", &flat_paths, offset.map(|(_, k)| k).unwrap_or(1), |f| {
            let w = f.mono.width;
            let white = f.white;
            let n_clip = f.mono.data.iter().step_by(31).filter(|&&v| v >= 0.98 * white).count();
            let frac = n_clip as f32 / (f.mono.data.len() / 31).max(1) as f32;
            {
                let mut c = clipped.lock().unwrap();
                *c = c.max(frac);
            }
            match offset {
                Some((o, _)) => {
                    f.mono.data.par_iter_mut().zip(o.data.par_iter()).for_each(|(v, o)| *v -= o)
                }
                None => {
                    let black = f.black;
                    f.mono.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                        for (x, v) in row.iter_mut().enumerate() {
                            *v -= black[(y & 1) * 2 + (x & 1)];
                        }
                    });
                }
            }
            // Equalise brightness between flats (sky flats drift) so the
            // clipped mean compares like with like.
            let med = sample_median(&f.mono).max(1e-6);
            f.mono.data.par_iter_mut().for_each(|v| *v /= med);
        })?;
        let frac = *clipped.lock().unwrap();
        if frac > 0.01 {
            let msg = format!(
                "Flats are overexposed: up to {:.0}% of pixels are clipped, so flat correction will be wrong in those areas.",
                frac * 100.0
            );
            progress.log(msg.clone());
            out.notes.push(msg);
        }
        if let Some((mut flat, _)) = flat {
            normalise_flat(&mut flat, pattern);
            out.flat = Some(flat);
        }
    }

    if let Some((dark, _)) = dark {
        if opts.hot_sigma > 0.0 {
            out.hot = hotpixel::find_in_dark(&dark, opts.hot_sigma);
            progress.log(format!(
                "master dark: {} hot pixels ({:.3}%)",
                out.hot.len(),
                100.0 * out.hot.len() as f64 / dark.data.len() as f64
            ));
        }
        out.dark = Some(dark);
    }
    out.bias = bias.map(|(m, _)| m);
    Ok(out)
}

/// Remove every cached master. Returns the number of files deleted.
pub fn clear_cache(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "fits"))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_normalisation_is_per_colour() {
        let pattern: CfaPattern = [0, 1, 1, 2];
        let mut flat = Mono::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                // Red twice as bright as green, blue half; plus vignetting in x.
                let gain = [2.0, 1.0, 0.5][cfa_color(&pattern, x, y)];
                flat.data[y * 8 + x] = gain * (1.0 - 0.05 * (x / 2) as f32);
            }
        }
        normalise_flat(&mut flat, &pattern);
        for c in 0..3 {
            let v: Vec<f32> = (0..64)
                .filter(|i| cfa_color(&pattern, i % 8, i / 8) == c)
                .map(|i| flat.data[i])
                .collect();
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            assert!((mean - 1.0).abs() < 1e-5);
        }
        // Vignetting shape survives: left edge brighter than right edge.
        assert!(flat.data[0] > flat.data[6]);
    }

    #[test]
    fn cache_key_depends_on_inputs() {
        let a = cache_key("dark", &[PathBuf::from("/nonexistent/a")], 3.0, 0);
        assert_eq!(a, cache_key("dark", &[PathBuf::from("/nonexistent/a")], 3.0, 0));
        assert_ne!(a, cache_key("dark", &[PathBuf::from("/nonexistent/b")], 3.0, 0));
        assert_ne!(a, cache_key("flat", &[PathBuf::from("/nonexistent/a")], 3.0, 0));
        assert_ne!(a, cache_key("dark", &[PathBuf::from("/nonexistent/a")], 2.5, 0));
        assert_ne!(a, cache_key("dark", &[PathBuf::from("/nonexistent/a")], 3.0, 7));
    }
}
