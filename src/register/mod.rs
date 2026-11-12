//! Registration: find stars, match them between frames, resample.

pub mod matching;
pub mod stars;
pub mod warp;

pub use matching::{Affine, Match, find_transform};
pub use stars::{Detection, Star, detect};
