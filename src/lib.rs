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
pub use geom::signed_area_2x;

// Low-level per-tile polygon-ring clipping (keep-original with synthetic clip-boundary corners).
mod clip_polygon;

// The stateless slicing engine shared by both slicers.
mod grid;

mod slicer;
pub use slicer::{FeatureView, SlicerAll, SlicerOne, TileView};

// Borrowed views of clipped polygons, shared by both polygon slicers.
mod polygon_view;
pub use polygon_view::{PolygonView, RingView};

mod polygon_slicer;
pub use polygon_slicer::PolygonSlicerOne;

mod polygon_all;
pub use polygon_all::{FillRun, PolygonFeatureView, PolygonSlicerAll, PolygonTileView};

mod mosaic;
pub use mosaic::{Mosaic, PolygonMosaic};
