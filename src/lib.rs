//! Astro Processor core: RAW loading, calibration, registration, stacking and
//! non-destructive processing. Nothing in this library depends on the GUI.

pub mod calibrate;
pub mod combine;
pub mod debayer;
pub mod frames;
pub mod histogram;
pub mod image;
pub mod io;
pub mod process;
pub mod progress;
pub mod register;
pub mod stack;
