//! Every user-adjustable processing setting. `Default` is the identity:
//! rendering with default parameters returns the stack unchanged (clamped to
//! 0..1).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    /// Fine rotation in degrees (counter-clockwise), applied first.
    pub angle: f32,
    /// Crop as fractions of the image: left, top, right, bottom edges.
    pub crop: [f32; 4],
    /// Number of clockwise quarter turns, applied after cropping.
    pub quarter_turns: u8,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self { angle: 0.0, crop: [0.0, 0.0, 1.0, 1.0], quarter_turns: 0, flip_h: false, flip_v: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gradient {
    pub enabled: bool,
    /// Polynomial degree of the background model (1 = plane ... 4).
    pub degree: u8,
    /// Samples brighter than the model by more than this many sigma are
    /// treated as object, not background. Lower protects the target more.
    pub tolerance: f32,
    /// 1 removes the modelled gradient completely.
    pub strength: f32,
}

impl Default for Gradient {
    fn default() -> Self {
        Self { enabled: false, degree: 2, tolerance: 1.0, strength: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Color {
    /// Shift each channel so the sky background is neutral grey.
    pub neutralize_background: bool,
    /// White balance multipliers (R, G, B).
    pub wb: [f32; 3],
}

impl Default for Color {
    fn default() -> Self {
        Self { neutralize_background: false, wb: [1.0; 3] }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stretch {
    /// Linear value mapped to black.
    pub black: f32,
    /// Midtones balance: the input value that maps to 0.5. 0.5 is linear.
    pub midtone: f32,
    /// Colour-preserving arcsinh pre-stretch strength; 0 is off.
    pub asinh: f32,
    /// Generalised hyperbolic stretch, applied after the arcsinh and before
    /// the midtones.
    pub ghs: Ghs,
}

impl Default for Stretch {
    fn default() -> Self {
        Self { black: 0.0, midtone: 0.5, asinh: 0.0, ghs: Ghs::default() }
    }
}

/// Generalised hyperbolic stretch (see `process::ghs`). Values are in the
/// 0..1 range left after the black point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ghs {
    /// ln(D + 1), 0..10; 0 is off.
    pub stretch: f32,
    /// Local intensity `b`, -5..15: how tightly the stretch is focused on
    /// the symmetry point.
    pub local_intensity: f32,
    /// Level where the stretch is strongest (usually the sky background).
    pub symmetry: f32,
    /// Below this the curve is linear, protecting the shadows.
    pub shadow_protect: f32,
    /// Above this the curve is linear, protecting the highlights.
    pub highlight_protect: f32,
    /// Stretch all channels by the same factor so colours do not wash out.
    pub preserve_color: bool,
}

impl Default for Ghs {
    fn default() -> Self {
        Self {
            stretch: 0.0,
            local_intensity: 0.0,
            symmetry: 0.0,
            shadow_protect: 0.0,
            highlight_protect: 1.0,
            preserve_color: false,
        }
    }
}

/// Richardson-Lucy deconvolution of the linear luminance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deconvolution {
    /// 0..1 blend of the deconvolved result; 0 is off.
    pub amount: f32,
    /// FWHM of the star profile in full-resolution pixels (measured from
    /// the stars); 0 means not measured yet, which disables the stage.
    pub psf_fwhm: f32,
    pub iterations: u32,
    /// Signal-to-noise (in background sigmas) needed before deconvolution
    /// applies in full. Higher keeps the sky smoother.
    pub noise_protection: f32,
    /// 0..1: how strictly dark rings around stars are prevented.
    pub deringing: f32,
}

impl Default for Deconvolution {
    fn default() -> Self {
        Self { amount: 0.0, psf_fwhm: 0.0, iterations: 20, noise_protection: 3.0, deringing: 1.0 }
    }
}

/// Large-scale contrast on the stretched image (like StarTools' Contrast and
/// HDR modules, or CLAHE).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalContrast {
    /// 0..1 boost of structure smaller than `size`; 0 is off.
    pub amount: f32,
    /// 0..1 compression of bright areas larger than `size`, revealing detail
    /// in galaxy cores and bright nebulae; 0 is off.
    pub hdr: f32,
    /// Scale of the structures, in full-resolution pixels.
    pub size: f32,
}

impl Default for LocalContrast {
    fn default() -> Self {
        Self { amount: 0.0, hdr: 0.0, size: 64.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Level {
    pub black: f32,
    pub gamma: f32,
    pub white: f32,
}

impl Default for Level {
    fn default() -> Self {
        Self { black: 0.0, gamma: 1.0, white: 1.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Levels {
    pub master: Level,
    pub r: Level,
    pub g: Level,
    pub b: Level,
}

/// Control points `[input, output]`, each in 0..1.
pub type CurvePoints = Vec<[f32; 2]>;

pub fn identity_curve() -> CurvePoints {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Curves {
    pub master: CurvePoints,
    pub r: CurvePoints,
    pub g: CurvePoints,
    pub b: CurvePoints,
}

impl Default for Curves {
    fn default() -> Self {
        Self { master: identity_curve(), r: identity_curve(), g: identity_curve(), b: identity_curve() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tone {
    /// Exposure compensation in stops.
    pub exposure: f32,
    /// -1..1
    pub contrast: f32,
    /// -1..1: negative recovers bright areas, positive brightens them.
    pub highlights: f32,
    /// -1..1: positive lifts dark areas, negative deepens them.
    pub shadows: f32,
    /// 1 is unchanged, 0 is monochrome.
    pub saturation: f32,
    /// 0..1: removal of excess green (typical of one-shot-colour cameras).
    pub green_removal: f32,
}

impl Default for Tone {
    fn default() -> Self {
        Self { exposure: 0.0, contrast: 0.0, highlights: 0.0, shadows: 0.0, saturation: 1.0, green_removal: 0.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    /// 0..1 luminance noise reduction.
    pub denoise_luma: f32,
    /// 0..1 colour noise reduction.
    pub denoise_chroma: f32,
    /// 0..2 fine-detail sharpening.
    pub sharpen: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stars {
    /// 0..1 how strongly stars are shrunk.
    pub reduce: f32,
    /// Shrink radius in full-resolution pixels.
    pub radius: f32,
}

impl Default for Stars {
    fn default() -> Self {
        Self { reduce: 0.0, radius: 1.5 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessParams {
    pub geometry: Geometry,
    pub gradient: Gradient,
    pub color: Color,
    pub deconvolution: Deconvolution,
    pub stretch: Stretch,
    pub levels: Levels,
    pub curves: Curves,
    pub tone: Tone,
    pub local_contrast: LocalContrast,
    pub detail: Detail,
    pub stars: Stars,
}
