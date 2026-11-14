//! The egui application: state, background jobs and layout.

mod curve_editor;
pub mod errors;
mod files_panel;
mod histogram_view;
mod history_panel;
mod process_panel;
mod stack_panel;
mod viewer;
mod worker;

use crate::Args;
use astro_processor::frames::{self, FrameInfo, FrameKind, FrameSet};
use astro_processor::histogram::Histogram;
use astro_processor::image::{Image, Rect};
use astro_processor::io::{export, import, project};
use astro_processor::process::history::History;
use astro_processor::process::{self, Model, ProcessParams, pipeline};
use astro_processor::progress::Progress;
use astro_processor::stack::{self, StackOptions, StackResult};
use eframe::egui::{self, Color32, RichText};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;
use viewer::Viewer;
use worker::{DetailRequest, PreviewOutput, PreviewRequest, PreviewWorker};

pub const ACCENT: Color32 = Color32::from_rgb(232, 160, 72);
pub const WARN: Color32 = Color32::from_rgb(240, 190, 90);
pub const ERROR: Color32 = Color32::from_rgb(240, 110, 100);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Files,
    Stack,
    Process,
}

struct StackOutcome {
    result: StackResult,
    params: ProcessParams,
}

enum Event {
    Scanned { kind: FrameKind, frames: Vec<FrameInfo>, bad: Vec<(PathBuf, String)> },
    ScanCancelled,
    StackDone(Result<Box<StackOutcome>, String>),
    IoDone(Result<String, String>),
    Loaded(Result<Box<(PathBuf, project::Loaded)>, String>),
    Saved(Result<PathBuf, String>),
    Imported(Result<Box<ImportOutcome>, String>),
}

/// An already-stacked image read from disk, with its starting settings.
struct ImportOutcome {
    path: PathBuf,
    image: Image,
    format: String,
    stretched: bool,
    /// Starting white balance (also what "reset" goes back to).
    wb: [f32; 3],
    params: ProcessParams,
}

struct StackJob {
    progress: Progress,
    started: Instant,
}

pub struct App {
    frames: FrameSet,
    stack_opts: StackOptions,
    params: ProcessParams,
    /// Edits made to `params` since stacking (or since the project was made).
    history: History,
    /// Name for the edit made this frame, where a button knows it better
    /// than a description of the changed settings would.
    edit_label: Option<&'static str>,
    show_history: bool,
    /// Bring the current history step into view (after undo or redo).
    scroll_to_step: bool,
    /// As-shot white balance of the lights.
    wb: [f32; 3],
    /// Area of the stack covered by (nearly) all frames.
    covered: Option<Rect>,
    stack: Option<Arc<Image>>,
    stack_id: u64,
    project_path: Option<PathBuf>,
    /// File name (without extension) of the stacked image that was opened,
    /// used to suggest names for the project and exports.
    image_name: Option<String>,

    tab: Tab,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    scans: usize,
    stack_job: Option<StackJob>,
    log: Vec<String>,
    /// Label of the file operation in progress (dialogs, export, save, load).
    io_busy: Option<&'static str>,
    status: Option<(String, bool)>,

    preview: PreviewWorker,
    viewer: Viewer,
    histogram: Option<Histogram>,
    model: Option<Model>,
    last_request: Option<(u64, ProcessParams, Option<DetailRequest>)>,
    show_before: bool,
    hist_log: bool,
    curve_channel: usize,
    levels_channel: usize,
    /// Slot shown in the Files tab; dropped files are added to it.
    slot: FrameKind,

    /// Tab requested on the command line; wins over the automatic switch
    /// when the start-up project finishes loading.
    start_tab: Option<Tab>,
    /// Whether the window system delivers dropped files (not on Wayland).
    can_drop: bool,
    /// Start stacking once the start-up scans have finished.
    auto_stack: bool,
    screenshot: Option<Screenshot>,
}

