//! The public slicing API.

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
    divider: i32,
    /// Margin, in coordinate units, kept around every tile.
    buffer: i32,
}

impl Slicer {
    /// Create a slicer with the given tile side and buffer.
    #[must_use]
    pub const fn new(divider: u32, buffer: u32) -> Option<Self> {
        if divider == 0 || divider > i32::MAX.cast_unsigned() {
            None
        } else {
            Some(Self {
                divider: divider.cast_signed(),
                buffer: buffer.cast_signed(),
            })
        }
    }

    #[must_use]
    pub fn divider(self) -> u32 {
        self.divider.cast_unsigned()
    }

    #[must_use]
    pub fn buffer(self) -> u32 {
        self.buffer.cast_unsigned()
    }

    /// Clip `geom` to a single tile, keeping original vertices. Returns the same geometry kind
    /// (`LineString` for one run, `MultiLineString` for several), or `None` when nothing of `geom`
    /// touches the tile's (buffered) box.
    #[must_use]
    pub fn slice(self, geom: &Geometry<i32>, tile: TileId) -> Option<Geometry<i32>> {
        let (min, max) = self.tile_bounds(tile);
        let lines = each_line(geom);
        let mut pieces = Vec::with_capacity(lines.len());
        for line in lines {
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
        let lines = each_line(geom);

        // Candidate tiles: every tile whose buffered box a segment touches. Stream each line's
        // segments (dropping consecutive duplicates), and for each segment scan the tiles in its
        // coordinate bounding box (grown by the buffer), keeping the ones actually hit. Collect
        // into a `Vec` (with duplicates) then sort+dedup — cheaper than a `BTreeSet` for the small
        // tile counts here (no per-insert node allocation).
        let mut tiles: Vec<TileId> = Vec::new();
        for line in lines {
            let mut prev: Option<Coord<i32>> = None;
            for &c in &line.0 {
                if prev == Some(c) {
                    continue;
                }
                if let Some(a) = prev {
                    let lo = tile_of(
                        Coord {
                            x: a.x.min(c.x) - self.buffer,
                            y: a.y.min(c.y) - self.buffer,
                        },
                        self.divider,
                    );
                    let hi = tile_of(
                        Coord {
                            x: a.x.max(c.x) + self.buffer,
                            y: a.y.max(c.y) + self.buffer,
                        },
                        self.divider,
                    );
                    for ty in lo.y..=hi.y {
                        for tx in lo.x..=hi.x {
                            let tile = TileId::new(tx, ty);
                            let (min, max) = self.tile_bounds(tile);
                            if segment_intersects(a, c, min, max) {
                                tiles.push(tile);
                            }
                        }
                    }
                }
                prev = Some(c);
            }
        }
        tiles.sort_unstable();
        tiles.dedup();

        let mut out = Vec::with_capacity(tiles.len());
        for tile in tiles {
            let (min, max) = self.tile_bounds(tile);
            let mut pieces = Vec::with_capacity(lines.len());
            for line in lines {
                clip_line(line, min, max, &mut pieces);
            }
            if let Some(g) = assemble(pieces) {
                out.push((tile, g));
            }
        }
        out
    }

    /// The closed integer bounds `(min, max)` of `tile`'s clip box, grown by `buffer` on each side.
    #[inline]
    fn tile_bounds(self, tile: TileId) -> (Coord<i32>, Coord<i32>) {
        let base_x = tile.x * self.divider;
        let base_y = tile.y * self.divider;

        (
            Coord {
                x: base_x - self.buffer,
                y: base_y - self.buffer,
            },
            Coord {
                x: base_x + self.divider - 1 + self.buffer,
                y: base_y + self.divider - 1 + self.buffer,
            },
        )
    }
}
