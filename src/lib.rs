#![doc = include_str!("../README.md")]

mod error;
pub use error::TileError;

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

mod mosaic;
pub use mosaic::Mosaic;

// Optional `geo-types` `Geometry` bridge for the accumulator. The core API is geo-free.
#[cfg(feature = "geo")]
mod geo;
