#![doc = include_str!("../README.md")]

mod tile;
pub use tile::TileId;

// Low-level per-tile polyline clipping used by the slicer.
mod clip_polyline;

mod slicer;
pub use slicer::Slicer;
