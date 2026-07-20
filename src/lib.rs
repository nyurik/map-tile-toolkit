#![doc = include_str!("../README.md")]

mod clip;

mod slice;
pub use slice::{for_each_tile_slice, slice_all_tiles, slice_tile};

mod tile;
pub use tile::{SliceOptions, TileId};
