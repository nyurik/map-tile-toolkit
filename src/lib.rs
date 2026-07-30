#![doc = include_str!("../README.md")]

mod error;
pub use error::TileError;

mod tile;
pub use tile::TileId;

mod vertex;
pub use vertex::{Measured, Vertex};

// Low-level per-tile polyline clipping used by the slicer.
mod clip_polyline;

// Exact integer geometry predicates (orientation, point-in-ring) shared across clipping and polygons.
mod geom;

// The stateless slicing engine shared by both slicers.
mod grid;

mod slicer;
pub use slicer::{FeatureView, SlicerAll, SlicerOne, TileView};

mod mosaic;
pub use mosaic::Mosaic;
