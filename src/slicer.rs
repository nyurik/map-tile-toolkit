//! The public slicing API.

use geo_types::{Coord, Geometry, LineString};

use crate::clip_polyline::{assemble, clip_line, each_line, segment_intersects};
use crate::tile::{TileId, tile_of};

/// One segment routed into one tile during [`Slicer::slice_all`]. Sorting hits (derived `Ord`:
/// `tile`, then `line`, then `i0`) groups every tile's segments together in original order. `i0`
/// and `i1` are the segment endpoints' **original** vertex indices in `line`, so endpoints are
/// looked up (no coordinate copies) and consecutive-duplicate vertices simply make `i1 > i0 + 1`.
/// Within a tile a run continues only when the previous segment ended where this one starts
/// (`prev.i1 == i0`, same line); any gap — or a line boundary — starts a new piece.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Hit {
    tile: TileId,
    line: u32,
    i0: u32,
    i1: u32,
}

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
    ///
    /// The geometry is walked **once**. Every segment (consecutive-duplicate vertices skipped) is
    /// routed into each tile whose buffered box it touches, recording a [`Hit`] of `(tile, line,
    /// i0, i1)` — the segment endpoints' original vertex indices. Sorting the hits groups every
    /// tile's segments together in original order; within a tile a run grows while each segment
    /// starts where the previous ended (a gap — or a line boundary — starts a new piece), looking
    /// the endpoints back up from the input. This yields the same runs [`clip_line`] produces per
    /// tile, but without re-clipping the whole geometry once per tile and without copying vertices.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "line/vertex indices fit in u32 for any realistic polyline (u32::MAX vertices is >32 GB)"
    )]
    pub fn slice_all(self, geom: &Geometry<i32>) -> Vec<(TileId, Geometry<i32>)> {
        let lines = each_line(geom);

        // Reserve ~two hits per segment (a segment usually lands in one or two tiles); this is an
        // O(lines) estimate, so it stays cheap for a single huge line too.
        let segments: usize = lines.iter().map(|l| l.0.len().saturating_sub(1)).sum();
        let mut hits: Vec<Hit> = Vec::with_capacity(segments * 2);
        for (li, line) in lines.iter().enumerate() {
            let li = li as u32;
            // Carry the previous vertex (index + coordinate) so the segment start needs no re-index.
            let mut prev: Option<(usize, Coord<i32>)> = None;
            for (idx, &c) in line.0.iter().enumerate() {
                if let Some((p, a)) = prev {
                    if a == c {
                        continue; // drop a consecutive duplicate vertex (keep `prev` at `p`)
                    }
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
                                hits.push(Hit {
                                    tile,
                                    line: li,
                                    i0: p as u32,
                                    i1: idx as u32,
                                });
                            }
                        }
                    }
                }
                prev = Some((idx, c));
            }
        }

        // Group by tile (then original order within a tile) and assemble each tile's runs.
        hits.sort_unstable();
        // Presize the output to the number of distinct tiles (one cheap pass over the sorted hits)
        // so pushing results never reallocates.
        let distinct = hits.windows(2).filter(|w| w[0].tile != w[1].tile).count()
            + usize::from(!hits.is_empty());
        let mut out: Vec<(TileId, Geometry<i32>)> = Vec::with_capacity(distinct);
        let mut i = 0;
        while i < hits.len() {
            let tile = hits[i].tile;
            // Most tiles hold a single run; size for that and let it grow when a line re-enters.
            let mut pieces: Vec<LineString<i32>> = Vec::with_capacity(1);
            let mut cur: Vec<Coord<i32>> = Vec::new();
            let mut prev_end: Option<(u32, u32)> = None; // (line, i1) of the previous segment
            while i < hits.len() && hits[i].tile == tile {
                let h = &hits[i];
                let verts = &lines[h.line as usize].0;
                let (a, c) = (verts[h.i0 as usize], verts[h.i1 as usize]);
                if prev_end == Some((h.line, h.i0)) && !cur.is_empty() {
                    cur.push(c); // segment starts where the previous ended: extend the open run
                } else {
                    if cur.len() >= 2 {
                        pieces.push(LineString(std::mem::take(&mut cur)));
                    }
                    cur = vec![a, c]; // start a new run
                }
                prev_end = Some((h.line, h.i1));
                i += 1;
            }
            if cur.len() >= 2 {
                pieces.push(LineString(cur));
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
