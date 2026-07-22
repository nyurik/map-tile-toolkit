//! The public slicing API.

use geo_types::{Coord, Geometry, LineString};

use crate::Error;
use crate::clip_polyline::{assemble, clip_line, each_line, segment_intersects};
use crate::tile::{TileId, tile_of};

/// One segment routed into one tile during [`Slicer::slice_all`], packed into 8 bytes so the sort
/// moves little memory. `dx`/`dy` are the tile's offset from a reference tile (the first vertex's
/// tile): a polyline is geographically local, so the offsets fit `i16` (checked up front), and their
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

/// A line index or vertex index in `0..=u16::MAX` fits the compact [`Hit`]; a length up to this many
/// therefore has all indices representable.
const MAX_INDEXED_LEN: usize = u16::MAX as usize + 1;

/// Upper bound on the candidate tiles [`Slicer::slice_all`] will examine before giving up with
/// [`Error::TooManyTiles`]. Far above any realistic polyline (a local way examines a handful per
/// segment), it caps worst-case time and memory for adversarial, widely-spread input. ~33M tests is
/// well under a second.
const MAX_TILE_VISITS: i64 = 1 << 25;

/// Slices integer polylines ([`Geometry::LineString`] / [`Geometry::MultiLineString`]) into
/// per-tile pieces on an integer grid.
///
/// A tile of side [`divider`](Self::divider) covers the closed square
/// `[x·divider, x·divider + divider − 1]` on each axis. Each tile's clip box is then grown outward
/// by [`buffer`](Self::buffer) units on every side, so geometry within `buffer` of a tile is kept
/// in it. Clipping keeps the geometry's original vertices and includes every tile a segment passes
/// through.
///
/// The slicer never panics: bad input (a non-polyline geometry, an oversized polyline, or
/// coordinates that overflow the tile math) yields an [`Error`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slicer {
    /// Tile side length, in coordinate units (always in `1..=i32::MAX`).
    divider: i32,
    /// Margin, in coordinate units, kept around every tile (always in `0..=u16::MAX`).
    buffer: i32,
}

impl Slicer {
    /// Create a slicer with the given tile side and buffer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDivider`] if `divider` is `0` or greater than `i32::MAX`.
    pub const fn new(divider: u32, buffer: u16) -> Result<Self, Error> {
        if divider == 0 || divider > i32::MAX.cast_unsigned() {
            Err(Error::InvalidDivider)
        } else {
            Ok(Self {
                divider: divider.cast_signed(),
                buffer: buffer as i32,
            })
        }
    }

    /// The tile side length, in coordinate units.
    #[must_use]
    pub fn divider(self) -> u32 {
        self.divider.cast_unsigned()
    }

