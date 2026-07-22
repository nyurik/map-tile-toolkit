//! The public slicing API.

use geo_types::{Coord, Geometry, LineString};

use crate::clip_polyline::{assemble, clip_line, each_line, segment_intersects};
use crate::tile::{TileId, tile_of};

/// One segment routed into one tile during [`Slicer::slice_all`], packed into 8 bytes so the sort
/// moves little memory. `dx`/`dy` are the tile's offset from a reference tile (the first vertex's
/// tile): a polyline is geographically local, so the offsets fit `i16` with huge margin, and their
/// order equals absolute tile order (the reference is constant), so the derived `Ord` — `dx`, `dy`,
/// then `line`, `i0` — groups each tile's segments in original order. `line` and `i0` are the
/// segment's line index and its start vertex's **original** index; the end vertex is the next one
/// distinct from the start (consecutive duplicates were skipped when routing), so it is recovered
/// on lookup rather than stored. Within a tile a run continues only when the previous segment ended
/// where this one starts (same line, `prev end == i0`); any gap — or a line boundary — starts a
/// new piece.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Hit {
    dx: i16,
    dy: i16,
    line: u16,
    i0: u16,
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
    /// routed into each tile whose buffered box it touches, recording a compact [`Hit`] (tile as an
    /// `i16` offset from the first vertex's tile, plus the segment's line index and its endpoints'
    /// original vertex indices). Sorting the hits groups every tile's segments together in original
    /// order; within a tile a run grows while each segment starts where the previous ended (a gap —
    /// or a line boundary — starts a new piece), looking the endpoints back up from the input. This
    /// yields the same runs [`clip_line`] produces per tile, but without re-clipping the whole
    /// geometry once per tile and without copying vertices.
    ///
    /// # Panics
    ///
    /// Panics if `geom` spans more than `i16::MAX` (32767) tiles from its first vertex on either
    /// axis, or has a line with `≥ u16::MAX` vertices. Neither occurs for a normal polyline (e.g. an
    /// OSM way, capped at 2000 nodes and geographically local); both hold with enormous margin.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "line/vertex indices fit in u16 for a normal polyline; see # Panics"
    )]
    pub fn slice_all(self, geom: &Geometry<i32>) -> Vec<(TileId, Geometry<i32>)> {
        let lines = each_line(geom);

        // All hit tiles are stored as `i16` offsets from this reference tile (the first vertex's
        // tile). A polyline is local, so the offsets fit with huge margin; empty input → no tiles.
        let Some(first) = lines.iter().find_map(|l| l.0.first()).copied() else {
            return Vec::new();
        };
        let reference = tile_of(first, self.divider);

        // Reserve ~two hits per segment (a segment usually lands in one or two tiles); this is an
        // O(lines) estimate, so it stays cheap for a single huge line too.
        let segments: usize = lines.iter().map(|l| l.0.len().saturating_sub(1)).sum();
        let mut hits: Vec<Hit> = Vec::with_capacity(segments * 2);
        for (li, line) in lines.iter().enumerate() {
            let li = li as u16;
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
                        let dy = i16::try_from(ty - reference.y)
                            .expect("tile y offset fits i16 (polyline stays local)");
                        for tx in lo.x..=hi.x {
                            let tile = TileId::new(tx, ty);
                            let (min, max) = self.tile_bounds(tile);
                            if segment_intersects(a, c, min, max) {
                                let dx = i16::try_from(tx - reference.x)
                                    .expect("tile x offset fits i16 (polyline stays local)");
                                hits.push(Hit {
                                    dx,
                                    dy,
                                    line: li,
                                    i0: p as u16,
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
        let distinct = hits
            .windows(2)
            .filter(|w| (w[0].dx, w[0].dy) != (w[1].dx, w[1].dy))
            .count()
            + usize::from(!hits.is_empty());
        let mut out: Vec<(TileId, Geometry<i32>)> = Vec::with_capacity(distinct);
        let mut i = 0;
        while i < hits.len() {
            let (dx, dy) = (hits[i].dx, hits[i].dy);
            let tile = TileId::new(reference.x + i32::from(dx), reference.y + i32::from(dy));
            // Most tiles hold a single run; size for that and let it grow when a line re-enters.
            let mut pieces: Vec<LineString<i32>> = Vec::with_capacity(1);
            let mut cur: Vec<Coord<i32>> = Vec::new();
            let mut prev_end: Option<(u16, u16)> = None; // (line, end index) of the previous segment
            while i < hits.len() && (hits[i].dx, hits[i].dy) == (dx, dy) {
                let h = &hits[i];
                let verts = &lines[h.line as usize].0;
                let i0 = h.i0 as usize;
                let a = verts[i0];
                // End vertex: the next one distinct from the start (routing skipped consecutive
                // duplicates); a routed segment always has one, so `position` never fails.
                let end = i0
                    + 1
                    + verts[i0 + 1..]
                        .iter()
                        .position(|v| *v != a)
                        .expect("routed segment has a distinct end vertex");
                let c = verts[end];
                if prev_end == Some((h.line, h.i0)) && !cur.is_empty() {
                    cur.push(c); // segment starts where the previous ended: extend the open run
                } else {
                    if cur.len() >= 2 {
                        pieces.push(LineString(std::mem::take(&mut cur)));
                    }
                    cur = vec![a, c]; // start a new run
                }
                prev_end = Some((h.line, end as u16));
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
