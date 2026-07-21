//! Slice a whole integer polyline into every tile it touches.
//!
//! [`slice_all_tiles`] walks each segment once to find the tiles it crosses, then clips the whole
//! geometry against each of them. A tile produces a piece iff some segment touches it, so a segment
//! that merely crosses a tile (no vertex inside) still lands there. Each tile's piece is built with
//! the same per-line clip as [`crate::slice_tile`], so `slice_all_tiles(geom)[tile]` is identical
//! to `slice_tile(geom, tile)` for every tile — the batch and per-tile paths agree by construction.

use std::collections::{BTreeMap, BTreeSet};

use geo_types::Geometry;

use crate::clip_polyline::{assemble, clip_line, each_line, segment_intersects};
use crate::tile::{TileId, tile_bounds, tile_of};

/// Clip an integer polyline geometry into per-tile pieces, one entry per tile it touches, keyed by
/// [`TileId`] (ordered by `x`, then `y`). Pieces keep the geometry's original coordinates.
#[must_use]
pub fn slice_all_tiles(geom: &Geometry<i32>, tile_size: i32) -> BTreeMap<TileId, Geometry<i32>> {
    let lines = each_line(geom);

    // Candidate tiles: every tile that any segment touches. For each segment, scan the tiles in its
    // coordinate bounding box and keep those the segment actually intersects.
    let mut tiles = BTreeSet::new();
    for line in &lines {
        let mut pts: Vec<_> = Vec::with_capacity(line.0.len());
        for &c in &line.0 {
            if pts.last() != Some(&c) {
                pts.push(c);
            }
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (ta, tb) = (tile_of(a, tile_size), tile_of(b, tile_size));
            for ty in ta.y.min(tb.y)..=ta.y.max(tb.y) {
                for tx in ta.x.min(tb.x)..=ta.x.max(tb.x) {
                    let tile = TileId::new(tx, ty);
                    let (min, max) = tile_bounds(tile, tile_size);
                    if segment_intersects(a, b, min, max) {
                        tiles.insert(tile);
                    }
                }
            }
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
