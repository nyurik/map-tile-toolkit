//! Tile addressing on the integer grid.
//!
//! A tile of side `extent` covers the closed integer square `[x·extent, x·extent + extent − 1]` on
//! each axis, so the boundary between tiles `k−1` and `k` sits at `k·extent − 0.5` (between two
//! integer coordinates). Integer vertices therefore never land exactly on a tile edge — every vertex
//! belongs to exactly one tile.

use geo_types::Coord;

/// A tile address on the integer grid. Coordinates may be negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub x: i32,
    pub y: i32,
}

impl TileId {
    #[must_use]
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl From<(i32, i32)> for TileId {
    fn from((x, y): (i32, i32)) -> Self {
        Self { x, y }
    }
}

/// The tile that owns coordinate `c` for the given tile side `extent`.
pub(crate) fn tile_of(c: Coord<i32>, extent: i32) -> TileId {
    TileId::new(c.x.div_euclid(extent), c.y.div_euclid(extent))
}
