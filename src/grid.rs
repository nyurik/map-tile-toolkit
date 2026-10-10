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

/// Upper bound on the tiles [`Grid::route`] will visit (one per tile each segment touches, plus one
/// per tile row a segment crosses between two unbuffered tiles without touching either) before
/// giving up with [`TileError::TooManyTiles`]. This is the only reach limit: it bounds the time and
/// the per-tile output memory a single feature can demand, so adversarial input (say, thousands of
/// world-spanning zigzags) is rejected rather than exhausting the host. It sits far above any
/// realistic feature — a local way visits a handful of tiles per segment, a world-spanning z16
/// diagonal (extent 4096, buffer 64) ~140 000 — and ~33M visits is well under a second.
pub(crate) const MAX_TILE_VISITS: i64 = 1 << 25;

/// `c` shifted by `d` on both axes. Used for the `± buffer` corner offsets, where the caller has
/// already proved the result stays in `i32` (so no checked arithmetic).
#[inline]
const fn shift(c: Coord<i32>, d: i32) -> Coord<i32> {
    Coord {
        x: c.x + d,
        y: c.y + d,
    }
}

/// `⌊n·m / d⌋` for `d > 0`, exactly. The product of two coordinate spans can exceed `i64`, so it
/// falls back to `i128` — only for spans beyond ~3 billion units, so the common case stays in `i64`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "callers pass |n| <= d, so the quotient is within |m| and fits i64"
)]
fn mul_div_floor(n: i64, m: i64, d: i64) -> i64 {
    match n.checked_mul(m) {
        Some(p) => p.div_euclid(d),
        None => (i128::from(n) * i128::from(m)).div_euclid(i128::from(d)) as i64,
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

    /// Called after each segment `a`–`c` is routed (and charged to the tile budget), with `row` the
    /// tile row whose cells' inner boxes hold the whole segment, if one does.
    ///
    /// # Errors
    ///
    /// As the sink reports; the default does nothing.
    fn end_segment(
        &mut self,
        a: Coord<i32>,
        c: Coord<i32>,
        row: Option<i32>,
    ) -> Result<(), TileError> {
        let _ = (a, c, row);
        Ok(())
    }
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
    /// Any other segment is walked row by row through exactly the tiles it touches ([`Self::walk`]),
    /// so its cost follows those tiles, not its bounding rectangle.
    /// The sink gets each touched tile's id, local-frame origin, and the segment's two **original**
    /// vertices (it localizes).
    ///
    /// `begin_polyline` is called once, then `begin_segment` before each segment's `emit`s, so the
    /// sink can track run continuity.
    ///
    /// # Errors
    ///
    /// - [`TileError::GeometryTooLarge`] — the polyline has more than `u32::MAX` vertices (the
    ///   slicers' flat storage indexes vertices with `u32`).
    /// - [`TileError::TooManyTiles`], [`TileError::Overflow`] — as in [`Self::route_within`], with a
    ///   fresh `MAX_TILE_VISITS` budget.
    pub(crate) fn route<V: Vertex, S: RouteSink<V>>(
        self,
        polyline: &[V],
        sink: &mut S,
    ) -> Result<(), TileError> {
        // Up-front length check before any `emit`, so this input-level error is atomic.
        if u32::try_from(polyline.len()).is_err() {
            return Err(TileError::GeometryTooLarge);
        }
        // Bound the total tiles visited, so an adversarial spread of long segments can't exhaust
        // time or memory: a polyline needing more than this is rejected rather than crashing.
        let mut budget = MAX_TILE_VISITS;
        self.route_within(polyline, sink, &mut budget)
    }

    /// [`Self::route`] without the vertex-count cap, charging every tile visited to the caller's
    /// `budget` — so several polylines (a polygon's rings) can share one working-set bound.
    ///
    /// # Errors
    ///
    /// - [`TileError::TooManyTiles`] — its segments would visit more tiles than remain in `budget`.
    /// - [`TileError::Overflow`] — a touched tile's buffered box overflows `i32` (an outermost tile,
    ///   with `buffer > 0`), or (from the sink) a kept vertex lies more than an `i32` span from its
    ///   tile origin.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn route_within<V: Vertex, S: RouteSink<V>>(
        self,
        polyline: &[V],
        sink: &mut S,
        budget: &mut i64,
    ) -> Result<(), TileError> {
        let poly = polyline;

        // Empty polyline → nothing to route.
        if poly.is_empty() {
            return Ok(());
        }

        sink.begin_polyline();
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
                    *budget -= 1;
                    if *budget < 0 {
                        return Err(TileError::TooManyTiles);
                    }
                    sink.emit(la.owner, la.core_lo, a, *v)?;
                    sink.end_segment(a_pos, c, Some(la.owner.y))?;
                    prev_loc = Some(la); // `c` is in `la`'s core, so its tile is `la`
                } else {
                    // Slow path: walk the segment through exactly the tiles it touches.
                    self.walk(a, *v, sink, budget)?;
                    sink.end_segment(a_pos, c, None)?;
                    // `c`'s tile for the next step: reuse `la` if `c` shares its core, else locate it
                    // (its tile was just visited by the walk, so this cannot newly error).
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

    /// Route segment `a`–`c` into exactly the tiles whose buffered box it touches, charging each to
    /// `budget` — so a long diagonal costs the tiles it crosses, not its bounding rectangle. A row the
    /// segment crosses without touching a tile (it runs through the gap between two unbuffered
    /// tiles) is charged one visit too, so every row the walk iterates is paid for.
    ///
    /// Rows ascend, then columns within a row. Inside a row's buffered `y`-slab the segment is a
    /// sub-segment whose `x`-extent is one interval, and a tile of the row touches the segment iff its
    /// buffered `x`-range meets that interval — the separating-axis answer [`segment_intersects`]
    /// gives per tile, computed once per row with exact rounding. Each touched tile's box is built
    /// with checked math; since the endpoints' own tiles are always touched, a segment reaching past
    /// the `i32` range reports [`TileError::Overflow`].
    ///
    /// # Errors
    ///
    /// - [`TileError::TooManyTiles`] — the touched tiles and gap rows exceed the remaining `budget`.
    /// - [`TileError::Overflow`] — a touched tile's buffered box overflows `i32`, or (from the sink) a
    ///   kept vertex lies more than an `i32` span from its tile origin.
    fn walk<V: Vertex, S: RouteSink<V>>(
        self,
        a: V,
        c: V,
        sink: &mut S,
        budget: &mut i64,
    ) -> Result<(), TileError> {
        let (extent, buffer) = (i64::from(self.extent), i64::from(self.buffer));
        let wide = |p: Coord<i32>| Coord {
            x: i64::from(p.x),
            y: i64::from(p.y),
        };
        // Order the endpoints by `y`, so rows ascend and the `y` offsets below are non-negative.
        let (lo, hi) = if a.position().y <= c.position().y {
            (wide(a.position()), wide(c.position()))
        } else {
            (wide(c.position()), wide(a.position()))
        };
        let (dx, dy) = (hi.x - lo.x, hi.y - lo.y);
        for ty in (lo.y - buffer).div_euclid(extent)..=(hi.y + buffer).div_euclid(extent) {
            // The row's buffered slab clipped to the segment's `y`-range, as offsets from `lo.y`;
            // never empty, since these are exactly the rows whose slab meets `[lo.y, hi.y]`.
            let base = ty * extent;
            let start = (base - buffer).max(lo.y) - lo.y;
            let end = (base + extent - 1 + buffer).min(hi.y) - lo.y;
            // The sub-segment's `x`-extent, `x(y) = lo.x + (y − lo.y)·dx/dy` at the slab's two ends
            // (monotone in `y`): rounding the low end up and the high end down keeps exactly the
            // integers it spans, and so the closed integer boxes it meets. An end at a vertex is
            // exact, which spares the division for most short segments.
            let x_at = |off: i64, up: bool| match off {
                0 => lo.x,
                _ if off == dy => hi.x,
                _ if up => lo.x - mul_div_floor(-off, dx, dy),
                _ => lo.x + mul_div_floor(off, dx, dy),
            };
            let (x_min, x_max) = if dy == 0 {
                (lo.x.min(hi.x), lo.x.max(hi.x))
            } else if dx >= 0 {
                (x_at(start, true), x_at(end, false))
            } else {
                (x_at(end, true), x_at(start, false))
            };
            let (first, last) = (
                (x_min - buffer).div_euclid(extent),
                (x_max + buffer).div_euclid(extent),
            );
            // Charge every row, even one whose sub-segment runs through the gap between two
            // unbuffered tiles and touches none (only possible with `buffer == 0`): otherwise a
            // near-vertical segment along such a gap would cost one free iteration per row.
            *budget -= (last - first + 1).max(1);
            if *budget < 0 {
                return Err(TileError::TooManyTiles);
            }
            if first > last {
                continue;
            }
            // Every touched box of the row lies within the row's outermost buffered corners, so
            // checking those once covers each tile's origin and box.
            let fits = |v: i64| i32::try_from(v).map_err(|_| TileError::Overflow);
            fits(first * extent - buffer)?;
            fits(last * extent + extent - 1 + buffer)?;
            fits(base - buffer)?;
            fits(base + extent - 1 + buffer)?;
            let (ty, origin_y) = (fits(ty)?, fits(base)?);
            for tx in fits(first)?..=fits(last)? {
                let origin = Coord {
                    x: tx * self.extent,
                    y: origin_y,
                };
                sink.emit(TileId::new(tx, ty), origin, a, c)?;
            }
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

    /// The tile whose inner box holds every point of `points`, and its origin: then every segment
    /// between them touches that tile's buffered box and no other's. `None` if they span more than one
    /// inner box, or there are none (or the tile overflows, left to the general path to report).
    pub(crate) fn inner_tile(
        self,
        mut points: impl Iterator<Item = Coord<i32>>,
    ) -> Option<(TileId, Coord<i32>)> {
        let first = points.next()?;
        let located = self.locate(first).ok()?;
        (located.contains_inner(first) && points.all(|p| located.contains_inner(p)))
            .then_some((located.owner, located.core_lo))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts the tiles routing emits to.
    struct Emits(usize);

    impl RouteSink<Coord<i32>> for Emits {
        fn begin_polyline(&mut self) {}

        fn begin_segment(&mut self) {}

        fn emit(
            &mut self,
            _: TileId,
            _: Coord<i32>,
            _: Coord<i32>,
            _: Coord<i32>,
        ) -> Result<(), TileError> {
            self.0 += 1;
            Ok(())
        }
    }

    #[test]
    fn budget_bounds_segments_inside_one_tile() {
        // Both segments stay in one tile's inner box (the fast path); the budget covers only the first.
        let grid = Grid::new(4096, 8).expect("config");
        let line = [(100, 100), (200, 100), (300, 100)].map(|(x, y)| Coord { x, y });
        let (mut budget, mut sink) = (1, Emits(0));
        let err = grid.route_within(&line, &mut sink, &mut budget).err();
        assert_eq!(err, Some(TileError::TooManyTiles));
        assert_eq!(
            sink.0, 1,
            "the first segment was routed before the budget ran out"
        );
    }

    /// Records the tiles each `emit` reaches, in order.
    struct Tiles(Vec<TileId>);

    impl RouteSink<Coord<i32>> for Tiles {
        fn begin_polyline(&mut self) {}
        fn begin_segment(&mut self) {}
        fn emit(
            &mut self,
            tile: TileId,
            _: Coord<i32>,
            _: Coord<i32>,
            _: Coord<i32>,
        ) -> Result<(), TileError> {
            self.0.push(tile);
            Ok(())
        }
    }

    /// The tiles the walk routes segment `a`–`c` into, and the visits it charged for them.
    fn walked(grid: Grid, a: Coord<i32>, c: Coord<i32>) -> Result<(Vec<TileId>, i64), TileError> {
        let mut sink = Tiles(Vec::new());
        let mut budget = MAX_TILE_VISITS;
        grid.walk(a, c, &mut sink, &mut budget)?;
        Ok((sink.0, MAX_TILE_VISITS - budget))
    }

    /// The previous routing: test every tile of the segment's buffered bounding box, row-major.
    fn scanned(grid: Grid, a: Coord<i32>, c: Coord<i32>) -> Result<Vec<TileId>, TileError> {
        let (extent, buffer) = (i64::from(grid.extent), i64::from(grid.buffer));
        let tile = |v: i32, d: i64| i32::try_from((i64::from(v) + d).div_euclid(extent)).unwrap();
        let mut out = Vec::new();
        for ty in tile(a.y.min(c.y), -buffer)..=tile(a.y.max(c.y), buffer) {
            for tx in tile(a.x.min(c.x), -buffer)..=tile(a.x.max(c.x), buffer) {
                let tile = TileId::new(tx, ty);
                let (min, max) = grid.tile_buffered_bounds(tile)?;
                if segment_intersects(a, c, min, max) {
                    out.push(tile);
                }
            }
        }
        Ok(out)
    }

    /// The walk visits exactly the tiles (in the same order) that the bounding-box scan accepts, and
    /// reports `Overflow` for exactly the same segments near the `i32` limits.
    #[test]
    fn walk_matches_bounding_box_scan() {
        // A deterministic xorshift stream, reduced to `0..n`.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut below = |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            i64::from(u32::try_from(state % u64::from(n)).unwrap())
        };
        let mut overflows = 0;
        for case in 0..20_000 {
            let extent: u32 = [1, 2, 3, 5, 7, 10, 25, 4096][case % 8];
            let buffer = u16::try_from(below(extent.div_ceil(2))).unwrap();
            let grid = Grid::new(extent, buffer).unwrap();
            // Spans of up to 40 tiles per axis, around an origin sometimes at the i32 limits.
            let span = extent * 40;
            let origin = match case % 5 {
                0 => i64::from(i32::MIN),
                1 => i64::from(i32::MAX) - i64::from(span),
                _ => -i64::from(span / 2),
            };
            let mut coord = || {
                let mut v = || i32::try_from(origin + below(span)).unwrap();
                Coord { x: v(), y: v() }
            };
            let (a, mut c) = (coord(), coord());
            // Bias towards axis-parallel and near-vertical segments, the walk's degenerate rows.
            match below(4) {
                0 => c.y = a.y,
                1 => c.x = a.x,
                2 => c.x = i32::try_from(i64::from(a.x) + below(3) - 1).unwrap_or(a.x),
                _ => {}
            }
            let walk = walked(grid, a, c).map(|(tiles, charged)| {
                // One charge per visited tile, plus one per row the walk crosses without a visit.
                let tile_y = |v: i32, d: i64| (i64::from(v) + d).div_euclid(i64::from(extent));
                let rows = tile_y(a.y.max(c.y), i64::from(buffer))
                    - tile_y(a.y.min(c.y), -i64::from(buffer))
                    + 1;
                let mut visited_rows: Vec<i32> = tiles.iter().map(|t| t.y).collect();
                visited_rows.dedup();
                let gap_rows = rows - i64::try_from(visited_rows.len()).unwrap();
                assert!(
                    buffer == 0 || gap_rows == 0,
                    "only unbuffered tiles leave gaps"
                );
                assert_eq!(
                    charged,
                    i64::try_from(tiles.len()).unwrap() + gap_rows,
                    "one charge per visit or gap row"
                );
                tiles
            });
            overflows += usize::from(walk == Err(TileError::Overflow));
            assert_eq!(walk, scanned(grid, a, c), "{extent}/{buffer} {a:?}-{c:?}");
        }
        assert!(
            overflows > 100,
            "the i32 limits are exercised ({overflows})"
        );
    }
}
