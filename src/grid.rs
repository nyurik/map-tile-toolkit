//! The stateless slicing engine shared by [`SlicerAll`](crate::SlicerAll) and
//! [`SlicerOne`](crate::SlicerOne).
//!
//! [`Grid`] holds only the tile geometry (extent + buffer) and knows how to clip one polyline —
//! into a single tile ([`Grid::slice_one`]) or, by routing it into every tile it touches, into a
//! [`RouteSink`] ([`Grid::route`]). It
//! keeps no accumulated state; the two public slicers layer feature accumulation on top of it.

use geo_types::Coord;

use crate::TileError;
use crate::clip_polyline::{segment_intersects, to_local};
use crate::tile::{TileId, tile_of};
use crate::vertex::Vertex;

/// The maximum polyline length the slicer accepts (`u16::MAX + 1` vertices); a longer polyline yields
/// [`TileError::PolylineTooLarge`]. A fixed cap, so the documented per-line vertex limit holds.
const MAX_INDEXED_LEN: usize = u16::MAX as usize + 1;

/// Upper bound on the candidate tiles [`Grid::route`] will examine before giving up with
/// [`TileError::TooManyTiles`]. Far above any realistic polyline (a local way examines a handful per
/// segment), it caps worst-case time and memory for adversarial, widely-spread input. ~33M tests is
/// well under a second.
const MAX_TILE_VISITS: i64 = 1 << 25;

/// `c` shifted by `d` on both axes. Used for the `± buffer` corner offsets, where the caller has
/// already proved the result stays in `i32` (so no checked arithmetic).
#[inline]
const fn shift(c: Coord<i32>, d: i32) -> Coord<i32> {
    Coord {
        x: c.x + d,
        y: c.y + d,
    }
}

/// Sink for [`Grid::route`]: receives every `(tile, segment)` the routing produces and decides how to
/// store it. [`SlicerAll`](crate::SlicerAll) implements it to append clipped vertices straight into
/// its per-tile buffers, with no intermediate hit list, sort, or copy.
pub(crate) trait RouteSink<V: Vertex> {
    /// Called once at the start of a polyline, before any segment — lets the sink break run continuity
    /// across separate polylines.
    fn begin_polyline(&mut self);

    /// Called before each segment's `emit`s, in walk order — lets the sink tell whether a tile's run
    /// continues (the same tile was emitted to by the immediately preceding segment).
    fn begin_segment(&mut self);

    /// Route the segment `a`–`c` (the original vertices) into `tile`, whose local-frame origin is
    /// `origin` (`tile · extent`). The sink localizes and stores.
    ///
    /// # Errors
    ///
    /// [`TileError::Overflow`] if a vertex lies more than an `i32` span from `origin`.
    fn emit(&mut self, tile: TileId, origin: Coord<i32>, a: V, c: V) -> Result<(), TileError>;
}

/// A vertex's owner tile with its core cell and inner box precomputed in **global** coordinates, so
/// membership tests are plain comparisons with no division. [`Grid::route`] caches the last one
/// across the vertex walk: consecutive vertices in the same tile reuse it, and a segment whose two
/// endpoints both lie in the inner box touches only that one tile.
///
/// - the **core cell** `[base, base + extent − 1]` is the tile's own cell (owning `base = owner ·
///   extent`); a coordinate here has this tile as its owner.
/// - the **inner box** `[base + buffer, base + extent − 1 − buffer]` is the core shrunk by the
///   buffer; a segment with both endpoints inside it stays ≥ `buffer` from every edge, so it cannot
///   reach any neighboring tile's buffered box.
#[derive(Clone, Copy)]
struct Located {
    owner: TileId,
    core_lo: Coord<i32>,
    core_hi: Coord<i32>,
    inner_lo: Coord<i32>,
    inner_hi: Coord<i32>,
}

impl Located {
    /// Does `c`'s owner tile equal this one (is `c` in the core cell)?
    fn contains_core(&self, c: Coord<i32>) -> bool {
        c.x >= self.core_lo.x
            && c.x <= self.core_hi.x
            && c.y >= self.core_lo.y
            && c.y <= self.core_hi.y
    }

    /// Is `c` in the inner box (≥ `buffer` from every cell edge)?
    fn contains_inner(&self, c: Coord<i32>) -> bool {
        c.x >= self.inner_lo.x
            && c.x <= self.inner_hi.x
            && c.y >= self.inner_lo.y
            && c.y <= self.inner_hi.y
    }
}