struct Screenshot {
    path: PathBuf,
    settled_frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.panel_fill = Color32::from_rgb(22, 23, 27);
        v.window_fill = Color32::from_rgb(28, 29, 34);
        v.extreme_bg_color = Color32::from_rgb(12, 12, 15);
        v.faint_bg_color = Color32::from_rgb(30, 31, 37);
        v.selection.bg_fill = Color32::from_rgb(120, 78, 30);
        v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = true;
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.slider_width = 150.0;
    });
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Self {
        style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let mut app = Self {
            frames: FrameSet::default(),
            stack_opts: StackOptions::default(),
            params: ProcessParams::default(),
            history: History::default(),
            edit_label: None,
            show_history: true,
            scroll_to_step: false,
            wb: [1.0; 3],
            covered: None,
            stack: None,
            stack_id: 0,
            project_path: None,
            image_name: None,
            tab: Tab::Files,
            tx,
            rx,
            scans: 0,
            stack_job: None,
            log: Vec::new(),
            io_busy: None,
            status: None,
            preview: PreviewWorker::new(cc.egui_ctx.clone()),
            viewer: Viewer::default(),
            histogram: None,
            model: None,
            last_request: None,
            show_before: false,
            hist_log: true,
            curve_channel: 0,
            levels_channel: 0,
            slot: FrameKind::Light,
            start_tab: None,
            can_drop: std::env::var_os("WAYLAND_DISPLAY").is_none(),
            auto_stack: args.stack,
            screenshot: args.screenshot.clone().map(|path| Screenshot {
                path,
                settled_frames: 0,
                requested: false,
                done: Arc::new(AtomicBool::new(false)),
            }),
        };
        let ctx = &cc.egui_ctx;
        for (kind, paths) in [
            (FrameKind::Light, &args.lights),
            (FrameKind::Dark, &args.darks),
            (FrameKind::Flat, &args.flats),
            (FrameKind::DarkFlat, &args.dark_flats),
            (FrameKind::Bias, &args.bias),
        ] {
            if !paths.is_empty() {
                app.scan(ctx, kind, paths.clone());
            }
        }
        if let Some(p) = &args.project {
            app.open_path(ctx, p.clone());
        }
        app.start_tab = match args.tab.as_deref() {
            Some("stack") => Some(Tab::Stack),
            Some("process") => Some(Tab::Process),
            Some("files") => Some(Tab::Files),
            _ => None,
        };
        app.tab = app.start_tab.unwrap_or(if args.project.is_some() { Tab::Process } else { Tab::Files });
        app
    }

    fn busy(&self) -> bool {
        self.stack_job.is_some() || self.io_busy.is_some()
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false));
    }

    fn set_error(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), true));
    }

    /// Run `work` on a background thread and deliver its event to the UI.
    fn spawn(&self, ctx: &egui::Context, work: impl FnOnce() -> Event + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    // --- Frames --------------------------------------------------------------

    fn scan(&mut self, ctx: &egui::Context, kind: FrameKind, paths: Vec<PathBuf>) {
        self.scans += 1;
        self.spawn(ctx, move || {
            let (frames, bad) = frames::scan(&paths);
            Event::Scanned { kind, frames, bad }
        });
    }

    fn pick_and_scan(&mut self, ctx: &egui::Context, kind: FrameKind, folder: bool) {
        self.scans += 1;
        self.spawn(ctx, move || {
            let dialog = rfd::FileDialog::new().set_title(format!("Add {}", kind.label().to_lowercase()));
            let picked = if folder {
                dialog.pick_folder().map(|p| vec![p])
            } else {
                // The portal matches extensions case-sensitively, and cameras
                // write upper-case names (DSC_0001.NEF), so list both.
                let exts: Vec<String> = astro_processor::io::raw::RAW_EXTENSIONS
                    .iter()
                    .flat_map(|e| [e.to_string(), e.to_uppercase()])
                    .collect();
                dialog.add_filter("Camera RAW", &exts).add_filter("All files", &["*"]).pick_files()
            };
            match picked {
                Some(paths) => {
                    let (frames, bad) = frames::scan(&paths);
                    Event::Scanned { kind, frames, bad }
                }
                None => Event::ScanCancelled,
            }
        });
    }

    // --- Stacking --------------------------------------------------------------

    fn start_stack(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let repaint = ctx.clone();
        let progress = Progress::with_notify(move || repaint.request_repaint());
        self.stack_job = Some(StackJob { progress: progress.clone(), started: Instant::now() });
        self.log.clear();
        self.status = None;
        let (set, opts) = (self.frames.clone(), self.stack_opts.clone());
        self.spawn(ctx, move || {
            let outcome = stack::stack(&set, &opts, &progress).map(|result| {
                progress.stage("Preparing preview", 1);
                let (w, h) = (result.image.width as f32, result.image.height as f32);
                let c = result.crop;
                let trimmed = c.w < result.image.width || c.h < result.image.height;
                let crop = trimmed.then(|| {
                    [c.x as f32 / w, c.y as f32 / h, (c.x + c.w) as f32 / w, (c.y + c.h) as f32 / h]
                });
                let params = pipeline::auto_params(&result.image, result.wb, crop);
                Box::new(StackOutcome { result, params })
            });
            Event::StackDone(outcome.map_err(|e| format!("{e:#}")))
        });
    }

    // --- Projects and export ---------------------------------------------------

    fn project(&self) -> project::Project {
        project::Project {
            frames: self.frames.clone(),
            stack_options: self.stack_opts.clone(),
            params: self.params.clone(),
            history: self.history.clone(),
            wb: self.wb,
            covered: self.covered,
            ..project::Project::default()
        }
    }

    fn save_project(&mut self, ctx: &egui::Context, ask: bool) {
        if self.busy() {
            return;
        }
        self.io_busy = Some("Saving project");
        let (project, stack) = (self.project(), self.stack.clone());
        let known = if ask { None } else { self.project_path.clone() };
        let suggestion = self.project_path.clone();
        let name = self.image_name.clone().unwrap_or_else(|| "untitled".into());
        self.spawn(ctx, move || {
            let path = known.or_else(|| {
                let mut d = rfd::FileDialog::new()
                    .set_title("Save project")
                    .add_filter("Astro Processor project", &[project::EXTENSION]);
                d = match &suggestion {
                    Some(p) => d.set_file_name(p.file_name().unwrap_or_default().to_string_lossy()),
                    None => d.set_file_name(format!("{name}.{}", project::EXTENSION)),
                };
                d.save_file().map(|p| {
                    if p.extension().is_some_and(|e| e == project::EXTENSION) { p } else { p.with_extension(project::EXTENSION) }
                })
            });
            match path {
                None => Event::Saved(Err(String::new())),
                Some(p) => Event::Saved(
                    project::save(&p, &project, stack.as_deref()).map(|_| p).map_err(|e| format!("{e:#}")),
                ),
            }
        });
    }

    fn open_project(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.io_busy = Some("Opening project");
        self.spawn(ctx, move || {
            let picked = rfd::FileDialog::new()
                .set_title("Open project")
                .add_filter("Astro Processor project", &[project::EXTENSION])
                .pick_file();
            match picked {
                None => Event::Loaded(Err(String::new())),
                Some(p) => open(&p),
            }
        });
    }

    /// Choose an already-stacked image to process.
    fn open_image(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.io_busy = Some("Opening image");
        self.spawn(ctx, move || {
            // The portal matches extensions case-sensitively.
            let exts: Vec<String> =
                import::EXTENSIONS.iter().flat_map(|e| [e.to_string(), e.to_uppercase()]).collect();
            let picked = rfd::FileDialog::new()
                .set_title("Open a stacked image")
                .add_filter("Stacked images (TIFF, FITS, PNG, JPEG)", &exts)
                .add_filter("All files", &["*"])
                .pick_file();
            match picked {
                None => Event::Imported(Err(String::new())),
                Some(p) => open(&p),
            }
        });
    }

    /// Open a project or an already-stacked image, whichever `path` is.
    fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.io_busy = Some(if is_project(&path) { "Opening project" } else { "Opening image" });
        self.spawn(ctx, move || open(&path));
    }

    /// Export the processed image, or the untouched linear stack.
    fn export(&mut self, ctx: &egui::Context, linear: bool) {
        let Some(stack) = self.stack.clone() else { return };
        if self.busy() {
            return;
        }
        self.io_busy = Some("Exporting");
        let params = self.params.clone();
        let wb = self.wb;
        let stem = self
            .project_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| self.image_name.clone())
            .unwrap_or_else(|| "astro".into());
        self.spawn(ctx, move || {
            let dialog = rfd::FileDialog::new().set_title(if linear { "Export linear stack" } else { "Export image" });
            let dialog = if linear {
                dialog.add_filter("FITS, 32-bit float", &["fits"]).set_file_name(format!("{stem}-linear.fits"))
            } else {
                dialog
                    .add_filter("TIFF, 16-bit", &["tif"])
                    .add_filter("PNG, 16-bit", &["png"])
                    .add_filter("JPEG", &["jpg"])
                    .add_filter("FITS, 32-bit float", &["fits"])
                    .set_file_name(format!("{stem}.tif"))
            };
            let Some(mut path) = dialog.save_file() else {
                return Event::IoDone(Err(String::new()));
            };
            if path.extension().is_none() {
                path.set_extension(if linear { "fits" } else { "tif" });
            }
            let result = if linear {
                let cards: Vec<(String, String)> =
                    ["WB_R", "WB_G", "WB_B"].iter().zip(wb).map(|(k, v)| (k.to_string(), v.to_string())).collect();
                export::save_fits(&path, &stack, &cards)
            } else {
                export::save(&path, &process::process_full(&stack, &params))
            };
            Event::IoDone(result.map(|_| format!("Exported {}", path.display())).map_err(|e| format!("{e:#}")))
        });
    }

    // --- Event handling --------------------------------------------------------

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Scanned { kind, frames, bad } => {
                    self.scans = self.scans.saturating_sub(1);
                    let found = frames.len();
                    let added = self.frames.add(kind, frames);
                    let off = self.frames.get(kind).iter().filter(|f| !f.enabled && f.note.is_some()).count();
                    let mut msg = format!("{}: added {added} file(s)", kind.label());
                    if found > added {
                        msg += &format!(", {} already listed", found - added);
                    }
                    if off > 0 {
                        msg += &format!("; {off} unticked because their settings differ");
                    }
                    if !bad.is_empty() {
                        msg += &format!("; {} could not be read", bad.len());
                        for (p, e) in &bad {
                            self.log.push(format!("{}: {e}", p.display()));
                        }
                    }
                    if found == 0 && bad.is_empty() {
                        self.set_error(format!("{}: no RAW files found there", kind.label()));
                    } else {
                        self.set_status(msg);
                    }
                }
                Event::ScanCancelled => self.scans = self.scans.saturating_sub(1),
                Event::StackDone(outcome) => {
                    let job = self.stack_job.take();
                    if let Some(job) = &job {
                        self.log = job.progress.snapshot().log;
                    }
                    match outcome {
                        Ok(o) => {
                            let StackOutcome { result, params } = *o;
                            for (path, stats) in &result.stats {
                                if let Some(f) = self.frames.lights.iter_mut().find(|f| &f.path == path) {
                                    f.stats = Some(*stats);
                                }
                            }
                            let secs = job.map(|j| j.started.elapsed().as_secs_f32()).unwrap_or(0.0);
                            self.set_status(format!(
                                "Stacked {} frames in {:.0}s. Automatic stretch and colour balance applied.",
                                result.frames_stacked, secs
                            ));
                            self.log.extend(result.notes.iter().cloned());
                            self.wb = result.wb;
                            self.covered = Some(result.crop);
                            self.set_stack(result.image);
                            self.image_name = None;
                            self.history = History::new("Stacked, automatic settings", params.clone(), history_panel::unix_now());
                            self.params = params;
                            self.tab = Tab::Process;
                        }
                        Err(e) if e == "cancelled" => self.set_status("Stacking cancelled."),
                        Err(e) => self.set_error(format!("Stacking failed: {e}")),
                    }
                }
                Event::IoDone(r) => {
                    self.io_busy = None;
                    match r {
                        Ok(msg) => self.set_status(msg),
                        Err(e) if e.is_empty() => {}
                        Err(e) => self.set_error(e),
                    }
                }
                Event::Saved(r) => {
                    self.io_busy = None;
                    match r {
                        Ok(p) => {
                            self.set_status(format!("Saved {}", p.display()));
                            self.project_path = Some(p);
                        }
                        Err(e) if e.is_empty() => {}
                        Err(e) => self.set_error(format!("Could not save: {e}")),
                    }
                }
                Event::Loaded(r) => {
                    self.io_busy = None;
                    match r {
                        Ok(b) => {
                            let (path, project::Loaded { project, stack, stack_error }) = *b;
                            self.frames = project.frames;
                            self.stack_opts = project.stack_options;
                            self.params = project.params;
                            self.image_name = None;
                            self.history = if project.history.matches(&self.params) {
                                project.history
                            } else {
                                History::new("Opened project", self.params.clone(), history_panel::unix_now())
                            };
                            self.edit_label = None;
                            self.wb = project.wb;
                            self.covered = project.covered;
                            self.log.clear();
                            match stack {
                                Some(img) => {
                                    self.set_stack(img);
                                    self.tab = Tab::Process;
                                }
                                None => {
                                    self.clear_stack();
                                    self.tab = Tab::Files;
                                }
                            }
                            if let Some(tab) = self.start_tab.take() {
                                self.tab = tab;
                            }
                            match stack_error {
                                Some(e) => self.set_error(format!("{e}. Frames and settings were loaded; stack again to rebuild it.")),
                                None => self.set_status(format!("Opened {}", path.display())),
                            }
                            self.project_path = Some(path);
                        }
                        Err(e) if e.is_empty() => {}
                        Err(e) => self.set_error(format!("Could not open: {e}")),
                    }
                }
                Event::Imported(r) => {
                    self.io_busy = None;
                    match r {
                        Ok(b) => {
                            let ImportOutcome { path, image, format, stretched, wb, params } = *b;
                            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                            // A new session: the frames of a previous one
                            // have nothing to do with this image.
                            self.frames = FrameSet::default();
                            self.stack_opts = StackOptions::default();
                            self.wb = wb;
                            self.covered = None;
                            self.project_path = None;
                            self.image_name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                            self.log.clear();
                            self.edit_label = None;
                            self.set_status(format!(
                                "Opened {name} ({format}, {} × {} px). {}",
                                image.width,
                                image.height,
                                if stretched {
                                    "It is already stretched, so it is shown as it is."
                                } else {
                                    "Automatic stretch and colour balance applied."
                                }
                            ));
                            self.set_stack(image);
                            self.history =
                                History::new(&format!("Opened {name}"), params.clone(), history_panel::unix_now());
                            self.params = params;
                            self.tab = self.start_tab.take().unwrap_or(Tab::Process);
                        }
                        Err(e) if e.is_empty() => {}
                        Err(e) => self.set_error(format!("Could not open: {e}")),
                    }
                }
            }
        }

        if let Some(e) = errors::take_dialog_error() {
            self.set_error(format!(
                "The file dialog could not be opened ({e}). Run the app from `nix develop`, or pass folders on the command line."
            ));
        }

        for out in self.preview.poll() {
            match out {
                PreviewOutput::Proxy { image, base_size, histogram, model } => {
                    self.viewer.set_proxy(ctx, &image, base_size);
                    self.histogram = Some(histogram);
                    self.model = Some(model);
                }
                PreviewOutput::Detail { image, request } => self.viewer.set_detail(ctx, &image, request),
            }
        }
    }

    fn set_stack(&mut self, image: Image) {
        self.stack = Some(Arc::new(image));
        self.stack_id += 1;
        self.last_request = None;
        self.viewer.fit = true;
    }

    fn clear_stack(&mut self) {
        self.stack = None;
        self.stack_id += 1;
        self.last_request = None;
        self.histogram = None;
        self.model = None;
        self.viewer.clear();
    }

    /// Parameters for the "before" view: framing, colour balance and stretch
    /// only, so the comparison shows what the adjustments add.
    fn before_params(&self) -> ProcessParams {
        ProcessParams {
            geometry: self.params.geometry.clone(),
            color: self.params.color.clone(),
            stretch: self.params.stretch.clone(),
            ..ProcessParams::default()
        }
    }

    fn request_preview(&mut self, detail: Option<DetailRequest>) {
        let Some(stack) = &self.stack else { return };
        let params = if self.show_before { self.before_params() } else { self.params.clone() };
        let key = (self.stack_id, params, detail);
        if self.last_request.as_ref() == Some(&key) {
            return;
        }
        self.preview.request(PreviewRequest {
            stack: stack.clone(),
            stack_id: key.0,
            params: key.1.clone(),
            detail,
        });
        self.last_request = Some(key);
    }

    /// Add a history step once an edit has settled (a slider being dragged
    /// is one step, made when it is let go).
    fn track_history(&mut self, ctx: &egui::Context) {
        if self.stack.is_none() || self.history.matches(&self.params) {
            self.edit_label = None;
            return;
        }
        if ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let label = self.edit_label.take();
        self.history.record(&self.params, label, history_panel::unix_now(), Instant::now());
        self.scroll_to_step = true;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        if self.stack.is_none() || self.tab != Tab::Process || ctx.egui_wants_keyboard_input() {
            return;
        }
        let redo = ctx.input_mut(|i| {
            i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
        });
        if redo {
            self.redo();
        } else if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z))) {
            self.undo();
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if dropped.is_empty() {
            return;
        }
        if let [one] = dropped.as_slice()
            && (is_project(one) || (one.is_file() && import::is_image_file(one)))
        {
            if !self.busy() {
                self.open_path(ctx, one.clone());
            }
            return;
        }
        self.scan(ctx, self.slot, dropped);
        self.tab = Tab::Files;
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let stack_ready = self.stack.is_none() || self.viewer.has_image();
        let idle = self.scans == 0 && !self.busy() && !self.auto_stack && stack_ready;
        let Some(shot) = &mut self.screenshot else { return };
        if shot.done.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint();
        shot.settled_frames = if idle { shot.settled_frames + 1 } else { 0 };
        if shot.settled_frames > 40 && !shot.requested {
            shot.requested = true;
            let (path, done) = (shot.path.clone(), shot.done.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |img: Arc<egui::ColorImage>| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(buf) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba)
                        && let Err(e) = buf.save(&path) {
                            eprintln!("screenshot failed: {e}");
                        }
                    done.store(true, Ordering::Relaxed);
                },
            )));
        }
    }

    // --- Layout ------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.busy(), |ui| {
                if ui.button("Open…").on_hover_text("Open a project").clicked() {
                    self.open_project(&ctx);
                }
                if ui
                    .button("Open image…")
                    .on_hover_text("Process an image that is already stacked (TIFF, FITS, PNG or JPEG), without its source frames")
                    .clicked()
                {
                    self.open_image(&ctx);
                }
                if ui.button("Save").clicked() {
                    self.save_project(&ctx, false);
                }
                if ui.button("Save as…").clicked() {
                    self.save_project(&ctx, true);
                }
                ui.separator();
                ui.add_enabled_ui(self.stack.is_some(), |ui| {
                    if ui.button("Export image…").on_hover_text("Processed image as 16-bit TIFF/PNG, JPEG or FITS").clicked() {
                        self.export(&ctx, false);
                    }
                    if ui
                        .button("Export linear…")
                        .on_hover_text("The unprocessed stack as 32-bit FITS, for other astronomy software")
                        .clicked()
                    {
                        self.export(&ctx, true);
                    }
                });
            });
            ui.separator();
            ui.add_enabled_ui(self.viewer.has_image(), |ui| {
                if ui.selectable_label(self.viewer.fit, "Fit").clicked() {
                    self.viewer.fit = true;
                }
                if ui.button("1:1").on_hover_text("One image pixel per screen pixel").clicked() {
                    self.viewer.zoom_to(1.0);
                }
                ui.label(RichText::new(format!("{:.0}%", self.viewer.zoom * 100.0)).weak());
                ui.toggle_value(&mut self.show_before, "Before")
                    .on_hover_text("Show the image with only framing, colour balance and stretch applied");
                ui.toggle_value(&mut self.show_history, "History")
                    .on_hover_text("List of the edits made in the Process tab; click one to go back to it");
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(p) = &self.project_path {
                    ui.label(RichText::new(p.file_name().unwrap_or_default().to_string_lossy()).weak());
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(label) = self.io_busy {
                ui.spinner();
                ui.label(format!("{label}…"));
            } else if self.scans > 0 {
                ui.spinner();
                ui.label("Reading frame information…");
            } else if let Some((msg, is_error)) = &self.status {
                let text = RichText::new(msg);
                ui.label(if *is_error { text.color(ERROR) } else { text });
            } else {
                ui.label(RichText::new("Ready").weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (w, h) = self.viewer.base_size;
                if w > 0 {
                    ui.label(RichText::new(format!("{w} × {h} px")).weak());
                }
            });
        });
    }
}

