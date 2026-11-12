//! Frame bookkeeping: which files are in which slot, their capture settings,
//! and consistency checks between lights and calibration frames.

use crate::io::raw::{self, RawMeta};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameKind {
    Light,
    Dark,
    Flat,
    DarkFlat,
    Bias,
}

impl FrameKind {
    pub const ALL: [FrameKind; 5] =
        [FrameKind::Light, FrameKind::Dark, FrameKind::Flat, FrameKind::DarkFlat, FrameKind::Bias];

    pub fn label(self) -> &'static str {
        match self {
            FrameKind::Light => "Lights",
            FrameKind::Dark => "Darks",
            FrameKind::Flat => "Flats",
            FrameKind::DarkFlat => "Dark-flats",
            FrameKind::Bias => "Bias",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            FrameKind::Light => "The long exposures of your target.",
            FrameKind::Dark => "Lens covered, same exposure and ISO as the lights.",
            FrameKind::Flat => "Evenly lit frames; correct vignetting and dust.",
            FrameKind::DarkFlat => "Lens covered, same exposure and ISO as the flats.",
            FrameKind::Bias => "Lens covered, fastest shutter speed.",
        }
    }
}

/// Measurements made during registration (lights only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameStats {
    pub stars: usize,
    pub fwhm: f32,
    pub background: f32,
    pub score: f32,
    pub registered: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameInfo {
    pub path: PathBuf,
    pub meta: RawMeta,
    pub enabled: bool,
    /// Why the frame was flagged (differs from the rest of its set), if it was.
    pub note: Option<String>,
    #[serde(default)]
    pub stats: Option<FrameStats>,
}

impl FrameInfo {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameSet {
    pub lights: Vec<FrameInfo>,
    pub darks: Vec<FrameInfo>,
    pub flats: Vec<FrameInfo>,
    pub dark_flats: Vec<FrameInfo>,
    pub bias: Vec<FrameInfo>,
}

impl FrameSet {
    pub fn get(&self, kind: FrameKind) -> &Vec<FrameInfo> {
        match kind {
            FrameKind::Light => &self.lights,
            FrameKind::Dark => &self.darks,
            FrameKind::Flat => &self.flats,
            FrameKind::DarkFlat => &self.dark_flats,
            FrameKind::Bias => &self.bias,
        }
    }

    pub fn get_mut(&mut self, kind: FrameKind) -> &mut Vec<FrameInfo> {
        match kind {
            FrameKind::Light => &mut self.lights,
            FrameKind::Dark => &mut self.darks,
            FrameKind::Flat => &mut self.flats,
            FrameKind::DarkFlat => &mut self.dark_flats,
            FrameKind::Bias => &mut self.bias,
        }
    }

    /// Paths of the ticked frames in a slot.
    pub fn enabled_paths(&self, kind: FrameKind) -> Vec<PathBuf> {
        self.get(kind).iter().filter(|f| f.enabled).map(|f| f.path.clone()).collect()
    }

    /// Add already-scanned frames to a slot, skipping duplicates, then flag
    /// (and untick, for the new ones) frames that differ from the majority.
    /// Returns the number actually added.
    pub fn add(&mut self, kind: FrameKind, scanned: Vec<FrameInfo>) -> usize {
        let set = self.get_mut(kind);
        let first_new = set.len();
        for f in scanned {
            if !set.iter().any(|e| e.path == f.path) {
                set.push(f);
            }
        }
        let added = set.len() - first_new;
        flag_outliers(set, first_new);
        added
    }
}

/// Expand folders (non-recursively) and keep only RAW files, sorted by name.
pub fn collect_raw_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                out.extend(rd.flatten().map(|e| e.path()).filter(|f| f.is_file() && raw::is_raw_file(f)));
            }
        } else if raw::is_raw_file(p) {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Read metadata for every RAW file under `paths`. Unreadable files are
/// returned separately with the reason.
pub fn scan(paths: &[PathBuf]) -> (Vec<FrameInfo>, Vec<(PathBuf, String)>) {
    let files = collect_raw_files(paths);
    let results: Vec<_> = files
        .par_iter()
        .map(|p| (p.clone(), raw::read_meta(p)))
        .collect();
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for (path, r) in results {
        match r {
            Ok(meta) => ok.push(FrameInfo { path, meta, enabled: true, note: None, stats: None }),
            Err(e) => bad.push((path, e.to_string())),
        }
    }
    (ok, bad)
}

/// Relative exposure difference tolerated within one set (shutter jitter on
/// bulb/remote exposures is a few percent).
const EXPOSURE_TOLERANCE: f64 = 0.10;

fn exposure_matches(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= EXPOSURE_TOLERANCE * a.max(b),
        _ => true,
    }
}

