//! The public slicing API.

use std::collections::BTreeSet;

use geo_types::{Coord, Geometry};

use crate::clip_polyline::{assemble, clip_line, each_line, segment_intersects};
use crate::tile::{TileId, tile_of};

/// Slices integer polylines ([`Geometry::LineString`] / [`Geometry::MultiLineString`]) into
/// per-tile pieces on an integer grid.
///
/// A tile of side [`divider`](Self::divider) covers the closed square
/// `[x·divider, x·divider + divider − 1]` on each axis. Each tile's clip box is then grown outward
/// by [`buffer`](Self::buffer) units on every side, so geometry within `buffer` of a tile is kept
/// in it. Clipping keeps the geometry's original vertices and includes every tile a segment passes
/// through. `divider` must be non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slicer {
    /// Tile side length, in coordinate units (must be non-zero).
    pub divider: u32,
    /// Margin, in coordinate units, kept around every tile.
    pub buffer: u32,
}

impl Slicer {
    /// Create a slicer with the given tile side and buffer.
    #[must_use]
    pub fn new(divider: u32, buffer: u32) -> Self {
        Self { divider, buffer }
    }

    /// `divider` and `buffer` as `i32` (the coordinate type).
    fn params(self) -> (i32, i32) {
        (
            i32::try_from(self.divider).expect("divider fits in i32"),
            i32::try_from(self.buffer).expect("buffer fits in i32"),
        )
    }

    /// Clip `geom` to a single tile, keeping original vertices. Returns the same geometry kind
    /// (`LineString` for one run, `MultiLineString` for several), or `None` when nothing of `geom`
    /// touches the tile's (buffered) box.
    #[must_use]
    pub fn slice(self, geom: &Geometry<i32>, tile: TileId) -> Option<Geometry<i32>> {
        let (divider, buffer) = self.params();
        let (min, max) = tile_bounds(tile, divider, buffer);
        let mut pieces = Vec::new();
        for line in each_line(geom) {
            clip_line(line, min, max, &mut pieces);
        }
        assemble(pieces)
    }

    /// Clip `geom` into every tile it touches, as `(tile, piece)` pairs ordered by [`TileId`].
    ///
    /// `self.slice_all(geom)` and `self.slice(geom, tile)` agree by construction: the pair for a
    /// tile equals what `slice` returns for it.
    #[must_use]
    pub fn slice_all(self, geom: &Geometry<i32>) -> Vec<(TileId, Geometry<i32>)> {
        let (divider, buffer) = self.params();
        let lines = each_line(geom);

        // Candidate tiles: every tile whose buffered box a segment touches. Stream each line's
        // segments (dropping consecutive duplicates), and for each segment scan the tiles in its
        // coordinate bounding box (grown by the buffer), keeping the ones actually hit.
        let mut tiles = BTreeSet::new();
        for line in lines {
            let mut prev: Option<Coord<i32>> = None;
            for &c in &line.0 {
                if prev == Some(c) {
                    continue;
                }
                if let Some(a) = prev {
                    let lo = tile_of(
                        Coord {
                            x: a.x.min(c.x) - buffer,
                            y: a.y.min(c.y) - buffer,
                        },
                        divider,
                    );
                    let hi = tile_of(
                        Coord {
                            x: a.x.max(c.x) + buffer,
                            y: a.y.max(c.y) + buffer,
                        },
                        divider,
                    );
                    for ty in lo.y..=hi.y {
                        for tx in lo.x..=hi.x {
                            let tile = TileId::new(tx, ty);
                            let (min, max) = tile_bounds(tile, divider, buffer);
                            if segment_intersects(a, c, min, max) {
                                tiles.insert(tile);
                            }
                        }
                    }
                }
                prev = Some(c);
            }
        }

        // Reuse one `pieces` buffer across tiles; `assemble` takes ownership of the runs.
        let mut out = Vec::with_capacity(tiles.len());
        let mut pieces = Vec::new();
        for tile in tiles {
            let (min, max) = tile_bounds(tile, divider, buffer);
            for line in lines {
                clip_line(line, min, max, &mut pieces);
            }
            if let Some(g) = assemble(std::mem::take(&mut pieces)) {
                out.push((tile, g));
            }
        }
        out
    }
}

/// The closed integer bounds `(min, max)` of `tile`'s clip box, grown by `buffer` on each side.
fn tile_bounds(tile: TileId, divider: i32, buffer: i32) -> (Coord<i32>, Coord<i32>) {
    let base_x = tile.x * divider;
    let base_y = tile.y * divider;
    (
        Coord {
            x: base_x - buffer,
            y: base_y - buffer,
        },
        Coord {
            x: base_x + divider - 1 + buffer,
            y: base_y + divider - 1 + buffer,
        },
    )
}
