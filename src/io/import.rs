//! Reading images that were stacked elsewhere (DeepSkyStacker, Siril,
//! PixInsight, ...) so they can be processed without their source frames.
//!
//! TIFF (8/16/32-bit integer and 32/64-bit float), FITS (every standard
//! sample format), PNG and JPEG are accepted, in greyscale or RGB. Samples
//! are scaled to the 0..1 range the pipeline works in.

use super::fits;
use crate::image::{Image, luma};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Extensions offered in file dialogs and recognised when a file is dropped.
pub const EXTENSIONS: [&str; 8] = ["tif", "tiff", "fit", "fits", "fts", "png", "jpg", "jpeg"];

pub fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub struct Imported {
    pub image: Image,
    /// What the file held, for the status line ("32-bit float TIFF").
    pub format: String,
    /// The image has already been stretched for display, rather than being
    /// linear data straight from a stacking program.
    pub stretched: bool,
}

/// Whether `img` has already been stretched for display.
///
/// A linear stack keeps nearly all of its pixels in a sliver just above the
/// sky level (measured stacks: 95th percentile within 0.003 of the median),
/// while a stretched picture spreads them out (0.15 and more), so the gap
/// between the two tells them apart with a wide margin.
pub fn looks_stretched(img: &Image) -> bool {
    let step = (((img.width * img.height) as f32 / 300_000.0).sqrt().ceil() as usize).max(1);
    let mut lum = Vec::new();
    for y in (0..img.height).step_by(step) {
        for x in (0..img.width).step_by(step) {
            let p = img.px(x, y);
            let l = luma(p[0], p[1], p[2]);
            // Pure black is an uncovered border, not sky.
            if l.is_finite() && p != [0.0; 3] {
                lum.push(l);
            }
        }
    }
    if lum.is_empty() {
        return false;
    }
    lum.sort_unstable_by(|a, b| a.total_cmp(b));
    let at = |f: f32| lum[((lum.len() - 1) as f32 * f) as usize];
    at(0.95) - at(0.5) > 0.05
}

/// Samples as stored, before scaling to 0..1.
struct Samples {
    width: usize,
    height: usize,
    channels: usize,
    data: Vec<f32>,
    /// Value that means white for integer formats; `None` when it has to be
    /// worked out from the data (floating point).
    white: Option<f32>,
    format: String,
}

pub fn load(path: &Path) -> Result<Imported> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let samples = match ext.as_str() {
        "tif" | "tiff" => load_tiff(path)?,
        "fit" | "fits" | "fts" => load_fits(path)?,
        "png" | "jpg" | "jpeg" => load_other(path)?,
        _ => bail!("{}: not a supported image (use TIFF, FITS, PNG or JPEG)", path.display()),
    };
    finish(samples).with_context(|| format!("reading {}", path.display()))
}

/// White level for samples with no declared range. Stacking programs write
/// floats either as 0..1 or on the 16-bit scale (DeepSkyStacker); anything
/// larger is scaled by its own maximum.
fn guess_white(max: f32) -> f32 {
    if !(max > 2.0) {
        1.0
    } else if max <= 65535.0 {
        65535.0
    } else {
        max
    }
}

fn finish(s: Samples) -> Result<Imported> {
    let Samples { width, height, channels, mut data, white, format } = s;
    if width == 0 || height == 0 || data.len() != width * height * channels {
        bail!("image data does not match its dimensions");
    }
    let white = white.unwrap_or_else(|| {
        let max = data.par_iter().copied().filter(|v| v.is_finite()).reduce(|| 0.0, f32::max);
        guess_white(max)
    });
    if white != 1.0 {
        let k = 1.0 / white;
        data.par_iter_mut().for_each(|v| *v *= k);
    }
    // Extra channels (alpha) are dropped; greyscale becomes neutral RGB.
    let rgb = match channels {
        3 => data,
        1 => data.iter().flat_map(|&v| [v, v, v]).collect(),
        2 => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0]]).collect(),
        4 => data.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        n => bail!("{n}-channel images are not supported"),
    };
    if !rgb.par_iter().any(|v| v.is_finite() && *v > 0.0) {
        bail!("the image is completely black");
    }
    let image = Image { width, height, data: rgb };
    let stretched = looks_stretched(&image);
    Ok(Imported { image, format, stretched })
}

