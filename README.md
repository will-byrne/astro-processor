# Astro Processor

A desktop app, written in Rust, for turning a night's worth of camera RAW files
into a finished astrophotograph: calibrate, align and stack the exposures, then
stretch and tune the result.

## Running

The repository ships a Nix dev shell with the Rust toolchain and the system
libraries the GUI needs:

```sh
nix develop
cargo run --release                  # the desktop app
cargo run --release --bin astro-cli -- --help   # headless stacking/processing
cargo test
```

Always use `--release` for real data; a debug build is many times slower.

Run the app from inside `nix develop`: the file dialogs reach the desktop
portal through `libdbus`, which the shell puts on the library path.

On Wayland, dropping files onto the window is not supported by the windowing
library yet. Start with `--x11` (`cargo run --release -- --x11`) to run
through XWayland, where it is.

## Workflow

1. **Files** – pick a slot (Lights, Darks, Flats, Dark-flats, Bias) and add
   files or a folder, or drop them onto the window (see the note on Wayland
   above). Only lights are required.
   Frames whose exposure or ISO differs from the rest of their slot are
   unticked and marked; calibration frames that do not match the lights
   (ISO, exposure, date, size) produce warnings but can still be used.
2. **Stack** – choose which calibration sets to apply and press *Stack*.
   *Advanced* has the combination method, rejection threshold, reference
   frame, a "keep best N %" filter and hot-pixel repair. After stacking, the
   Files tab shows star count, FWHM and a quality score for every light.
3. **Process** – everything here is non-destructive and applied live:
   stretch (midtones, arcsinh, and a generalised hyperbolic stretch with an
   *Auto GHS* button), tone (exposure, contrast, highlights, shadows,
   saturation, green removal), local contrast and HDR, levels, curves,
   colour balance (including white balance measured from the stars),
   background gradient removal, deconvolution, noise reduction and
   sharpening, star reduction, crop and rotate.
   *Deconvolution* (Richardson–Lucy) works on the linear data before the
   stretch; turning it on measures the star size from the image. It
   leaves the sky alone and stops dark rings forming around stars.
   *Local contrast & HDR* brings out dust lanes and filaments and
   brings bright cores (M31, M42) down without boosting the stars.
   Double-click a slider to reset it. *Before* shows the image without the
   adjustments. Scroll to zoom, drag to pan, double-click to toggle fit/1:1.
   The *History* panel on the right lists every change with when it was
   made; click a step to go back to it (Ctrl+Z / Ctrl+Shift+Z step through
   it). Later steps stay available until you make a new change from the
   earlier point. The history is saved with the project.
   **Already-stacked images** – *Open image…* (or dropping the file on the
   window, or `astro-processor stacked.tif`) loads a stack made elsewhere,
   e.g. DeepSkyStacker's `Autosave.tif`, straight into the Process tab; no
   light or calibration frames are needed. TIFF (8/16/32-bit and float),
   FITS, PNG and JPEG are read. Linear stacks get the automatic stretch and
   a white balance measured from the stars (so the typical star is white;
   stacks from this app keep the camera white balance they were saved
   with); pictures that are already stretched are shown as they are.
   Stacks with vignetting or light pollution usually need *Background
   gradient* turned on as well.
4. **Export** – 16-bit TIFF or PNG, JPEG, or 32-bit FITS; *Export linear*
   writes the unprocessed stack as FITS for other astronomy software.
   *Save* writes a `.astroproj` project plus the stack next to it
   (`name.stack.fits`), so a session reopens without restacking.

## How stacking works

- Master dark, flat, dark-flat and bias frames are built with a sigma-clipped
  mean and cached in `~/.cache/astro-processor/masters`.
- Each light is calibrated on the raw Bayer data: subtract the dark (or bias,
  or the camera's black level), divide by the flat, repair hot pixels.
- Stars are detected on a 2×2-binned copy; frames are matched to the
  reference by triangle similarity and solved for an affine transform.
- Frames are demosaiced, normalised to the reference's sky background and
  transparency, resampled with Lanczos-3, and combined by average or
  kappa-sigma clipping. Frames are streamed, so memory use does not grow with
  the number of lights (kappa-sigma reads every frame once per pass).

## Test data

These folders sit in `test-data/` during development and are ignored by
git:

| Folder         | Slot       | Contents                                   |
|----------------|------------|--------------------------------------------|
| `andromeda/`   | Lights     | 123 NEF, 108 of them ~60 s at ISO 800      |
| `darks/`       | Darks      | 10 NEF, 60 s, ISO 1600                     |
| `bias/`        | Flats      | 45 NEF, 1/2500 s, ISO 1600, evenly lit     |
| `dark bias/`   | Dark-flats | 45 NEF, 1/2500 s, ISO 1600, lens covered   |
| `pre-stacked/` | Open image | Finished stacks: PixInsight FITS (32-bit integer and float, Canon DSLR and ZWO OSC), DeepSkyStacker 32-bit float TIFFs, and an Orion L/R/G/B set of mono masters (TIFF and XISF) |

(The folder names predate this project; the slot column is what the frames
actually are.) End-to-end run:

```sh
cd test-data
cargo run --release --bin astro-cli -- stack \
    --lights andromeda --darks darks --flats bias --dark-flats "dark bias" \
    -o m31.fits --project m31.astroproj
cargo run --release --bin astro-cli -- process m31.fits -o m31.jpg
cargo run --release -- m31.astroproj      # continue in the app

# An image stacked elsewhere:
cargo run --release --bin astro-cli -- process --gradient \
    pre-stacked/c49_9h_stacked.fit -o c49.jpg

# With the extra processing tools (see `process --help`):
cargo run --release --bin astro-cli -- process --gradient --deconv 0.8 \
    --local-contrast 0.5 --hdr 0.6 pre-stacked/andromeda_11h_stacked.fit -o m31.jpg
```

## Limitations

- Bayer (2×2 colour filter) RAW files only; no X-Trans or monochrome sensors.
- Demosaicing is bilinear.
- Median stacking and drizzle are not implemented.
- Alignment is affine (shift, rotation, scale), without lens-distortion
  correction.

## Code layout

- `src/lib.rs` and its modules – all image processing, no GUI dependency:
  `io` (RAW, FITS, export, projects), `frames`, `calibrate`, `debayer`,
  `register`, `combine`, `stack`, `process`, `histogram`. Processing
  stages live in `src/process/`, one module each (`ghs`, `deconv`,
  `local_contrast`, `gradient`, `detail`, `stars`, ...), run in order by
  `pipeline.rs`.
- `src/bin/astro-cli.rs` – command-line front end.
- `src/main.rs`, `src/ui/` – the egui application.
