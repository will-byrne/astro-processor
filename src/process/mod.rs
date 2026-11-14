//! Non-destructive processing: a serialisable parameter set applied to the
//! untouched linear stack, in a fixed order.

pub mod color;
pub mod curves;
pub mod deconv;
pub mod detail;
pub mod geometry;
pub mod ghs;
pub mod gradient;
pub mod history;
pub mod local_contrast;
pub mod params;
pub mod pipeline;
pub mod stars;
pub mod tone;

pub use params::ProcessParams;
pub use pipeline::{Model, Region, analyze, context_radius, process_full, render};