fn load_tiff(path: &Path) -> Result<Samples> {
    use tiff::ColorType;
    use tiff::decoder::{Decoder, DecodingResult, Limits};

    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut dec = Decoder::new(BufReader::with_capacity(1 << 20, file))
        .with_context(|| format!("{} is not a readable TIFF", path.display()))?
        .with_limits(Limits::unlimited());
    let (w, h) = dec.dimensions()?;
    let channels = match dec.colortype()? {
        ColorType::Gray(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) => 3,
        ColorType::RGBA(_) => 4,
        other => bail!("{}: {other:?} TIFF files are not supported (save it as RGB)", path.display()),
    };
    let (data, white, kind): (Vec<f32>, Option<f32>, &str) =
        match dec.read_image().with_context(|| format!("decoding {}", path.display()))? {
            DecodingResult::U8(v) => (v.into_iter().map(f32::from).collect(), Some(255.0), "8-bit"),
            DecodingResult::U16(v) => (v.into_iter().map(f32::from).collect(), Some(65535.0), "16-bit"),
            DecodingResult::U32(v) => (v.into_iter().map(|x| x as f32).collect(), Some(u32::MAX as f32), "32-bit integer"),
            DecodingResult::F32(v) => (v, None, "32-bit float"),
            DecodingResult::F64(v) => (v.into_iter().map(|x| x as f32).collect(), None, "64-bit float"),
            _ => bail!("{}: this TIFF sample format is not supported", path.display()),
        };
    Ok(Samples { width: w as usize, height: h as usize, channels, data, white, format: format!("{kind} TIFF") })
}

fn load_fits(path: &Path) -> Result<Samples> {
    let f = fits::read(path)?;
    let (white, kind) = match f.bitpix {
        8 => (Some(255.0), "8-bit"),
        16 => (Some(65535.0), "16-bit"),
        32 => (None, "32-bit integer"),
        -64 => (None, "64-bit float"),
        _ => (None, "32-bit float"),
    };
    Ok(Samples {
        width: f.width,
        height: f.height,
        channels: f.channels,
        data: f.data,
        white,
        format: format!("{kind} FITS"),
    })
}

