//! Astro Processor desktop application.

mod ui;

use clap::Parser;
use eframe::egui;
use std::path::PathBuf;

#[derive(Parser, Clone, Default)]
#[command(name = "astro-processor", about = "Stack and process astrophotography RAW frames")]
pub struct Args {
    /// Project file, or an already-stacked image (TIFF, FITS, PNG, JPEG),
    /// to open at start-up.
    pub project: Option<PathBuf>,
    /// Light frames (folders or files) to add at start-up.
    #[arg(long, num_args = 1..)]
    pub lights: Vec<PathBuf>,
    #[arg(long, num_args = 1..)]
    pub darks: Vec<PathBuf>,
    #[arg(long, num_args = 1..)]
    pub flats: Vec<PathBuf>,
    #[arg(long = "dark-flats", num_args = 1..)]
    pub dark_flats: Vec<PathBuf>,
    #[arg(long, num_args = 1..)]
    pub bias: Vec<PathBuf>,
    /// Tab to show at start-up: files, stack or process.
    #[arg(long)]
    pub tab: Option<String>,
    /// Start stacking as soon as the frames given on the command line have
    /// been read.
    #[arg(long)]
    pub stack: bool,
    /// Run through X11 (XWayland) instead of native Wayland. Needed for
    /// dropping files onto the window, which the windowing library does not
    /// support on Wayland yet; text may look softer on scaled displays.
    #[arg(long)]
    pub x11: bool,
    /// Save a screenshot of the window once everything has loaded, then quit
    /// (used for testing and documentation).
    #[arg(long, hide = true)]
    pub screenshot: Option<PathBuf>,
}

fn main() -> eframe::Result {
    let args = Args::parse();
    ui::errors::install();
    if args.x11 {
        // SAFETY: no other threads exist yet.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Astro Processor")
            .with_app_id("astro-processor")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Astro Processor",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, args)))),
    )
}
