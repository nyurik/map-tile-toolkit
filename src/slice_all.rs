//! Slice a whole integer polyline into every tile it touches.
//!
//! [`slice_all_tiles`] walks the geometry once to find the tiles it touches, then clips it against
//! each of them. A tile can produce a piece only if a vertex falls inside it - the clip keeps a
//! vertex only when it, or a neighbor, is inside the tile. The candidate tiles are exactly
//! the tiles that contain a vertex. Each tile's piece is built with the same per-line clip as
//! [`crate::slice_tile`], so `slice_all_tiles(geom)[tile]` is identical to `slice_tile(geom, tile)`
//! for every tile — the batch and per-tile paths agree by construction.

use std::collections::{BTreeMap, BTreeSet};

use geo_types::Geometry;

use crate::clip_polyline::{assemble, clip_line, each_line};
use crate::tile::{TileId, tile_bounds, tile_of};

/// Clip an integer polyline geometry into per-tile pieces, one entry per tile it touches, keyed by
/// [`TileId`] (ordered by `x`, then `y`). Pieces keep the geometry's original coordinates.
#[must_use]
pub fn slice_all_tiles(geom: &Geometry<i32>, tile_size: i32) -> BTreeMap<TileId, Geometry<i32>> {
    let lines = each_line(geom);

    // Candidate tiles: those containing at least one vertex. A tile with no vertex inside it keeps
    // nothing, so this set is exact.
    let mut tiles = BTreeSet::new();
    for line in &lines {
        for &c in &line.0 {
            tiles.insert(tile_of(c, tile_size));
        }
    }

    let mut out = BTreeMap::new();
    for tile in tiles {
        let (min, max) = tile_bounds(tile, tile_size);
        let mut pieces = Vec::new();
        for line in &lines {
            clip_line(line, min, max, &mut pieces);
        }
        if let Some(g) = assemble(pieces) {
            out.insert(tile, g);
        }
    }
    out
}
