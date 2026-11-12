//! Minimal FITS reader/writer (1 or 3 planes). Files are written as 32-bit
//! float; every standard sample format is read.
//!
//! Hand-rolled to avoid a C dependency on cfitsio. Rows are stored bottom-up,
//! which is what Siril, PixInsight and most astro tools expect.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const BLOCK: usize = 2880;

/// Planar-agnostic container: `data` is interleaved, `channels` per pixel.
#[derive(Clone, Debug)]
pub struct FitsImage {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    /// Sample format the file was stored in (FITS `BITPIX`): 8, 16, 32 or
    /// 64 for integers, -32 or -64 for floats. Not compared or written.
    pub bitpix: i32,
    pub data: Vec<f32>,
    /// Extra header cards (keyword, value). Keywords are at most 8 ASCII
    /// characters; values at most 68.
    pub cards: Vec<(String, String)>,
}

impl PartialEq for FitsImage {
    fn eq(&self, o: &Self) -> bool {
        (self.width, self.height, self.channels) == (o.width, o.height, o.channels)
            && self.data == o.data
            && self.cards == o.cards
    }
}

impl FitsImage {
    pub fn card(&self, key: &str) -> Option<&str> {
        self.cards.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn card_f32(&self, key: &str) -> Option<f32> {
        self.card(key)?.trim().parse().ok()
    }
}

fn card(key: &str, value: &str) -> [u8; 80] {
    let mut c = [b' '; 80];
    let s = format!("{key:<8}= {value:>20}");
    let n = s.len().min(80);
    c[..n].copy_from_slice(&s.as_bytes()[..n]);
    c
}

fn string_card(key: &str, value: &str) -> [u8; 80] {
    let clean: String =
        value.chars().filter(|c| c.is_ascii() && !c.is_ascii_control()).take(66).collect();
    let quoted = format!("'{:<8}'", clean.replace('\'', "''"));
    let mut c = [b' '; 80];
    let s = format!("{key:<8}= {quoted}");
    let n = s.len().min(80);
    c[..n].copy_from_slice(&s.as_bytes()[..n]);
    c
}

pub fn write(path: &Path, img: &FitsImage) -> Result<()> {
    let (w, h, ch) = (img.width, img.height, img.channels);
    if img.data.len() != w * h * ch {
        bail!("FITS data length does not match dimensions");
    }
    let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut out = BufWriter::with_capacity(1 << 20, f);

    let mut header: Vec<[u8; 80]> = vec![
        card("SIMPLE", "T"),
        card("BITPIX", "-32"),
        card("NAXIS", if ch == 1 { "2" } else { "3" }),
        card("NAXIS1", &w.to_string()),
        card("NAXIS2", &h.to_string()),
    ];
    if ch != 1 {
        header.push(card("NAXIS3", &ch.to_string()));
    }
    header.push(string_card("ROWORDER", "BOTTOM-UP"));
    for (k, v) in &img.cards {
        let key: String = k.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(8).collect();
        header.push(string_card(&key.to_ascii_uppercase(), v));
    }
    let mut end = [b' '; 80];
    end[..3].copy_from_slice(b"END");
    header.push(end);
    let mut bytes = 0;
    for c in &header {
        out.write_all(c)?;
        bytes += 80;
    }
    out.write_all(&vec![b' '; (BLOCK - bytes % BLOCK) % BLOCK])?;

    let mut row = vec![0u8; w * 4];
    for c in 0..ch {
        for y in (0..h).rev() {
            for x in 0..w {
                let v = img.data[(y * w + x) * ch + c];
                row[x * 4..x * 4 + 4].copy_from_slice(&v.to_be_bytes());
            }
            out.write_all(&row)?;
        }
    }
    let data_bytes = w * h * ch * 4;
    out.write_all(&vec![0u8; (BLOCK - data_bytes % BLOCK) % BLOCK])?;
    out.flush()?;
    Ok(())
}

pub fn read(path: &Path) -> Result<FitsImage> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut cards: Vec<(String, String)> = Vec::new();
    let mut block = [0u8; BLOCK];
    'header: loop {
        r.read_exact(&mut block).context("truncated FITS header")?;
        for c in block.chunks_exact(80) {
            let key = String::from_utf8_lossy(&c[..8]).trim().to_string();
            if key == "END" {
                break 'header;
            }
            if c[8] != b'=' {
                continue;
            }
            let raw = String::from_utf8_lossy(&c[10..]).to_string();
            let value = if let Some(rest) = raw.trim_start().strip_prefix('\'') {
                // Quoted string: ends at the first quote not followed by another.
                let mut s = String::new();
                let mut chars = rest.chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '\'' {
                        if chars.peek() == Some(&'\'') {
                            s.push('\'');
                            chars.next();
                        } else {
                            break;
                        }
                    } else {
                        s.push(ch);
                    }
                }
                s.trim_end().to_string()
            } else {
                raw.split('/').next().unwrap_or("").trim().to_string()
            };
            cards.push((key, value));
        }
    }
    let get = |k: &str| cards.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let num = |k: &str| -> Result<usize> {
        get(k).and_then(|v| v.parse().ok()).with_context(|| format!("FITS header is missing {k}"))
    };
    let bitpix: i32 = get("BITPIX").and_then(|v| v.parse().ok()).context("FITS header is missing BITPIX")?;
    if ![8, 16, 32, 64, -32, -64].contains(&bitpix) {
        bail!("{}: unsupported FITS sample format (BITPIX {bitpix})", path.display());
    }
    // Physical value = BZERO + BSCALE * stored (how unsigned 16-bit is kept).
    let float = |k: &str, default: f64| get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(default);
    let (bzero, bscale) = (float("BZERO", 0.0), float("BSCALE", 1.0));
    let scaled = bzero != 0.0 || bscale != 1.0;
    let naxis = num("NAXIS")?;
    let (w, h) = (num("NAXIS1")?, num("NAXIS2")?);
    let ch = if naxis >= 3 { num("NAXIS3")? } else { 1 };
    if naxis < 2 || w == 0 || h == 0 || !(ch == 1 || ch == 3) {
        bail!("{}: unsupported FITS layout", path.display());
    }
    let bottom_up = get("ROWORDER").map(|v| !v.eq_ignore_ascii_case("TOP-DOWN")).unwrap_or(true);

    let mut data = vec![0f32; w * h * ch];
    let size = bitpix.unsigned_abs() as usize / 8;
    let mut row = vec![0u8; w * size];
    for c in 0..ch {
        for i in 0..h {
            r.read_exact(&mut row).context("truncated FITS data")?;
            let y = if bottom_up { h - 1 - i } else { i };
            for (x, b) in row.chunks_exact(size).enumerate() {
                let stored = match bitpix {
                    -32 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
                    -64 => f64::from_be_bytes(b.try_into().expect("8 bytes")),
                    8 => b[0] as f64,
                    16 => i16::from_be_bytes([b[0], b[1]]) as f64,
                    32 => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
                    _ => i64::from_be_bytes(b.try_into().expect("8 bytes")) as f64,
                };
                let v = if scaled { bzero + bscale * stored } else { stored };
                data[(y * w + x) * ch + c] = v as f32;
            }
        }
    }
    const STRUCTURAL: [&str; 9] =
        ["SIMPLE", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "NAXIS3", "ROWORDER", "BZERO", "BSCALE"];
    cards.retain(|(k, _)| !STRUCTURAL.contains(&k.as_str()));
    Ok(FitsImage { width: w, height: h, channels: ch, bitpix, data, cards })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(ch: usize) {
        let (w, h) = (37, 11);
        let data: Vec<f32> = (0..w * h * ch).map(|i| (i as f32 * 0.37).sin()).collect();
        let img = FitsImage {
            width: w,
            height: h,
            channels: ch,
            bitpix: -32,
            data,
            cards: vec![("WB_R".into(), "1.953125".into()), ("NOTE".into(), "it's a test".into())],
        };
        let dir = std::env::temp_dir().join(format!("astro-fits-test-{}-{ch}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.fits");
        write(&path, &img).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len() as usize % BLOCK, 0);
        let back = read(&path).unwrap();
        assert_eq!(back, img);
        assert_eq!(back.card_f32("WB_R"), Some(1.953125));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mono_roundtrip() {
        roundtrip(1);
    }

    #[test]
    fn rgb_roundtrip() {
        roundtrip(3);
    }
}