/// Most common (exposure, ISO, dimensions) among the frames.
pub fn majority(frames: &[FrameInfo]) -> Option<RawMeta> {
    let mut counts: HashMap<(u64, u32, usize, usize), (usize, &RawMeta)> = HashMap::new();
    for f in frames {
        let key = (
            f.meta.exposure.unwrap_or(0.0).to_bits(),
            f.meta.iso.unwrap_or(0),
            f.meta.width,
            f.meta.height,
        );
        counts.entry(key).or_insert((0, &f.meta)).0 += 1;
    }
    counts.into_values().max_by_key(|(n, _)| *n).map(|(_, m)| m.clone())
}

/// Set `note` on frames that differ from the set's majority; untick those at
/// index >= `first_new` (frames the user has not had a chance to review).
pub fn flag_outliers(frames: &mut [FrameInfo], first_new: usize) {
    let Some(m) = majority(frames) else { return };
    for (i, f) in frames.iter_mut().enumerate() {
        let mut why = Vec::new();
        if !exposure_matches(f.meta.exposure, m.exposure) {
            why.push(format!(
                "exposure {} differs from the usual {}",
                fmt_exposure(f.meta.exposure),
                fmt_exposure(m.exposure)
            ));
        }
        if f.meta.iso != m.iso {
            why.push(format!("ISO {} differs from the usual {}", fmt_iso(f.meta.iso), fmt_iso(m.iso)));
        }
        if (f.meta.width, f.meta.height) != (m.width, m.height) {
            why.push(format!("size {}x{} differs", f.meta.width, f.meta.height));
        }
        if why.is_empty() {
            f.note = None;
        } else {
            f.note = Some(why.join("; "));
            if i >= first_new {
                f.enabled = false;
            }
        }
    }
}

pub fn fmt_exposure(e: Option<f64>) -> String {
    match e {
        None => "?".into(),
        Some(e) if e >= 0.3 => format!("{:.1}s", e),
        Some(e) if e > 0.0 => format!("1/{:.0}s", 1.0 / e),
        Some(_) => "0s".into(),
    }
}

pub fn fmt_iso(i: Option<u32>) -> String {
    i.map(|i| i.to_string()).unwrap_or_else(|| "?".into())
}

/// Days since an arbitrary epoch from an EXIF "YYYY:MM:DD hh:mm:ss" string.
fn exif_day(s: &str) -> Option<i64> {
    let mut it = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    // Days-from-civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe)
}

fn enabled(frames: &[FrameInfo]) -> Vec<FrameInfo> {
    frames.iter().filter(|f| f.enabled).cloned().collect()
}

fn date_of(m: &RawMeta) -> Option<i64> {
    m.datetime.as_deref().and_then(exif_day)
}

/// Human-readable warnings about calibration frames that do not match the
/// lights. Stacking is still allowed; these only inform.
pub fn warnings(set: &FrameSet) -> Vec<String> {
    let mut w = Vec::new();
    let lights = enabled(&set.lights);
    let Some(light) = majority(&lights) else {
        return w;
    };
    let unticked = set.lights.iter().filter(|f| !f.enabled && f.note.is_some()).count();
    if unticked > 0 {
        w.push(format!(
            "{unticked} light frame(s) are unticked because their exposure or ISO differs from the rest."
        ));
    }

    fn check(w: &mut Vec<String>, light: &RawMeta, kind: FrameKind, frames: &[FrameInfo]) -> Option<RawMeta> {
        let m = majority(&enabled(frames))?;
        let name = kind.label();
        if (m.width, m.height) != (light.width, light.height) {
            w.push(format!(
                "{name} are {}x{} but the lights are {}x{}; they cannot be used.",
                m.width, m.height, light.width, light.height
            ));
        }
        if !m.camera.is_empty() && !light.camera.is_empty() && m.camera != light.camera {
            w.push(format!("{name} were taken with a different camera ({}).", m.camera));
        }
        Some(m)
    }

    if let Some(d) = check(&mut w, &light, FrameKind::Dark, &set.darks) {
        if d.iso != light.iso {
            w.push(format!(
                "Darks are ISO {} but the lights are ISO {}; dark subtraction will be inaccurate.",
                fmt_iso(d.iso),
                fmt_iso(light.iso)
            ));
        }
        if !exposure_matches(d.exposure, light.exposure) {
            w.push(format!(
                "Darks are {} but the lights are {}; dark subtraction will be inaccurate.",
                fmt_exposure(d.exposure),
                fmt_exposure(light.exposure)
            ));
        }
        if let (Some(a), Some(b)) = (date_of(&d), date_of(&light))
            && (a - b).abs() > 30 {
                w.push(format!(
                    "Darks were taken {} days apart from the lights; sensor temperature and hot pixels may differ.",
                    (a - b).abs()
                ));
            }
    }
    let flat = check(&mut w, &light, FrameKind::Flat, &set.flats);
    if let Some(f) = &flat
        && let (Some(a), Some(b)) = (date_of(f), date_of(&light))
            && (a - b).abs() > 1 {
                w.push(format!(
                    "Flats were taken {} days apart from the lights; dust and vignetting may not match.",
                    (a - b).abs()
                ));
            }
    if let Some(df) = check(&mut w, &light, FrameKind::DarkFlat, &set.dark_flats) {
        match &flat {
            None => w.push("Dark-flats are loaded but there are no flats to apply them to.".into()),
            Some(f) => {
                if df.iso != f.iso || !exposure_matches(df.exposure, f.exposure) {
                    w.push(format!(
                        "Dark-flats ({}, ISO {}) do not match the flats ({}, ISO {}).",
                        fmt_exposure(df.exposure),
                        fmt_iso(df.iso),
                        fmt_exposure(f.exposure),
                        fmt_iso(f.iso)
                    ));
                }
            }
        }
    }
    if let Some(b) = check(&mut w, &light, FrameKind::Bias, &set.bias)
        && b.iso != light.iso {
            w.push(format!(
                "Bias frames are ISO {} but the lights are ISO {}.",
                fmt_iso(b.iso),
                fmt_iso(light.iso)
            ));
        }
    w
}

