#![doc = include_str!("../README.md")]

mod error;
pub use error::Error;

mod tile;
pub use tile::TileId;

mod vertex;
pub use vertex::{Measured, Vertex};

// Low-level per-tile polyline clipping used by the slicer.
mod clip_polyline;

mod merge;
mod slicer;

pub use slicer::Slicer;
