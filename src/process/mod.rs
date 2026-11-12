//! Non-destructive processing: a serialisable parameter set applied to the
//! untouched linear stack, in a fixed order.

pub mod curves;
pub mod detail;
pub mod geometry;
pub mod gradient;
pub mod history;
pub mod params;
pub mod pipeline;
pub mod stars;
pub mod tone;

pub use params::ProcessParams;
pub use pipeline::{Model, Region, analyze, process_full, render};
