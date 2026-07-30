#![doc = include_str!("../README.md")]

mod error;
pub use error::TileError;

mod tile;
pub use tile::TileId;

mod vertex;
pub use vertex::{Measured, PolyVertex, Vertex};

// Low-level per-tile polyline clipping used by the slicer.
mod clip_polyline;

// Exact integer geometry predicates (orientation, point-in-ring) shared across clipping and polygons.
mod geom;

// Low-level per-tile polygon-ring clipping (keep-original with synthetic clip-boundary corners).
mod clip_polygon;

// The stateless slicing engine shared by both slicers.
mod grid;

mod slicer;
pub use slicer::{FeatureView, SlicerAll, SlicerOne, TileView};

mod polygon_slicer;
pub use polygon_slicer::{PolyFeatureView, PolygonSlicerOne, RingView};

mod mosaic;
pub use mosaic::Mosaic;
