//! Headless front end: scan folders, stack, and export without the GUI.

use anyhow::Result;
use astro_processor::frames::{self, FrameKind, FrameSet, fmt_exposure, fmt_iso};
use astro_processor::io::{export, import};
use astro_processor::process::{self, ProcessParams, pipeline};
use astro_processor::progress::Progress;
use astro_processor::stack::{self, StackMethod, StackOptions};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "astro-cli", about = "Stack and process astrophotography RAW frames")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct Inputs {
    /// Light frames: folders and/or RAW files.
    #[arg(long, num_args = 1.., required = true)]
    lights: Vec<PathBuf>,
    #[arg(long, num_args = 1..)]
    darks: Vec<PathBuf>,
    #[arg(long, num_args = 1..)]
    flats: Vec<PathBuf>,
    #[arg(long = "dark-flats", num_args = 1..)]
    dark_flats: Vec<PathBuf>,
    #[arg(long, num_args = 1..)]
    bias: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// List the frames that would be used and any mismatch warnings.
    Scan {
        #[command(flatten)]
        inputs: Inputs,
    },
    /// Calibrate, align and stack; writes the linear result.
    Stack {
        #[command(flatten)]
        inputs: Inputs,
        /// Output file (.fits keeps full precision; .tif/.png are 16-bit).
        #[arg(short, long)]
        output: PathBuf,
        /// Plain average instead of kappa-sigma clipping.
        #[arg(long)]
        average: bool,
        #[arg(long, default_value_t = 2.5)]
        kappa: f32,
        /// Stack only the first N usable lights (quick tests).
        #[arg(long)]
        limit: Option<usize>,
        /// Do not read or write cached master frames.
        #[arg(long)]
        no_cache: bool,
        /// Also save a project (with automatic processing settings) that the
        /// desktop app can open.
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Diagnose alignment between two light frames (no calibration).
    #[command(hide = true)]
    Match { reference: PathBuf, frame: PathBuf },
    /// Turn a linear stack (FITS from `stack`) into a finished image.
    Process {
        /// Linear stack: a FITS written by `stack`, or a stack from other
        /// software (TIFF, FITS, PNG, JPEG).
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Processing parameters as JSON (the "params" object of a project
        /// file). Without it: camera white balance (or one measured from the
        /// stars), neutral background, automatic stretch and crop to the
        /// fully covered area.
        #[arg(long)]
        params: Option<PathBuf>,
        /// Also remove background gradients (with automatic parameters).
        #[arg(long)]
        gradient: bool,
        /// Stretch with an automatic generalised hyperbolic stretch instead
        /// of the midtones stretch.
        #[arg(long)]
        ghs: bool,
        /// Deconvolve with this strength (0..1), using the star size
        /// measured from the image.
        #[arg(long, value_name = "AMOUNT")]
        deconv: Option<f32>,
        /// Boost large-scale local contrast (0..1).
        #[arg(long, value_name = "AMOUNT")]
        local_contrast: Option<f32>,
        /// Compress bright large areas such as galaxy cores (0..1).
        #[arg(long, value_name = "AMOUNT")]
        hdr: Option<f32>,
    },
}

fn load(inputs: &Inputs) -> FrameSet {
    let mut set = FrameSet::default();
    for (kind, paths) in [
        (FrameKind::Light, &inputs.lights),
        (FrameKind::Dark, &inputs.darks),
        (FrameKind::Flat, &inputs.flats),
        (FrameKind::DarkFlat, &inputs.dark_flats),
        (FrameKind::Bias, &inputs.bias),
    ] {
        if paths.is_empty() {
            continue;
        }
        let (added, bad) = frames::add_paths(&mut set, kind, paths);
        let on = set.get(kind).iter().filter(|f| f.enabled).count();
        println!("{:<10} {added} files, {on} ticked", kind.label());
        for (p, e) in bad {
            println!("  unreadable: {} ({e})", p.display());
        }
        for f in set.get(kind).iter().filter(|f| f.note.is_some()) {
            println!("  unticked {}: {}", f.name(), f.note.as_deref().unwrap_or(""));
        }
        if let Some(m) = frames::majority(set.get(kind)) {
            println!(
                "  {} ISO {} {}x{} {}",
                fmt_exposure(m.exposure),
                fmt_iso(m.iso),
                m.width,
                m.height,
                m.camera
            );
        }
    }
    for w in frames::warnings(&set) {
        println!("warning: {w}");
    }
    set
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Scan { inputs } => {
            load(&inputs);
        }
        Command::Stack { inputs, output, average, kappa, limit, no_cache, project } => {
            let mut set = load(&inputs);
            if let Some(n) = limit {
                let mut seen = 0;
                for f in set.lights.iter_mut().filter(|f| f.enabled) {
                    seen += 1;
                    f.enabled = seen <= n;
                }
            }
            let opts = StackOptions {
                method: if average { StackMethod::Average } else { StackMethod::KappaSigma },
                kappa,
                use_cache: !no_cache,
                ..StackOptions::default()
            };
            let start = Instant::now();
            // Print stage changes and log lines as they happen.
            let printed = std::sync::Arc::new(Mutex::new((String::new(), 0usize)));
            let handle: std::sync::Arc<Mutex<Option<Progress>>> = Default::default();
            let (p2, h2) = (printed.clone(), handle.clone());
            let progress = Progress::with_notify(move || {
                let Some(p) = h2.lock().unwrap().clone() else { return };
                let s = p.snapshot();
                let mut st = p2.lock().unwrap();
                for line in &s.log[st.1..] {
                    println!("  {line}");
                }
                st.1 = s.log.len();
                if st.0 != s.stage {
                    println!("[{:6.1}s] {} ({} items)", start.elapsed().as_secs_f32(), s.stage, s.total);
                    st.0 = s.stage;
                }
            });
            *handle.lock().unwrap() = Some(progress.clone());
            let result = stack::stack(&set, &opts, &progress)?;
            println!(
                "[{:6.1}s] done: {} frames, suggested crop {}x{}+{}+{}",
                start.elapsed().as_secs_f32(),
                result.frames_stacked,
                result.crop.w,
                result.crop.h,
                result.crop.x,
                result.crop.y
            );
            for (path, s) in &result.stats {
                println!(
                    "  {:<16} stars {:>4}  fwhm {:>5.2}  bg {:.4}  score {:>7.1}  {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    s.stars,
                    s.fwhm,
                    s.background,
                    s.score,
                    if s.registered { "aligned" } else { "NOT ALIGNED" }
                );
            }
            let is_fits = output
                .extension()
                .is_some_and(|e| ["fit", "fits", "fts"].contains(&e.to_string_lossy().to_lowercase().as_str()));
            if is_fits {
                // Keep what `process` needs for sensible automatic settings.
                let c = result.crop;
                let cards: Vec<(String, String)> = [
                    ("WB_R", result.wb[0].to_string()),
                    ("WB_G", result.wb[1].to_string()),
                    ("WB_B", result.wb[2].to_string()),
                    ("CROP_X", c.x.to_string()),
                    ("CROP_Y", c.y.to_string()),
                    ("CROP_W", c.w.to_string()),
                    ("CROP_H", c.h.to_string()),
                    ("NFRAMES", result.frames_stacked.to_string()),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
                export::save_fits(&output, &result.image, &cards)?;
            } else {
                export::save(&output, &result.image)?;
            }
            println!("wrote {}", output.display());
            if let Some(path) = project {
                for (p, st) in &result.stats {
                    if let Some(f) = set.lights.iter_mut().find(|f| &f.path == p) {
                        f.stats = Some(*st);
                    }
                }
                let (w, h) = (result.image.width as f32, result.image.height as f32);
                let c = result.crop;
                let crop = [c.x as f32 / w, c.y as f32 / h, (c.x + c.w) as f32 / w, (c.y + c.h) as f32 / h];
                let proj = astro_processor::io::project::Project {
                    frames: set,
                    stack_options: opts,
                    params: pipeline::auto_params(&result.image, result.wb, Some(crop)),
                    wb: result.wb,
                    covered: Some(c),
                    ..Default::default()
                };
                astro_processor::io::project::save(&path, &proj, Some(&result.image))?;
                println!("wrote {}", path.display());
            }
        }
        Command::Match { reference, frame } => {
            use astro_processor::calibrate::{Masters, calibrate};
            use astro_processor::register::{detect, find_transform, stars::DetectOptions};
            let det = |p: &PathBuf| -> Result<_> {
                let cfa = calibrate(astro_processor::io::raw::load_cfa(p)?, &Masters::default(), 8.0).0;
                Ok(detect(&astro_processor::debayer::bin2(&cfa), &DetectOptions::default()))
            };
            let (a, b) = (det(&reference)?, det(&frame)?);
            println!("reference: {} stars, fwhm {:.2}", a.stars.len(), a.fwhm);
            println!("frame:     {} stars, fwhm {:.2}", b.stars.len(), b.fwhm);
            match find_transform(&a.stars, &b.stars) {
                Some(m) => println!(
                    "matched: {} inliers, rms {:.3} px, shift ({:.1}, {:.1}) binned px, flux ratio {:.3}",
                    m.inliers, m.rms, m.transform.c, m.transform.f, m.flux_ratio
                ),
                None => println!("no match"),
            }
        }
        Command::Process { input, output, params, gradient, ghs, deconv, local_contrast, hdr } => {
            let imported = import::load(&input)?;
            let header = &imported;
            let mut p = match params {
                Some(path) => serde_json::from_str::<ProcessParams>(&std::fs::read_to_string(&path)?)?,
                None if imported.stretched => ProcessParams::default(),
                None => {
                    let wb = imported.white_balance();
                    let stack = &imported.image;
                    let (w, h) = (stack.width as f32, stack.height as f32);
                    let crop = match (
                        header.card_f32("CROP_X"),
                        header.card_f32("CROP_Y"),
                        header.card_f32("CROP_W"),
                        header.card_f32("CROP_H"),
                    ) {
                        (Some(x), Some(y), Some(cw), Some(ch)) => Some([x / w, y / h, (x + cw) / w, (y + ch) / h]),
                        _ => None,
                    };
                    pipeline::auto_params(stack, wb, crop)
                }
            };
            let stack = &imported.image;
            if gradient {
                p.gradient.enabled = true;
            }
            if gradient || ghs {
                let base = process::geometry::apply(stack, &p.geometry);
                let model = process::analyze(&base, &p);
                p.stretch = if ghs { pipeline::auto_ghs(&model, 0.15) } else { pipeline::auto_stretch(&model) };
            }
            if let Some(amount) = deconv {
                let t = Instant::now();
                match process::deconv::measure_fwhm(stack) {
                    Some(fwhm) => {
                        println!("star FWHM {fwhm:.2} px (measured in {:.1}s)", t.elapsed().as_secs_f32());
                        p.deconvolution.amount = amount;
                        p.deconvolution.psf_fwhm = fwhm;
                    }
                    None => println!("too few stars to measure their size: skipping deconvolution"),
                }
            }
            if let Some(a) = local_contrast {
                p.local_contrast.amount = a;
            }
            if let Some(a) = hdr {
                p.local_contrast.hdr = a;
            }
            let start = Instant::now();
            let out = process::process_full(stack, &p);
            export::save(&output, &out)?;
            println!(
                "wrote {} ({}x{}) in {:.1}s",
                output.display(),
                out.width,
                out.height,
                start.elapsed().as_secs_f32()
            );
            println!("{}", serde_json::to_string(&p)?);
        }
    }
    Ok(())
}
