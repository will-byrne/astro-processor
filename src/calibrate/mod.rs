//! Calibration: master frames, applying them to lights, hot-pixel repair.

pub mod apply;
pub mod hotpixel;
pub mod master;

pub use apply::calibrate;
pub use master::{CalibrationUse, MasterOptions, Masters};