fn load_other(path: &Path) -> Result<Samples> {
    let mut reader = image::ImageReader::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .with_guessed_format()?;
    reader.no_limits();
    let name = match reader.format() {
        Some(image::ImageFormat::Png) => "PNG",
        Some(image::ImageFormat::Jpeg) => "JPEG",
        _ => "image",
    };
    let img = reader.decode().with_context(|| format!("decoding {}", path.display()))?;
    let (width, height) = (img.width() as usize, img.height() as usize);
    let deep = img.color().bytes_per_pixel() / img.color().channel_count() > 1;
    let (data, white, bits): (Vec<f32>, f32, &str) = if deep {
        (img.into_rgb16().into_raw().into_iter().map(f32::from).collect(), 65535.0, "16-bit")
    } else {
        (img.into_rgb8().into_raw().into_iter().map(f32::from).collect(), 255.0, "8-bit")
    };
    Ok(Samples { width, height, channels: 3, data, white: Some(white), format: format!("{bits} {name}") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::export;
    use std::io::BufWriter;
    use tiff::encoder::{TiffEncoder, colortype};

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("astro-import-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn gradient(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = (i as f32 + 1.0) / (w * h * 3) as f32;
        }
        img
    }

    fn close(a: &Image, b: &Image, tol: f32) {
        assert_eq!((a.width, a.height), (b.width, b.height));
        for (x, y) in a.data.iter().zip(&b.data) {
            assert!((x - y).abs() <= tol, "{x} vs {y}");
        }
    }

    #[test]
    fn our_own_exports_come_back() {
        let d = dir("exports");
        let img = gradient(9, 7);
        for (ext, tol, format) in [
            ("tif", 1.0 / 65535.0, "16-bit TIFF"),
            ("png", 1.0 / 65535.0, "16-bit PNG"),
            ("fits", 0.0, "32-bit float FITS"),
        ] {
            let p = d.join(format!("a.{ext}"));
            export::save(&p, &img).unwrap();
            let back = load(&p).unwrap();
            assert_eq!(back.format, format);
            close(&back.image, &img, tol);
        }
        let p = d.join("a.JPG");
        export::save(&p, &img).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.format, "8-bit JPEG");
        close(&back.image, &img, 0.08);
        assert!(load(&d.join("a.bmp")).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn float_tiff_on_the_16_bit_scale_is_normalised() {
        // DeepSkyStacker's Autosave.tif: 32-bit float, values 0..65535.
        let d = dir("dss");
        let img = gradient(8, 5);
        let scaled: Vec<f32> = img.data.iter().map(|v| v * 65535.0).collect();
        let p = d.join("Autosave.tif");
        TiffEncoder::new(BufWriter::new(File::create(&p).unwrap()))
            .unwrap()
            .write_image::<colortype::RGB32Float>(8, 5, &scaled)
            .unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.format, "32-bit float TIFF");
        close(&back.image, &img, 1e-6);

        // Already 0..1: left alone.
        TiffEncoder::new(BufWriter::new(File::create(&p).unwrap()))
            .unwrap()
            .write_image::<colortype::RGB32Float>(8, 5, &img.data)
            .unwrap();
        assert_eq!(load(&p).unwrap().image, img);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn greyscale_becomes_rgb() {
        let d = dir("grey");
        let p = d.join("mono.tif");
        let data: Vec<u16> = (0..12).map(|i| i * 5000).collect();
        TiffEncoder::new(BufWriter::new(File::create(&p).unwrap()))
            .unwrap()
            .write_image::<colortype::Gray16>(4, 3, &data)
            .unwrap();
        let back = load(&p).unwrap().image;
        assert_eq!((back.width, back.height), (4, 3));
        assert_eq!(back.px(1, 0), [5000.0 / 65535.0; 3]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn integer_fits_with_an_offset_is_read() {
        // 16-bit FITS as most capture software writes it: signed samples
        // with BZERO = 32768, top row last.
        let d = dir("fits16");
        let p = d.join("m42.fit");
        let (w, h) = (3usize, 2usize);
        let mut bytes = Vec::new();
        let mut card = |s: &str| bytes.extend_from_slice(format!("{s:<80}").as_bytes());
        card("SIMPLE  =                    T");
        card("BITPIX  =                   16");
        card("NAXIS   =                    2");
        card("NAXIS1  =                    3");
        card("NAXIS2  =                    2");
        card("BZERO   =                32768 / offset");
        card("BSCALE  =                    1");
        card("END");
        bytes.resize(2880, b' ');
        let stored: [u16; 6] = [0, 1000, 2000, 30000, 40000, 65535];
        for v in stored {
            bytes.extend_from_slice(&((v as i32 - 32768) as i16).to_be_bytes());
        }
        bytes.resize(2880 * 2, 0);
        std::fs::write(&p, bytes).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.format, "16-bit FITS");
        let img = back.image;
        assert_eq!((img.width, img.height), (w, h));
        // First stored row is the bottom of the picture.
        assert_eq!(img.px(0, 1), [0.0; 3]);
        assert_eq!(img.px(2, 0), [1.0; 3]);
        assert!((img.px(0, 0)[0] - 30000.0 / 65535.0).abs() < 1e-6);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn stretched_pictures_are_told_from_linear_stacks() {
        // Linear: faint noisy sky with a few bright stars.
        let (w, h) = (200, 150);
        let mut linear = Image::new(w, h);
        let mut s = 3u32;
        for (i, px) in linear.data.chunks_exact_mut(3).enumerate() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (s >> 8) as f32 / (1 << 24) as f32 * 0.002;
            let star = if i % 997 == 0 { 0.6 } else { 0.0 };
            px.fill(0.018 + noise + star);
        }
        assert!(!looks_stretched(&linear));

        // The same picture after a strong midtone stretch.
        let mut stretched = linear.clone();
        for v in &mut stretched.data {
            *v = ((*v - 0.017) * 40.0).clamp(0.0, 1.0).sqrt();
        }
        assert!(looks_stretched(&stretched));
        assert!(!looks_stretched(&Image::new(4, 4)));
    }

    #[test]
    fn white_level_guesses() {
        assert_eq!(guess_white(0.8), 1.0);
        assert_eq!(guess_white(1.4), 1.0);
        assert_eq!(guess_white(64731.0), 65535.0);
        assert_eq!(guess_white(250_000.0), 250_000.0);
        assert_eq!(guess_white(f32::NAN), 1.0);
    }
}
