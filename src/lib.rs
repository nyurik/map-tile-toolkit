#![doc = include_str!("../README.md")]

mod error;
pub use error::Error;

mod tile;
pub use tile::TileId;

mod vertex;
pub use vertex::{Measured, Vertex};

// Low-level per-tile polyline clipping used by the slicer.
mod clip_polyline;

// The stateless slicing engine shared by both slicers.
mod grid;

mod slicer;
pub use slicer::{FeatureView, SlicerAll, SlicerOne, TileView};

mod merge;
pub use merge::merge;

// Optional `geo-types` `Geometry` bridge for the accumulator. The core API is geo-free.
#[cfg(feature = "geo")]
mod geo;
