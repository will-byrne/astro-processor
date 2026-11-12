//! Crop, rotate and flip. Applied to the stack before everything else.

use super::params::Geometry;
use crate::image::Image;
use rayon::prelude::*;

pub fn is_identity(g: &Geometry) -> bool {
    *g == Geometry::default()
}

fn rotate_fine(img: &Image, degrees: f32) -> Image {
    let (w, h) = (img.width, img.height);
    let (s, c) = degrees.to_radians().sin_cos();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            // Inverse map: rotate the output position back into the source.
            let (sx, sy) = (cx + c * dx - s * dy, cy + s * dx + c * dy);
            if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f32 || sy > (h - 1) as f32 {
                continue;
            }
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (ax, ay) = (sx - x0 as f32, sy - y0 as f32);
            let (p00, p10, p01, p11) = (img.px(x0, y0), img.px(x1, y0), img.px(x0, y1), img.px(x1, y1));
            for ch in 0..3 {
                row[x * 3 + ch] = (p00[ch] * (1.0 - ax) + p10[ch] * ax) * (1.0 - ay)
                    + (p01[ch] * (1.0 - ax) + p11[ch] * ax) * ay;
            }
        }
    });
    out
}

/// Pixel rectangle (x, y, w, h) for a fractional crop of a `w` x `h` image.
pub fn crop_rect(crop: &[f32; 4], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let l = crop[0].clamp(0.0, 1.0);
    let t = crop[1].clamp(0.0, 1.0);
    let r = crop[2].clamp(0.0, 1.0);
    let b = crop[3].clamp(0.0, 1.0);
    let x0 = ((l.min(r) * w as f32).round() as usize).min(w.saturating_sub(1));
    let y0 = ((t.min(b) * h as f32).round() as usize).min(h.saturating_sub(1));
    let x1 = ((l.max(r) * w as f32).round() as usize).clamp(x0 + 1, w);
    let y1 = ((t.max(b) * h as f32).round() as usize).clamp(y0 + 1, h);
    (x0, y0, x1 - x0, y1 - y0)
}

pub fn apply(img: &Image, g: &Geometry) -> Image {
    if is_identity(g) {
        return img.clone();
    }
    let rotated;
    let src = if g.angle.abs() > 1e-4 {
        rotated = rotate_fine(img, g.angle);
        &rotated
    } else {
        img
    };
    let (x0, y0, cw, ch) = crop_rect(&g.crop, src.width, src.height);
    let cropped = src.crop(x0, y0, cw, ch);
    let turns = g.quarter_turns % 4;
    if turns == 0 && !g.flip_h && !g.flip_v {
        return cropped;
    }
    let (w, h) = (cropped.width, cropped.height);
    let (ow, oh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = Image::new(ow, oh);
    out.data.par_chunks_mut(ow * 3).enumerate().for_each(|(oy, row)| {
        for ox in 0..ow {
            // Undo the flips, then the clockwise turns.
            let fx = if g.flip_h { ow - 1 - ox } else { ox };
            let fy = if g.flip_v { oh - 1 - oy } else { oy };
            let (sx, sy) = match turns {
                0 => (fx, fy),
                1 => (fy, h - 1 - fx),
                2 => (w - 1 - fx, h - 1 - fy),
                _ => (w - 1 - fy, fx),
            };
            row[ox * 3..ox * 3 + 3].copy_from_slice(&cropped.px(sx, sy));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for (i, p) in img.data.chunks_exact_mut(3).enumerate() {
            p.fill(i as f32);
        }
        img
    }

    fn values(img: &Image) -> Vec<u32> {
        img.data.chunks_exact(3).map(|p| p[0] as u32).collect()
    }

    #[test]
    fn identity_and_crop() {
        let img = numbered(4, 3);
        assert_eq!(apply(&img, &Geometry::default()), img);
        let g = Geometry { crop: [0.25, 0.0, 0.75, 2.0 / 3.0], ..Geometry::default() };
        let out = apply(&img, &g);
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(values(&out), [1, 2, 5, 6]);
    }

    #[test]
    fn quarter_turns_and_flips() {
        // 0 1 2
        // 3 4 5
        let img = numbered(3, 2);
        let turn = |n| apply(&img, &Geometry { quarter_turns: n, ..Geometry::default() });
        let cw = turn(1);
        assert_eq!((cw.width, cw.height), (2, 3));
        assert_eq!(values(&cw), [3, 0, 4, 1, 5, 2]);
        assert_eq!(values(&turn(2)), [5, 4, 3, 2, 1, 0]);
        assert_eq!(values(&turn(3)), [2, 5, 1, 4, 0, 3]);
        let flipped = apply(&img, &Geometry { flip_h: true, ..Geometry::default() });
        assert_eq!(values(&flipped), [2, 1, 0, 5, 4, 3]);
        let flipped = apply(&img, &Geometry { flip_v: true, ..Geometry::default() });
        assert_eq!(values(&flipped), [3, 4, 5, 0, 1, 2]);
    }

    #[test]
    fn fine_rotation_keeps_the_centre_and_size() {
        let mut img = Image::new(21, 21);
        let c = (10 * 21 + 10) * 3;
        img.data[c..c + 3].fill(1.0);
        let out = apply(&img, &Geometry { angle: 30.0, ..Geometry::default() });
        assert_eq!((out.width, out.height), (21, 21));
        assert!((out.px(10, 10)[0] - 1.0).abs() < 1e-5);
    }
}
