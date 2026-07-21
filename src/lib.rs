#![doc = include_str!("../README.md")]

mod tile;
pub use tile::{TileId, tile_bounds, tile_of};

// One tile's worth of an integer polyline.
mod clip_polyline;
pub use clip_polyline::slice_tile;

// The whole polyline sliced into every tile it touches.
mod slice_all;
pub use slice_all::slice_all_tiles;