fn is_project(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == project::EXTENSION)
}

/// Read a project, or an already-stacked image with automatic settings.
fn open(path: &Path) -> Event {
    if is_project(path) {
        return Event::Loaded(
            project::load(path).map(|l| Box::new((path.to_path_buf(), l))).map_err(|e| format!("{e:#}")),
        );
    }
    Event::Imported(import::load(path).map_err(|e| format!("{e:#}")).map(|imported| {
        let wb = imported.white_balance();
        let import::Imported { image, format, stretched, .. } = imported;
        // Linear data needs a stretch to be visible at all; a picture that
        // already has one would be blown out by a second.
        let params = if stretched { ProcessParams::default() } else { pipeline::auto_params(&image, wb, None) };
        Box::new(ImportOutcome { path: path.to_path_buf(), image, format, stretched, wb, params })
    }))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        self.handle_drops(&ctx);
        self.handle_shortcuts(&ctx);
        if self.auto_stack && self.scans == 0 && !self.busy() {
            self.auto_stack = false;
            if self.frames.lights.iter().any(|f| f.enabled) {
                self.start_stack(&ctx);
            }
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(3.0);
            self.status_bar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::left("controls").resizable(true).default_size(380.0).size_range(330.0..=560.0).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let lights = self.frames.lights.iter().filter(|f| f.enabled).count();
                ui.selectable_value(&mut self.tab, Tab::Files, format!("1  Files ({lights})"));
                ui.selectable_value(&mut self.tab, Tab::Stack, "2  Stack");
                ui.selectable_value(&mut self.tab, Tab::Process, "3  Process");
            });
            ui.separator();
            match self.tab {
                Tab::Files => self.files_panel(ui),
                Tab::Stack => self.stack_panel(ui),
                Tab::Process => self.process_panel(ui),
            }
        });
        if self.show_history && self.tab == Tab::Process && self.stack.is_some() {
            egui::Panel::right("history").resizable(true).default_size(230.0).size_range(180.0..=420.0).show(ui, |ui| {
                self.history_panel(ui);
            });
        }
        self.track_history(&ctx);
        let detail = egui::CentralPanel::no_frame().show(ui, |ui| self.viewer.ui(ui)).inner;
        self.request_preview(detail);
        self.handle_screenshot(&ctx);
    }
}
