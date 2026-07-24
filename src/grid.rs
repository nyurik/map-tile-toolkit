//! The stateless slicing engine shared by [`SlicerAll`](crate::SlicerAll) and
//! [`SlicerOne`](crate::SlicerOne).
//!
//! [`Grid`] holds only the tile geometry (extent + buffer) and knows how to clip one polyline —
//! into a single tile ([`Grid::slice_one`]) or, by routing it into every tile it touches, into a
//! [`RouteSink`] ([`Grid::route`]). It
//! keeps no accumulated state; the two public slicers layer feature accumulation on top of it.

use geo_types::Coord;

use crate::SliceError;
use crate::clip_polyline::{clip_line, segment_intersects};
use crate::tile::{TileId, tile_of};
use crate::vertex::Vertex;

/// The maximum polyline length the slicer accepts (`u16::MAX + 1` vertices); a longer polyline yields
/// [`SliceError::PolylineTooLarge`]. A fixed cap, so the documented per-line vertex limit holds.
const MAX_INDEXED_LEN: usize = u16::MAX as usize + 1;

/// Upper bound on the candidate tiles [`Grid::route`] will examine before giving up with
/// [`SliceError::TooManyTiles`]. Far above any realistic polyline (a local way examines a handful per
/// segment), it caps worst-case time and memory for adversarial, widely-spread input. ~33M tests is
/// well under a second.
const MAX_TILE_VISITS: i64 = 1 << 25;

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
    /// [`SliceError::Overflow`] if a vertex lies more than an `i32` span from `origin`.
    fn emit(&mut self, tile: TileId, origin: Coord<i32>, a: V, c: V) -> Result<(), SliceError>;
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
///   reach any neighbouring tile's buffered box.
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
/// Coordinates are integers in a pre-scaled **tile space**: a coordinate `x` belongs to tile
/// `x.div_euclid(extent)`, and a vertex kept in tile `t` is emitted at `x − t·extent ∈ [0, extent)`.
/// So `extent` is both the tile side and each tile's output resolution — the number of integers
/// across a tile. Each tile's clip box is grown outward by `buffer` units on every side.
///
/// The library owns no float/projection math: callers project, simplify, and affine-scale their data
/// into this integer tile space up front (e.g. with `geo`), so `Grid` stays dimensionless.
///
/// Clipping keeps the polyline's original vertices and includes every tile a segment passes through.
/// Output pieces are **tile-local runs** — the tile's `[0, 0]` corner is the origin.
///
/// The engine never panics: bad input (an oversized polyline, or coordinates that overflow the tile
/// math) yields an [`SliceError`] instead.
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
    /// - [`SliceError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    /// - [`SliceError::BufferTooLarge`] if `2 * buffer >= extent` — the buffer must stay under half a
    ///   tile, so a vertex near an edge spills into at most one neighbour per axis and the
    ///   tile-minus-buffer inner box stays non-empty (both relied on by the routing).
    pub(crate) const fn new(extent: u32, buffer: u16) -> Result<Self, SliceError> {
        if extent == 0 || extent > i32::MAX.cast_unsigned() {
            return Err(SliceError::InvalidExtent);
        }
        // `2 * buffer` cannot overflow: `buffer <= u16::MAX`, so the product fits `u32`.
        if 2 * (buffer as u32) >= extent {
            return Err(SliceError::BufferTooLarge);
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
    /// of `polyline` touches the tile's (buffered) box, and holds several runs where the polyline
    /// leaves the tile and re-enters.
    ///
    /// # Errors
    ///
    /// [`SliceError::Overflow`] if `tile`'s (buffered) box coordinates overflow `i32` (a tile far
    /// outside the representable range for this `extent`), or a kept vertex lies more than an `i32`
    /// span from the tile origin.
    pub(crate) fn slice_one<V: Vertex>(
        self,
        polyline: &[V],
        tile: TileId,
    ) -> Result<Vec<Vec<V>>, SliceError> {
        let poly = polyline;
        let (min, max) = self.tile_bounds(tile)?;
        // The tile origin is `min` grown back by the buffer: `tile_bounds` already proved
        // `origin − buffer` fits `i32` and `origin` is the checked base corner, so this cannot
        // overflow — no need to recompute (and re-check) `tile · extent`.
        let origin = Coord {
            x: min.x + self.buffer,
            y: min.y + self.buffer,
        };
        // Clip and localize in one pass: `clip_line` stores each kept vertex already offset by the
        // tile origin, so there is no separate localization pass over the output.
        let mut runs = Vec::new();
        clip_line(poly, min, max, origin, &mut runs)?;
        Ok(runs)
    }

    /// Walk one `polyline` once and drive `sink` with every `(tile, segment)` it produces — the same
    /// routing the per-tile clip agrees with, but streamed into the sink instead of collected into a
    /// hit list. [`SlicerAll`](crate::SlicerAll) uses this to write clipped vertices straight into its
    /// per-tile buffers, with no intermediate allocation, sort, or copy.
    ///
    /// Every segment (consecutive same-position vertices skipped) is routed into each tile whose
    /// buffered box it touches, in walk order. As a fast path, a segment whose two endpoints both lie
    /// in one tile's inner box (≥ `buffer` from every edge) — the common case for a dense polyline —
    /// is routed straight to that single tile, skipping the per-corner `tile_of` and the per-candidate
    /// geometry test; the owning tile is cached across the walk (see [`Located`]), so division happens
    /// only when the polyline crosses into a new tile. The sink sees, per touched tile, the tile id,
    /// its local-frame origin, and the segment's two **original** vertices (localization is its job).
    ///
    /// `sink.begin_polyline` is called once, then `sink.begin_segment` before each segment's `emit`s,
    /// so the sink can track run continuity (a run grows only across segments one tile sees back to
    /// back).
    ///
    /// # Errors
    ///
    /// - [`SliceError::PolylineTooLarge`] — the polyline has more than `u16::MAX` vertices.
    /// - [`SliceError::TooManyTiles`] — the polyline spans more than `i16::MAX` tiles on an axis, or its
    ///   segments would collectively examine more than `MAX_TILE_VISITS` candidate tiles.
    /// - [`SliceError::Overflow`] — a coordinate `± buffer` overflows `i32`, or (from the sink) a kept
    ///   vertex lies more than an `i32` span from its tile origin.
    pub(crate) fn route<V: Vertex, S: RouteSink<V>>(
        self,
        polyline: &[V],
        sink: &mut S,
    ) -> Result<(), SliceError> {
        let poly = polyline;

        // Up-front length check before any `emit`, so this input-level error is atomic.
        if poly.len() > MAX_INDEXED_LEN {
            return Err(SliceError::PolylineTooLarge);
        }

        // Empty polyline → nothing to route.
        let Some(first) = poly.first().map(Vertex::position) else {
            return Ok(());
        };

        // The overall tile span must fit `i16` from the first vertex's tile. The extreme tiles come
        // from the coordinate bounds grown by the buffer; `?` reports coordinates too close to the
        // i32 edge.
        let reference = tile_of(first, self.extent);
        let (lo_tile, hi_tile) = self.buffered_tile_bounds(poly, first)?;
        for (tile, refc) in [
            (lo_tile.x, reference.x),
            (hi_tile.x, reference.x),
            (lo_tile.y, reference.y),
            (hi_tile.y, reference.y),
        ] {
            i16::try_from(i64::from(tile) - i64::from(refc))
                .map_err(|_| SliceError::TooManyTiles)?;
        }

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
                        return Err(SliceError::TooManyTiles);
                    }
                    sink.emit(la.owner, la.core_lo, a, *v)?;
                    prev_loc = Some(la); // `c` is in `la`'s core, so its tile is `la`
                } else {
                    // Slow path: route the segment through every candidate tile it might touch.
                    let lo = tile_of(
                        Coord {
                            x: a_pos.x.min(c.x) - self.buffer,
                            y: a_pos.y.min(c.y) - self.buffer,
                        },
                        self.extent,
                    );
                    let hi = tile_of(
                        Coord {
                            x: a_pos.x.max(c.x) + self.buffer,
                            y: a_pos.y.max(c.y) + self.buffer,
                        },
                        self.extent,
                    );
                    // Charge this segment's candidate-tile box (in range: lo/hi ∈ [lo_tile, hi_tile]).
                    budget -= (i64::from(hi.x) - i64::from(lo.x) + 1)
                        * (i64::from(hi.y) - i64::from(lo.y) + 1);
                    if budget < 0 {
                        return Err(SliceError::TooManyTiles);
                    }
                    for ty in lo.y..=hi.y {
                        for tx in lo.x..=hi.x {
                            let tile = TileId::new(tx, ty);
                            let (min, max) = self.tile_bounds(tile)?;
                            if segment_intersects(a_pos, c, min, max) {
                                // Tile origin = base = min + buffer.
                                let origin = Coord {
                                    x: min.x + self.buffer,
                                    y: min.y + self.buffer,
                                };
                                sink.emit(tile, origin, a, *v)?;
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
    /// `buffer` on each side. All arithmetic is checked; [`SliceError::Overflow`] means the tile lies
    /// outside the representable range for this `extent`.
    fn tile_bounds(self, tile: TileId) -> Result<(Coord<i32>, Coord<i32>), SliceError> {
        let base_x = tile
            .x
            .checked_mul(self.extent)
            .ok_or(SliceError::Overflow)?;
        let base_y = tile
            .y
            .checked_mul(self.extent)
            .ok_or(SliceError::Overflow)?;
        // Distance from the base corner to the far corner of the buffered box: extent - 1 + buffer.
        let reach = (self.extent - 1)
            .checked_add(self.buffer)
            .ok_or(SliceError::Overflow)?;
        Ok((
            Coord {
                x: base_x
                    .checked_sub(self.buffer)
                    .ok_or(SliceError::Overflow)?,
                y: base_y
                    .checked_sub(self.buffer)
                    .ok_or(SliceError::Overflow)?,
            },
            Coord {
                x: base_x.checked_add(reach).ok_or(SliceError::Overflow)?,
                y: base_y.checked_add(reach).ok_or(SliceError::Overflow)?,
            },
        ))
    }

    /// Locate the tile owning `c` (in output space), with its core and inner boxes precomputed (see
    /// [`Located`]). Built on [`Self::tile_bounds`], so it reports [`SliceError::Overflow`] for exactly
    /// the tiles the routing scan would — `min = base − buffer` and `max = base + extent − 1 + buffer`,
    /// from which the core (`base .. base + extent − 1`) and inner (`base + buffer .. max − 2·buffer`)
    /// follow by `± buffer` (all within `[min, max]`, so no further overflow).
    fn locate(self, c: Coord<i32>) -> Result<Located, SliceError> {
        let owner = tile_of(c, self.extent);
        let (min, max) = self.tile_bounds(owner)?;
        Ok(Located {
            owner,
            core_lo: Coord {
                x: min.x + self.buffer,
                y: min.y + self.buffer,
            },
            core_hi: Coord {
                x: max.x - self.buffer,
                y: max.y - self.buffer,
            },
            inner_lo: Coord {
                x: min.x + 2 * self.buffer,
                y: min.y + 2 * self.buffer,
            },
            inner_hi: Coord {
                x: max.x - 2 * self.buffer,
                y: max.y - 2 * self.buffer,
            },
        })
    }

    /// The lowest and highest tiles any part of `poly` can reach: the coordinate bounding box grown by
    /// `buffer`, mapped to tiles. `first` seeds the bounds. [`SliceError::Overflow`] if a coordinate
    /// `± buffer` overflows `i32`.
    fn buffered_tile_bounds<V: Vertex>(
        self,
        poly: &[V],
        first: Coord<i32>,
    ) -> Result<(TileId, TileId), SliceError> {
        let (mut min, mut max) = (first, first);
        for v in poly {
            let c = v.position();
            min.x = min.x.min(c.x);
            min.y = min.y.min(c.y);
            max.x = max.x.max(c.x);
            max.y = max.y.max(c.y);
        }
        let lo = tile_of(
            Coord {
                x: min.x.checked_sub(self.buffer).ok_or(SliceError::Overflow)?,
                y: min.y.checked_sub(self.buffer).ok_or(SliceError::Overflow)?,
            },
            self.extent,
        );
        let hi = tile_of(
            Coord {
                x: max.x.checked_add(self.buffer).ok_or(SliceError::Overflow)?,
                y: max.y.checked_add(self.buffer).ok_or(SliceError::Overflow)?,
            },
            self.extent,
        );
        Ok((lo, hi))
    }
}