/// The tile geometry a slicer clips against: the tile side ([`extent`](Self::extent)) and a
/// [`buffer`](Self::buffer), plus the clipping engine.
///
/// Integers in pre-scaled tile space: `x` belongs to tile `x.div_euclid(extent)` and is emitted at
/// `x − tile·extent ∈ [0, extent)`, so `extent` is both the tile side and its output resolution; each
/// clip box grows `buffer` on every side. The library owns no float/projection math (callers scale
/// into this space up front), keeps original vertices, and never panics — bad input yields a
/// [`TileError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grid {
    /// Tile side length in tile space, i.e. the per-tile output resolution (always in `1..=i32::MAX`).
    extent: i32,
    /// Margin, in tile-space units, kept around every tile (always in `0..=u16::MAX`).
    buffer: i32,
}

impl Grid {
    /// Create a grid with the given tile side / output resolution `extent` and `buffer`.
    ///
    /// # Errors
    ///
    /// - [`TileError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    /// - [`TileError::BufferTooLarge`] if `2 * buffer >= extent` — the buffer must stay under half a
    ///   tile, so a vertex near an edge spills into at most one neighbor per axis and the
    ///   tile-minus-buffer inner box stays non-empty (both relied on by the routing).
    pub(crate) const fn new(extent: u32, buffer: u16) -> Result<Self, TileError> {
        if extent == 0 || extent > i32::MAX.cast_unsigned() {
            return Err(TileError::InvalidExtent);
        }
        // `2 * buffer` cannot overflow: `buffer <= u16::MAX`, so the product fits `u32`.
        if 2 * (buffer as u32) >= extent {
            return Err(TileError::BufferTooLarge);
        }
        Ok(Self {
            extent: extent.cast_signed(),
            buffer: buffer as i32,
        })
    }

    /// The tile side length / per-tile output resolution: kept vertices land in `0..extent`.
    pub(crate) fn extent(self) -> u32 {
        self.extent.cast_unsigned()
    }

