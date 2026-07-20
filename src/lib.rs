#![doc = include_str!("../README.md")]

mod clip;

mod slice;
pub use slice::{for_each_tile_slice, slice_all_tiles, slice_tile};

mod tile;
pub use tile::{SliceOptions, TileId};

// Eager stripe slicer: slice one geometry into every tile it touches at a zoom, with
// interior fill detection and antimeridian wrapping. `extents` filters to in-bounds tiles;
// `geo_utils` holds shared geometry helpers.
pub mod extents;
pub mod geo_utils;
pub mod stripe;
