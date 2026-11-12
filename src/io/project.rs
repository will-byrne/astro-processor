//! Project files: everything needed to reopen a session without restacking.
//!
//! `name.astroproj` is JSON (frame lists, stacking options, processing
//! parameters); the linear stack is stored next to it as `name.stack.fits`.

use super::export;
use crate::frames::FrameSet;
use crate::image::{Image, Rect};
use crate::process::ProcessParams;
use crate::process::history::History;
use crate::stack::StackOptions;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const EXTENSION: &str = "astroproj";
const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub frames: FrameSet,
    pub stack_options: StackOptions,
    pub params: ProcessParams,
    /// Edits made in the Process tab, ending at (or pointing to) `params`.
    #[serde(default)]
    pub history: History,
    /// As-shot white balance of the lights.
    #[serde(default = "unit_wb")]
    pub wb: [f32; 3],
    /// Fully covered area of the stack, if one exists.
    #[serde(default)]
    pub covered: Option<Rect>,
    /// File name of the stack, relative to the project file.
    #[serde(default)]
    pub stack_file: Option<String>,
}

fn unit_wb() -> [f32; 3] {
    [1.0; 3]
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: VERSION,
            frames: FrameSet::default(),
            stack_options: StackOptions::default(),
            params: ProcessParams::default(),
            history: History::default(),
            wb: unit_wb(),
            covered: None,
            stack_file: None,
        }
    }
}

fn stack_path(project: &Path) -> PathBuf {
    project.with_extension("stack.fits")
}

/// Write the project, and the stack beside it when there is one.
pub fn save(path: &Path, project: &Project, stack: Option<&Image>) -> Result<()> {
    let mut project = project.clone();
    project.version = VERSION;
    project.stack_file = match stack {
        Some(img) => {
            let sp = stack_path(path);
            export::save_fits(&sp, img, &[("CREATOR".into(), "astro-processor".into())])?;
            Some(sp.file_name().expect("has a file name").to_string_lossy().into_owned())
        }
        None => None,
    };
    let json = serde_json::to_string_pretty(&project)?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

/// A project read from disk.
pub struct Loaded {
    pub project: Project,
    pub stack: Option<Image>,
    /// Why the stack the project refers to could not be loaded. The frame
    /// lists and settings are still usable; the frames need restacking.
    pub stack_error: Option<String>,
}

pub fn load(path: &Path) -> Result<Loaded> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let project: Project =
        serde_json::from_str(&text).with_context(|| format!("{} is not a valid project file", path.display()))?;
    if project.version > VERSION {
        bail!("{} was saved by a newer version of Astro Processor", path.display());
    }
    let (stack, stack_error) = match &project.stack_file {
        Some(name) => {
            let sp = path.parent().unwrap_or(Path::new(".")).join(name);
            if !sp.exists() {
                (None, Some(format!("Stacked image {name} is missing")))
            } else {
                match export::load_fits(&sp) {
                    Ok((img, _)) => (Some(img), None),
                    Err(e) => (None, Some(format!("Stacked image {name} could not be read ({e:#})"))),
                }
            }
        }
        None => (None, None),
    };
    Ok(Loaded { project, stack, stack_error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{FrameInfo, FrameKind};
    use crate::io::raw::RawMeta;

    #[test]
    fn roundtrip_with_and_without_a_stack() {
        let dir = std::env::temp_dir().join(format!("astro-project-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut project = Project::default();
        project.frames.add(
            FrameKind::Light,
            vec![FrameInfo {
                path: "/data/a.NEF".into(),
                meta: RawMeta { exposure: Some(60.0), iso: Some(800), ..RawMeta::default() },
                enabled: true,
                note: None,
                stats: None,
            }],
        );
        project.params.tone.shadows = 0.25;
        project.params.curves.master.insert(1, [0.3, 0.45]);
        project.history = History::new("Stacked", ProcessParams::default(), 1_700_000_000);
        project.history.record(&project.params, None, 1_700_000_060, std::time::Instant::now());
        project.stack_options.kappa = 3.5;
        project.wb = [2.1, 1.0, 1.4];
        project.covered = Some(Rect { x: 1, y: 2, w: 3, h: 4 });

        let path = dir.join("m31.astroproj");
        save(&path, &project, None).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.project, project);
        assert!(back.stack.is_none() && back.stack_error.is_none());

        let mut img = Image::new(5, 4);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = i as f32 * 0.01;
        }
        save(&path, &project, Some(&img)).unwrap();
        assert!(dir.join("m31.stack.fits").exists());
        let back = load(&path).unwrap();
        assert_eq!(back.project.stack_file.as_deref(), Some("m31.stack.fits"));
        assert_eq!(back.stack.unwrap(), img);

        // Losing the stack file must not lose the frame lists and settings.
        std::fs::remove_file(dir.join("m31.stack.fits")).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.project.frames, project.frames);
        assert_eq!(back.project.params, project.params);
        assert!(back.stack.is_none());
        assert!(back.stack_error.unwrap().contains("missing"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn older_files_with_missing_fields_still_load() {
        let dir = std::env::temp_dir().join(format!("astro-project-compat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.astroproj");
        std::fs::write(
            &path,
            r#"{"version":1,"frames":{"lights":[],"darks":[],"flats":[],"dark_flats":[],"bias":[]},
               "stack_options":{"kappa":2.0},"params":{"tone":{"contrast":0.5}}}"#,
        )
        .unwrap();
        let p = load(&path).unwrap().project;
        assert_eq!(p.stack_options.kappa, 2.0);
        assert_eq!(p.stack_options.iterations, 1);
        assert_eq!(p.params.tone.contrast, 0.5);
        assert_eq!(p.params.tone.saturation, 1.0);
        std::fs::write(&path, r#"{"version":99}"#).unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
