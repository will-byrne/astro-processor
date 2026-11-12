//! Writing images in interchange formats.

use super::fits::{self, FitsImage};
use crate::image::Image;
use anyhow::{Context, Result, bail};
use image::{ImageBuffer, Rgb};
use std::path::Path;

fn to_u16(img: &Image) -> ImageBuffer<Rgb<u16>, Vec<u16>> {
    let data: Vec<u16> = img
        .data
        .iter()
        .map(|&v| if v.is_finite() { (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16 } else { 0 })
        .collect();
    ImageBuffer::from_raw(img.width as u32, img.height as u32, data).expect("buffer matches dimensions")
}

/// 8-bit RGB bytes (also used for on-screen textures).
pub fn to_rgb8(img: &Image) -> Vec<u8> {
    img.data
        .iter()
        .map(|&v| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 } else { 0 })
        .collect()
}

/// Save by file extension: `.tif`/`.tiff` and `.png` are 16-bit, `.jpg`/
/// `.jpeg` 8-bit, `.fit`/`.fits`/`.fts` 32-bit float. Values are expected in
/// 0..1 (FITS is written unclamped).
pub fn save(path: &Path, img: &Image) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "tif" | "tiff" | "png" => {
            to_u16(img).save(path).with_context(|| format!("writing {}", path.display()))
        }
        "jpg" | "jpeg" => {
            let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_raw(img.width as u32, img.height as u32, to_rgb8(img))
                    .expect("buffer matches dimensions");
            let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
            let mut w = std::io::BufWriter::new(file);
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, 95);
            buf.write_with_encoder(enc).with_context(|| format!("writing {}", path.display()))
        }
        "fit" | "fits" | "fts" => save_fits(path, img, &[]),
        _ => bail!("unsupported export format: .{ext} (use tif, png, jpg or fits)"),
    }
}

pub fn save_fits(path: &Path, img: &Image, cards: &[(String, String)]) -> Result<()> {
    fits::write(
        path,
        &FitsImage {
            width: img.width,
            height: img.height,
            channels: 3,
            bitpix: -32,
            data: img.data.clone(),
            cards: cards.to_vec(),
        },
    )
}

/// Load an RGB float FITS (as written by `save_fits`), with its header cards.
pub fn load_fits(path: &Path) -> Result<(Image, FitsImage)> {
    let mut f = fits::read(path)?;
    let data = std::mem::take(&mut f.data);
    let img = match f.channels {
        3 => Image { width: f.width, height: f.height, data },
        1 => Image { width: f.width, height: f.height, data: data.iter().flat_map(|&v| [v, v, v]).collect() },
        n => bail!("{}: {n}-channel FITS is not supported", path.display()),
    };
    Ok((img, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_roundtrips_or_at_least_decodes() {
        let mut img = Image::new(8, 6);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = i as f32 / 143.0;
        }
        let dir = std::env::temp_dir().join(format!("astro-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ext in ["tif", "png"] {
            let p = dir.join(format!("a.{ext}"));
            save(&p, &img).unwrap();
            let back = image::open(&p).unwrap().into_rgb16();
            assert_eq!(back.dimensions(), (8, 6));
            assert_eq!(back.as_raw()[143], 65535);
            assert_eq!(back.as_raw()[1], (65535.0 / 143.0 + 0.5) as u16, "{ext} keeps 16-bit precision");
        }
        let p = dir.join("a.jpg");
        save(&p, &img).unwrap();
        assert_eq!(image::open(&p).unwrap().into_rgb8().dimensions(), (8, 6));
        let p = dir.join("a.fits");
        save(&p, &img).unwrap();
        assert_eq!(load_fits(&p).unwrap().0, img);
        assert!(save(&dir.join("a.bmp"), &img).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