    /// The buffer kept around every tile, in tile-space units.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "buffer is always in 0..=u16::MAX (it was built from a u16)"
    )]
    pub(crate) fn buffer(self) -> u16 {
        self.buffer as u16
    }

    /// Clip one `polyline` to a single tile, keeping original vertices. Returns the kept runs in the
    /// tile's **local coordinates** — the tile's `[0, 0]` corner is the origin, so a kept vertex lands
    /// in `0..extent` (buffer vertices past the low edge go negative). The result is empty when nothing
    /// of `polyline` touches the tile's (buffered) box.
    ///
    /// A vertex is **kept** when either of its segments touches the box, so a border crossing keeps the
    /// first vertex just outside. A run breaks only where a vertex is *dropped* (both its segments miss
    /// the box): a single-segment excursion out of and back into the tile keeps its whole geometry as
    /// one run (both outside vertices are kept), while a longer excursion — which drops the vertices in
    /// between — comes back as separate runs.
    ///
    /// # Errors
    ///
    /// [`TileError::Overflow`] if `tile`'s (buffered) box coordinates overflow `i32` (a tile far
    /// outside the representable range for this `extent`), or a kept vertex lies more than an `i32`
    /// span from the tile origin.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn slice_one<V: Vertex>(
        self,
        polyline: &[V],
        tile: TileId,
    ) -> Result<Vec<Vec<V>>, TileError> {
        let poly = polyline;
        let (min, max) = self.tile_buffered_bounds(tile)?;
        // The tile origin is `min` grown back by the buffer: `tile_buffered_bounds` already proved
        // `origin − buffer` fits `i32` and `origin` is the checked base corner, so this cannot
        // overflow — no need to recompute (and re-check) `tile · extent`.
        let origin = shift(min, self.buffer);
        // Clip and localize in one pass: store each kept vertex already offset by the tile origin, so
        // there is no separate localization pass over the output. Each vertex is emitted once its keep
        // status is fully known (both its segments seen), so we act on `prev`, carrying whether the
        // segment *into* `prev` touched the box.
        let mut runs = Vec::new();
        let mut cur: Vec<V> = Vec::new();
        let mut prev: Option<V> = None;
        let mut left_hit = false;
        for &c in poly {
            if let Some(a) = prev {
                if a.position() == c.position() {
                    continue; // drop a consecutive duplicate vertex (keep `prev`/`left_hit`)
                }
                let this_hit = segment_intersects(a.position(), c.position(), min, max);
                if left_hit || this_hit {
                    cur.push(to_local(a, origin)?); // `a` is kept by one of its segments
                } else if cur.len() >= 2 {
                    runs.push(std::mem::take(&mut cur)); // `a` is dropped: close the run before it
                } else {
                    cur.clear();
                }
                left_hit = this_hit;
            }
            prev = Some(c);
        }
        // The last vertex is kept iff its only segment (the final one) touched the box.
        if left_hit && let Some(a) = prev {
            cur.push(to_local(a, origin)?);
        }
        if cur.len() >= 2 {
            runs.push(cur);
        }

        Ok(runs)
    }

    /// Walk one `polyline` once, driving `sink` with every `(tile, segment)` it produces — the same
    /// routing the per-tile clip agrees with, streamed instead of collected into a hit list, so
    /// [`SlicerAll`](crate::SlicerAll) writes clipped vertices straight into its buffers with no
    /// intermediate allocation, sort, or copy.
    ///
    /// Each segment (skipping consecutive duplicate positions) is routed into every tile whose
    /// buffered box it touches, in walk order. Fast path: a segment lying entirely within one tile's
    /// inner box (≥ `buffer` from every edge — the common case) goes straight to that tile, skipping
    /// `tile_of` and the geometry test; the owning tile is cached across the walk (see [`Located`]).
    /// The sink gets each touched tile's id, local-frame origin, and the segment's two **original**
    /// vertices (it localizes).
    ///
    /// `begin_polyline` is called once, then `begin_segment` before each segment's `emit`s, so the
    /// sink can track run continuity.
    ///
    /// # Errors
    ///
    /// - [`TileError::PolylineTooLarge`] — the polyline has more than `u16::MAX` vertices.
    /// - [`TileError::TooManyTiles`] — the polyline spans more than `i16::MAX` tiles on an axis, or its
    ///   segments would collectively examine more than `MAX_TILE_VISITS` candidate tiles.
    /// - [`TileError::Overflow`] — a coordinate `± buffer` overflows `i32`, or (from the sink) a kept
    ///   vertex lies more than an `i32` span from its tile origin.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn route<V: Vertex, S: RouteSink<V>>(
        self,
        polyline: &[V],
        sink: &mut S,
    ) -> Result<(), TileError> {
        let poly = polyline;

        // Up-front length check before any `emit`, so this input-level error is atomic.
        if poly.len() > MAX_INDEXED_LEN {
            return Err(TileError::PolylineTooLarge);
        }

        // Empty polyline → nothing to route.
        let Some(first) = poly.first().map(Vertex::position) else {
            return Ok(());
        };

        // Each routed segment's tile box is bounded against this reference (the first vertex's tile)
        // as it is walked, so there is no separate bounding-box pass over the whole polyline.
        let reference = tile_of(first, self.extent);

        sink.begin_polyline();
        // Bound the total candidate tiles examined, so an adversarial spread of long segments can't
        // exhaust time or memory: a polyline needing more than this is rejected rather than crashing.
        let mut budget: i64 = MAX_TILE_VISITS;
        // Carry the previous vertex and its located tile, so a segment whose two endpoints share one
        // tile's inner box needs no division or geometry test at all.
        let mut prev: Option<V> = None;
        let mut prev_loc: Option<Located> = None;
        for v in poly {
            let c = v.position();
            if let Some(a) = prev {
                let a_pos = a.position();
                if a_pos == c {
                    continue; // drop a consecutive duplicate vertex (keep `prev`/`prev_loc`)
                }
                sink.begin_segment();
                // `a`'s tile: carried from the previous step, or located now for the first segment.
                let la = match prev_loc {
                    Some(l) => l,
                    None => self.locate(a_pos)?,
                };
                if la.contains_inner(a_pos) && la.contains_inner(c) {
                    // Fast path: the whole segment lies in `la`'s inner box, so it touches only that
                    // tile (`la.core_lo` is that tile's origin) — no `tile_of`, `tile_bounds`, or
                    // geometry test.
                    budget -= 1;
                    if budget < 0 {
                        return Err(TileError::TooManyTiles);
                    }
                    sink.emit(la.owner, la.core_lo, a, *v)?;
                    prev_loc = Some(la); // `c` is in `la`'s core, so its tile is `la`
                } else {
                    // Slow path: route the segment through every candidate tile it might touch. Grow
                    // the segment's coordinate box by the buffer (checked — a coordinate too near the
                    // i32 edge reports `Overflow`) and map it to tiles.
                    let lo = tile_of(
                        Coord {
                            x: (a_pos.x.min(c.x))
                                .checked_sub(self.buffer)
                                .ok_or(TileError::Overflow)?,
                            y: (a_pos.y.min(c.y))
                                .checked_sub(self.buffer)
                                .ok_or(TileError::Overflow)?,
                        },
                        self.extent,
                    );
                    let hi = tile_of(
                        Coord {
                            x: (a_pos.x.max(c.x))
                                .checked_add(self.buffer)
                                .ok_or(TileError::Overflow)?,
                            y: (a_pos.y.max(c.y))
                                .checked_add(self.buffer)
                                .ok_or(TileError::Overflow)?,
                        },
                        self.extent,
                    );
                    // Bound this segment's tile box against the reference: each extreme must stay within
                    // `i16` of the first vertex's tile (else the polyline reaches too many tiles). This
                    // also keeps the candidate-count product below `i64` overflow.
                    for (t, r) in [
                        (lo.x, reference.x),
                        (hi.x, reference.x),
                        (lo.y, reference.y),
                        (hi.y, reference.y),
                    ] {
                        i16::try_from(i64::from(t) - i64::from(r))
                            .map_err(|_| TileError::TooManyTiles)?;
                    }
                    // Charge this segment's candidate-tile box.
                    budget -= (i64::from(hi.x) - i64::from(lo.x) + 1)
                        * (i64::from(hi.y) - i64::from(lo.y) + 1);
                    if budget < 0 {
                        return Err(TileError::TooManyTiles);
                    }
                    for ty in lo.y..=hi.y {
                        for tx in lo.x..=hi.x {
                            let tile = TileId::new(tx, ty);
                            let (min, max) = self.tile_buffered_bounds(tile)?;
                            if segment_intersects(a_pos, c, min, max) {
                                // Tile origin = base = min + buffer.
                                sink.emit(tile, shift(min, self.buffer), a, *v)?;
                            }
                        }
                    }
                    // `c`'s tile for the next step: reuse `la` if `c` shares its core, else locate it
                    // (its box was just validated in the scan above, so this cannot newly error).
                    prev_loc = Some(if la.contains_core(c) {
                        la
                    } else {
                        self.locate(c)?
                    });
                }
            }
            prev = Some(*v);
        }
        Ok(())
    }

    /// The closed integer bounds `(min, max)` of `tile`'s clip box (in output space), grown by
    /// `buffer` on each side. All arithmetic is checked; [`TileError::Overflow`] means the tile lies
    /// outside the representable range for this `extent`.
    pub(crate) fn tile_buffered_bounds(
        self,
        tile: TileId,
    ) -> Result<(Coord<i32>, Coord<i32>), TileError> {
        let base_x = tile.x.checked_mul(self.extent).ok_or(TileError::Overflow)?;
        let base_y = tile.y.checked_mul(self.extent).ok_or(TileError::Overflow)?;
        // Distance from the base corner to the far corner of the buffered box: extent - 1 + buffer.
        let reach = (self.extent - 1)
            .checked_add(self.buffer)
            .ok_or(TileError::Overflow)?;
        Ok((
            Coord {
                x: base_x.checked_sub(self.buffer).ok_or(TileError::Overflow)?,
                y: base_y.checked_sub(self.buffer).ok_or(TileError::Overflow)?,
            },
            Coord {
                x: base_x.checked_add(reach).ok_or(TileError::Overflow)?,
                y: base_y.checked_add(reach).ok_or(TileError::Overflow)?,
            },
        ))
    }

    /// Locate the tile owning `c` (in output space), with its core and inner boxes precomputed (see
    /// [`Located`]). Built on [`Self::tile_buffered_bounds`], so it reports [`TileError::Overflow`] for exactly
    /// the tiles the routing scan would — `min = base − buffer` and `max = base + extent − 1 + buffer`,
    /// from which the core (`base .. base + extent − 1`) and inner (`base + buffer .. max − 2·buffer`)
    /// follow by `± buffer` (all within `[min, max]`, so no further overflow).
    fn locate(self, c: Coord<i32>) -> Result<Located, TileError> {
        let owner = tile_of(c, self.extent);
        let (min, max) = self.tile_buffered_bounds(owner)?;
        Ok(Located {
            owner,
            core_lo: shift(min, self.buffer),
            core_hi: shift(max, -self.buffer),
            inner_lo: shift(min, 2 * self.buffer),
            inner_hi: shift(max, -2 * self.buffer),
        })
    }
}
