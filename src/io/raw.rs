//! Camera RAW decoding (via `rawler`) into a float Bayer mosaic plus metadata.

use crate::image::{CfaPattern, Mono};
use anyhow::{Context, Result, anyhow, bail};
use rawler::decoders::RawDecodeParams;
use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "dng",
    "srw", "erf", "kdc", "dcr", "mos", "mef", "mrw", "3fr", "iiq", "ari",
];

pub fn is_raw_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Capture settings read from the file header (no pixel decoding).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RawMeta {
    pub exposure: Option<f64>,
    pub iso: Option<u32>,
    pub datetime: Option<String>,
    pub camera: String,
    pub width: usize,
    pub height: usize,
}

pub fn read_meta(path: &Path) -> Result<RawMeta> {
    let src = RawSource::new(path).with_context(|| format!("opening {}", path.display()))?;
    let decoder = rawler::get_decoder(&src).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let params = RawDecodeParams::default();
    let md = decoder
        .raw_metadata(&src, &params)
        .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let (width, height) = match decoder.raw_image(&src, &params, true) {
        Ok(img) => match img.active_area {
            Some(a) => (a.d.w, a.d.h),
            None => (img.width, img.height),
        },
        Err(_) => (0, 0),
    };
    let exif = md.exif;
    let exposure = exif
        .exposure_time
        .filter(|r| r.d != 0)
        .map(|r| r.n as f64 / r.d as f64);
    let iso = exif
        .iso_speed
        .or(exif.iso_speed_ratings.map(u32::from))
        .or(exif.recommended_exposure_index)
        .filter(|&i| i > 0);
    Ok(RawMeta {
        exposure,
        iso,
        datetime: exif.date_time_original.or(exif.create_date).or(exif.modify_date),
        camera: format!("{} {}", md.make, md.model).trim().to_string(),
        width,
        height,
    })
}

/// A decoded Bayer frame in raw sensor units (black level not subtracted).
#[derive(Clone, Debug)]
pub struct CfaFrame {
    pub mono: Mono,
    pub pattern: CfaPattern,
    /// Black level per position in the 2x2 pattern (same layout as `pattern`).
    pub black: [f32; 4],
    pub white: f32,
    /// As-shot white balance multipliers (R, G, B), G normalised to 1.
    pub wb: [f32; 3],
}

pub fn load_cfa(path: &Path) -> Result<CfaFrame> {
    let raw = rawler::decode_file(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    if raw.cpp != 1 {
        bail!("{}: not a Bayer RAW file ({} components per pixel)", path.display(), raw.cpp);
    }
    let cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(cfg) if cfg.cfa.width == 2 && cfg.cfa.height == 2 => {
            cfg.cfa.clone()
        }
        _ => bail!("{}: only 2x2 Bayer sensors are supported", path.display()),
    };
    let (x0, y0, w, h) = match raw.active_area {
        Some(a) if a.d.w > 0 && a.d.h > 0 => (a.p.x, a.p.y, a.d.w, a.d.h),
        _ => (0, 0, raw.width, raw.height),
    };
    // Keep even dimensions so the Bayer pattern tiles cleanly.
    let (w, h) = (w & !1, h & !1);

    let mut pattern = [0u8; 4];
    let mut black = [0f32; 4];
    let bl = &raw.blacklevel;
    for py in 0..2 {
        for px in 0..2 {
            let c = cfa.color_at(y0 + py, x0 + px);
            pattern[py * 2 + px] = if c > 2 { 1 } else { c as u8 };
            let (bx, by) = ((x0 + px) % bl.width.max(1), (y0 + py) % bl.height.max(1));
            black[py * 2 + px] = bl
                .levels
                .get((by * bl.width + bx) * bl.cpp)
                .or(bl.levels.first())
                .map(|r| r.as_f32())
                .unwrap_or(0.0);
        }
    }
    let white = raw.whitelevel.0.iter().copied().min().unwrap_or(65535) as f32;

    let mut mono = Mono::new(w, h);
    let stride = raw.width;
    match &raw.data {
        RawImageData::Integer(d) => {
            for y in 0..h {
                let src = &d[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + w];
                for (o, &v) in mono.data[y * w..(y + 1) * w].iter_mut().zip(src) {
                    *o = v as f32;
                }
            }
        }
        RawImageData::Float(d) => {
            for y in 0..h {
                let src = &d[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + w];
                mono.data[y * w..(y + 1) * w].copy_from_slice(src);
            }
        }
    }

    let c = raw.wb_coeffs;
    let wb = if c[0].is_finite() && c[1].is_finite() && c[2].is_finite() && c[1] > 0.0 {
        [c[0] / c[1], 1.0, c[2] / c[1]]
    } else {
        [1.0, 1.0, 1.0]
    };
    Ok(CfaFrame { mono, pattern, black, white, wb })
}