    /// The buffer kept around every tile, in coordinate units.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "buffer is always in 0..=u16::MAX (it was built from a u16)"
    )]
    pub fn buffer(self) -> u16 {
        self.buffer as u16
    }

    /// Clip `geom` to a single tile, keeping original vertices. Returns the same geometry kind
    /// (`LineString` for one run, `MultiLineString` for several), or `None` when nothing of `geom`
    /// touches the tile's (buffered) box.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedGeometry`] if `geom` is not a polyline, or [`Error::Overflow`] if
    /// `tile`'s (buffered) box coordinates overflow `i32` (a tile far outside the representable
    /// coordinate range for this `divider`).
    pub fn slice(self, geom: &Geometry<i32>, tile: TileId) -> Result<Option<Geometry<i32>>, Error> {
        let lines = each_line(geom)?;
        let (min, max) = self.tile_bounds(tile)?;
        let mut pieces = Vec::with_capacity(lines.len());
        for line in lines {
            clip_line(line, min, max, &mut pieces);
        }
        Ok(assemble(pieces))
    }

    /// Clip `geom` into every tile it touches, as `(tile, piece)` pairs ordered by [`TileId`].
    ///
    /// `self.slice_all(geom)` and `self.slice(geom, tile)` agree by construction: the pair for a
    /// tile equals what `slice` returns for it.
    ///
    /// The geometry is walked **once**. Every segment (consecutive-duplicate vertices skipped) is
    /// routed into each tile whose buffered box it touches, recording a compact hit (tile as an
    /// `i16` offset from the first vertex's tile, plus the segment's line index and start vertex's
    /// original index). Sorting the hits groups every tile's segments together in original order;
    /// within a tile a run grows while each segment starts where the previous ended (a gap — or a
    /// line boundary — starts a new piece), looking the endpoints back up from the input. This
    /// yields the same runs the per-tile path produces, but without re-clipping the whole geometry
    /// once per tile and without copying vertices.
    ///
    /// Vertex/tile ranges are validated up front, so the hot loop packs into `Hit` without per-item
    /// bounds checks.
    ///
    /// # Errors
    ///
    /// - [`Error::UnsupportedGeometry`] — `geom` is not a polyline.
    /// - [`Error::PolylineTooLarge`] — a line has more than `u16::MAX` vertices, or there are more
    ///   than `u16::MAX` lines.
    /// - [`Error::TooManyTiles`] — the geometry spans more than `i16::MAX` tiles on an axis, or its
    ///   segments would collectively examine more than `MAX_TILE_VISITS` candidate tiles.
    /// - [`Error::Overflow`] — a coordinate `± buffer` overflows `i32`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "indices and tile offsets are validated to fit u16/i16 before these casts"
    )]
    pub fn slice_all(self, geom: &Geometry<i32>) -> Result<Vec<(TileId, Geometry<i32>)>, Error> {
        let lines = each_line(geom)?;

        // Empty geometry → no tiles.
        let Some(first) = lines.iter().find_map(|l| l.0.first()).copied() else {
            return Ok(Vec::new());
        };

        // Up-front validation, so the hot loop can pack into `Hit` without per-item checks:
        // (1) every line/vertex index fits u16;
        if lines.len() > MAX_INDEXED_LEN || lines.iter().any(|l| l.0.len() > MAX_INDEXED_LEN) {
            return Err(Error::PolylineTooLarge);
        }
        // (2) every tile offset from `reference` fits i16. The extreme tiles come from the overall
        // coordinate bounds grown by the buffer; `?` reports coordinates too close to the i32 edge.
        let reference = tile_of(first, self.divider);
        let (lo_tile, hi_tile) = self.buffered_tile_bounds(lines, first)?;
        for (tile, refc) in [
            (lo_tile.x, reference.x),
            (hi_tile.x, reference.x),
            (lo_tile.y, reference.y),
            (hi_tile.y, reference.y),
        ] {
            i16::try_from(i64::from(tile) - i64::from(refc)).map_err(|_| Error::TooManyTiles)?;
        }

        // Route each segment into the tiles it touches. All casts below are within the ranges just
        // validated; all coordinate arithmetic stays inside `[lo_tile, hi_tile]`, which is in range.
        let segments: usize = lines.iter().map(|l| l.0.len().saturating_sub(1)).sum();
        let mut hits: Vec<Hit> = Vec::with_capacity(segments.saturating_mul(2));
        // Bound the total candidate tiles examined, so an adversarial spread of long segments can't
        // exhaust time or memory: a geometry needing more than this is rejected rather than crashing.
        let mut budget: i64 = MAX_TILE_VISITS;
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
                    // Charge this segment's candidate-tile box (in range: lo/hi ∈ [lo_tile, hi_tile]).
                    budget -= (i64::from(hi.x) - i64::from(lo.x) + 1)
                        * (i64::from(hi.y) - i64::from(lo.y) + 1);
                    if budget < 0 {
                        return Err(Error::TooManyTiles);
                    }
                    for ty in lo.y..=hi.y {
                        let dy = (ty - reference.y) as i16;
                        for tx in lo.x..=hi.x {
                            let tile = TileId::new(tx, ty);
                            let (min, max) = self.tile_bounds(tile)?;
                            if segment_intersects(a, c, min, max) {
                                let dx = (tx - reference.x) as i16;
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

        Ok(Self::assemble_tiles(hits, lines, reference))
    }

    /// Group sorted hits by tile and assemble each tile's runs, looking endpoints back up from
    /// `lines`. Split out of [`Self::slice_all`] to keep each function focused.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`end` is a vertex index < line length, validated to fit u16 in slice_all"
    )]
    fn assemble_tiles(
        mut hits: Vec<Hit>,
        lines: &[LineString<i32>],
        reference: TileId,
    ) -> Vec<(TileId, Geometry<i32>)> {
        hits.sort_unstable();
        // Pre-alloc the output to the number of distinct tiles (one cheap pass over the sorted hits)
        // so pushing results never reallocates.
        let distinct = hits
            .windows(2)
            .filter(|w| (w[0].dx, w[0].dy) != (w[1].dx, w[1].dy))
            .count()
            + usize::from(!hits.is_empty());
        let mut out: Vec<(TileId, Geometry<i32>)> = Vec::with_capacity(distinct);

        for group in hits.chunk_by(|a, b| (a.dx, a.dy) == (b.dx, b.dy)) {
            let (dx, dy) = (group[0].dx, group[0].dy);
            let tile = TileId::new(reference.x + i32::from(dx), reference.y + i32::from(dy));
            // Most tiles hold a single run; size for that and let it grow when a line re-enters.
            let mut pieces: Vec<LineString<i32>> = Vec::with_capacity(1);
            let mut cur: Vec<Coord<i32>> = Vec::new();
            let mut prev_end: Option<(u16, u16)> = None; // (line, end index) of the previous segment
            for h in group {
                let verts = &lines[h.line as usize].0;
                let i0 = h.i0 as usize;
                let a = verts[i0];
                // End vertex: the next one distinct from the start (routing skipped consecutive
                // duplicates). A routed segment always has one; if somehow not, skip it (never
                // panic).
                let Some(step) = verts[i0 + 1..].iter().position(|v| *v != a) else {
                    continue;
                };
                let end = i0 + 1 + step;
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
    /// All arithmetic is checked; [`Error::Overflow`] means the tile lies outside the representable
    /// coordinate range for this `divider`.
    fn tile_bounds(self, tile: TileId) -> Result<(Coord<i32>, Coord<i32>), Error> {
        let base_x = tile.x.checked_mul(self.divider).ok_or(Error::Overflow)?;
        let base_y = tile.y.checked_mul(self.divider).ok_or(Error::Overflow)?;
        // Distance from the base corner to the far corner of the buffered box: divider - 1 + buffer.
        let reach = (self.divider - 1)
            .checked_add(self.buffer)
            .ok_or(Error::Overflow)?;
        Ok((
            Coord {
                x: base_x.checked_sub(self.buffer).ok_or(Error::Overflow)?,
                y: base_y.checked_sub(self.buffer).ok_or(Error::Overflow)?,
            },
            Coord {
                x: base_x.checked_add(reach).ok_or(Error::Overflow)?,
                y: base_y.checked_add(reach).ok_or(Error::Overflow)?,
            },
        ))
    }

    /// The lowest and highest tiles any part of `lines` can reach: the coordinate bounding box grown
    /// by `buffer`, mapped to tiles. `first` seeds the bounds. [`Error::Overflow`] if a coordinate
    /// `± buffer` overflows `i32`.
    fn buffered_tile_bounds(
        self,
        lines: &[LineString<i32>],
        first: Coord<i32>,
    ) -> Result<(TileId, TileId), Error> {
        let (mut min, mut max) = (first, first);
        for line in lines {
            for c in &line.0 {
                min.x = min.x.min(c.x);
                min.y = min.y.min(c.y);
                max.x = max.x.max(c.x);
                max.y = max.y.max(c.y);
            }
        }
        let lo = tile_of(
            Coord {
                x: min.x.checked_sub(self.buffer).ok_or(Error::Overflow)?,
                y: min.y.checked_sub(self.buffer).ok_or(Error::Overflow)?,
            },
            self.divider,
        );
        let hi = tile_of(
            Coord {
                x: max.x.checked_add(self.buffer).ok_or(Error::Overflow)?,
                y: max.y.checked_add(self.buffer).ok_or(Error::Overflow)?,
            },
            self.divider,
        );
        Ok((lo, hi))
    }
}
