#![doc = include_str!("../README.md")]

mod clip;

mod slice;
pub use slice::{for_each_tile_slice, slice_all_tiles, slice_tile};

mod tile;
pub use tile::{SliceOptions, TileId};

// Stubbed, not-yet-implemented eager stripe-slicer API (planetiler/geojson-vt style).
// The `tests/` suite ported from planetiler is the executable spec driving these.
pub mod extents;
pub mod geo_utils;
pub mod stripe;
