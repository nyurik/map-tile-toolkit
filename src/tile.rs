//! Tile addressing on the integer grid.
//!
//! A tile of side `size` covers the closed integer square `[x·size, x·size + size − 1]` on each
//! axis, so the boundary between tiles `k−1` and `k` sits at `k·size − 0.5` (between two integer
//! coordinates). Integer vertices therefore never land exactly on a tile edge — every vertex
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

/// The tile containing coordinate `c` for the given tile `size`.
#[must_use]
pub fn tile_of(c: Coord<i32>, size: i32) -> TileId {
    TileId::new(c.x.div_euclid(size), c.y.div_euclid(size))
}

/// The closed integer bounds `(min, max)` of a tile (both inclusive).
#[must_use]
pub fn tile_bounds(tile: TileId, size: i32) -> (Coord<i32>, Coord<i32>) {
    let min = Coord {
        x: tile.x * size,
        y: tile.y * size,
    };
    let max = Coord {
        x: min.x + size - 1,
        y: min.y + size - 1,
    };
    (min, max)
}