/// Convenience used by the CLI: scan a folder or file list into a slot.
pub fn add_paths(set: &mut FrameSet, kind: FrameKind, paths: &[PathBuf]) -> (usize, Vec<(PathBuf, String)>) {
    let (ok, bad) = scan(paths);
    (set.add(kind, ok), bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(name: &str, exp: f64, iso: u32, date: &str) -> FrameInfo {
        FrameInfo {
            path: PathBuf::from(name),
            meta: RawMeta {
                exposure: Some(exp),
                iso: Some(iso),
                datetime: Some(date.into()),
                camera: "Nikon D5600".into(),
                width: 6016,
                height: 4016,
            },
            enabled: true,
            note: None,
            stats: None,
        }
    }

    #[test]
    fn odd_exposures_are_unticked() {
        let mut set = FrameSet::default();
        let mut v: Vec<_> =
            (0..10).map(|i| frame(&format!("l{i}"), 60.0, 800, "2021:09:03 21:00:00")).collect();
        v.push(frame("jitter", 58.2, 800, "2021:09:03 21:00:00"));
        v.push(frame("short", 0.2, 800, "2021:09:03 21:00:00"));
        v.push(frame("half", 32.3, 800, "2021:09:03 21:00:00"));
        v.push(frame("iso", 60.0, 1600, "2021:09:03 21:00:00"));
        assert_eq!(set.add(FrameKind::Light, v), 14);
        let off: Vec<_> = set.lights.iter().filter(|f| !f.enabled).map(|f| f.name()).collect();
        assert_eq!(off, ["short", "half", "iso"]);
        assert!(set.lights.iter().all(|f| f.enabled == f.note.is_none()));
    }

    #[test]
    fn manual_ticks_survive_later_additions() {
        let mut set = FrameSet::default();
        let mut v: Vec<_> = (0..5).map(|i| frame(&format!("a{i}"), 60.0, 800, "2021:09:03")).collect();
        v.push(frame("short", 1.0, 800, "2021:09:03"));
        set.add(FrameKind::Light, v);
        set.lights[5].enabled = true; // user re-ticks it
        set.add(FrameKind::Light, vec![frame("b", 60.0, 800, "2021:09:03")]);
        assert!(set.lights[5].enabled);
        assert!(set.lights[5].note.is_some());
    }

    #[test]
    fn duplicates_are_not_added_twice() {
        let mut set = FrameSet::default();
        set.add(FrameKind::Dark, vec![frame("d", 60.0, 800, "2021:09:03")]);
        assert_eq!(set.add(FrameKind::Dark, vec![frame("d", 60.0, 800, "2021:09:03")]), 0);
    }

    #[test]
    fn mismatched_calibration_is_reported() {
        let mut set = FrameSet::default();
        set.add(FrameKind::Light, vec![frame("l", 60.0, 800, "2021:09:03 21:24:47")]);
        assert!(warnings(&set).is_empty());
        set.add(FrameKind::Dark, vec![frame("d", 60.0, 1600, "2020:09:20 21:10:13")]);
        set.add(FrameKind::Flat, vec![frame("f", 0.0004, 1600, "2020:09:20 21:29:48")]);
        set.add(FrameKind::DarkFlat, vec![frame("df", 0.0004, 1600, "2020:09:20 21:30:56")]);
        let w = warnings(&set).join("\n");
        assert!(w.contains("Darks are ISO 1600 but the lights are ISO 800"), "{w}");
        assert!(w.contains("Darks were taken 348 days apart"), "{w}");
        assert!(w.contains("Flats were taken 348 days apart"), "{w}");
        assert!(!w.contains("Dark-flats"), "{w}");
    }

    #[test]
    fn matching_calibration_is_quiet() {
        let mut set = FrameSet::default();
        set.add(FrameKind::Light, vec![frame("l", 60.0, 800, "2021:09:03 21:24:47")]);
        set.add(FrameKind::Dark, vec![frame("d", 59.5, 800, "2021:09:04 00:10:00")]);
        set.add(FrameKind::Flat, vec![frame("f", 0.01, 800, "2021:09:04 00:30:00")]);
        assert_eq!(warnings(&set), Vec::<String>::new());
    }
}
