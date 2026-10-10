//! Clipping one polygon **ring** to a single tile's buffered box, keeping the **original** vertices
//! and closing the result with synthetic clip-boundary corners so the ring stays a closed, correctly
//! wound loop (see `docs/polygon-slicer.md`). The engine behind the polygon slicers.
//!
//! A vertex is kept by the same rule as the polyline clip (an incident edge touches the box); the
//! dropped outside excursions between kept arcs are replaced by detours that hug the box exterior
//! (`B⁺`, one unit outside `B`) via its corners. Each detour is built to be **homotopic** to the
//! excursion it replaces — it reproduces the excursion's winding around the box center — so the fill
//! inside the box is exact even when a ring wraps the tile. All synthetic geometry lies strictly
//! outside `B`, which lets [`Mosaic`](crate::Mosaic) recognise and drop it geometrically.
//!
//! [`close_ring`] is the shared core: given which edges touch the box and a way to measure an
//! excursion's winding, it closes the kept arcs. [`clip_ring`] (the single-tile clip) finds the
//! touching edges and windings by walking the whole ring; the all-tiles slicer feeds it the edges its
//! routing pass found and windings from its per-row crossing index, so both produce identical rings.

use core::cmp::Ordering;
use core::iter;

use geo_types::Coord;

use crate::TileError;
use crate::clip_polyline::line_meets_box;
use crate::geom::{point_in_ring, ring_orientation};
use crate::vertex::{PolyVertex, Vertex};

// Cohen–Sutherland outcode bits for a point vs the inclusive box `[min, max]`.
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const BOTTOM: u8 = 4;
const TOP: u8 = 8;
/// All four outside bits.
const BOX: u8 = LEFT | RIGHT | BOTTOM | TOP;
/// Not an outcode bit: set beside one for a point above the winding ray's line (`y > cy`).
const ABOVE: u8 = 16;

/// Outcode of `p` relative to the closed box `[min, max]` (0 iff `p` is inside).
fn outcode(p: Coord<i32>, min: Coord<i32>, max: Coord<i32>) -> u8 {
    let mut oc = 0;
    if p.x < min.x {
        oc |= LEFT;
    } else if p.x > max.x {
        oc |= RIGHT;
    }
    if p.y < min.y {
        oc |= BOTTOM;
    } else if p.y > max.y {
        oc |= TOP;
    }
    oc
}

/// Boundary slot `0..8` of an **outside** point, counter-clockwise with y up:
/// `0=SW, 1=S, 2=SE, 3=E, 4=NE, 5=N, 6=NW, 7=W`. Corners are the even slots. A zero outcode (inside)
/// cannot occur for a detour endpoint; it maps to `0` defensively so nothing can panic.
fn slot(oc: u8) -> u8 {
    match oc {
        _ if oc == BOTTOM | LEFT => 0,
        BOTTOM => 1,
        _ if oc == BOTTOM | RIGHT => 2,
        RIGHT => 3,
        _ if oc == RIGHT | TOP => 4,
        TOP => 5,
        _ if oc == TOP | LEFT => 6,
        LEFT => 7,
        _ => 0,
    }
}

/// The `B⁺` corner at an even `slot`, one unit outside the box. [`TileError::Overflow`] if the tile
/// sits so close to the `i32` edge that a corner can't be placed strictly outside it.
fn corner(slot: u8, min: Coord<i32>, max: Coord<i32>) -> Result<Coord<i32>, TileError> {
    let lo_x = min.x.checked_sub(1).ok_or(TileError::Overflow)?;
    let lo_y = min.y.checked_sub(1).ok_or(TileError::Overflow)?;
    let hi_x = max.x.checked_add(1).ok_or(TileError::Overflow)?;
    let hi_y = max.y.checked_add(1).ok_or(TileError::Overflow)?;
    Ok(match slot {
        0 => Coord { x: lo_x, y: lo_y }, // SW⁺
        2 => Coord { x: hi_x, y: lo_y }, // SE⁺
        4 => Coord { x: hi_x, y: hi_y }, // NE⁺
        _ => Coord { x: lo_x, y: hi_y }, // NW⁺ (slot 6)
    })
}

/// Signed crossing of segment `a → b` with the ray `{ y = cy, x > max_x }` — the part of the `+x` ray
/// from the box center that lies outside the box: `+1` upward, `-1` downward, `0` if it misses
/// (half-open in `y`, so a vertex shared by two edges is counted once). Summed along a path this is
/// the path's winding around the box center; since every excursion and every detour lives outside the
/// box, only the outside portion of the ray can be crossed.
fn ray_crossing(a: Coord<i32>, b: Coord<i32>, cy: i32, max_x: i32) -> i64 {
    if (a.y > cy) == (b.y > cy) {
        return 0;
    }
    // Crossing `x` compared to `max_x` without division: `x − max_x = num / dy`.
    let dy = i128::from(b.y) - i128::from(a.y);
    let num = (i128::from(a.x) - i128::from(max_x)) * dy
        + (i128::from(b.x) - i128::from(a.x)) * (i128::from(cy) - i128::from(a.y));
    // `x > max_x` iff `num` and `dy` share a sign (their product is positive).
    if num.signum() * dy.signum() > 0 {
        if b.y > a.y { 1 } else { -1 }
    } else {
        0
    }
}

/// [`ray_crossing`] summed over a path's consecutive points.
fn ray_crossings(path: impl IntoIterator<Item = Coord<i32>>, cy: i32, max_x: i32) -> i64 {
    let mut path = path.into_iter();
    let Some(mut prev) = path.next() else {
        return 0;
    };
    let mut w = 0;
    for p in path {
        w += ray_crossing(prev, p, cy, max_x);
        prev = p;
    }
    w
}

/// Corner slots visited walking `advance` slots (CCW if positive, CW if negative) from `start`: every
/// even slot passed except the final one (where the detour's other endpoint sits). `advance == 0`
/// yields none (a direct chord).
fn corner_slots(start: u8, advance: i64) -> impl Iterator<Item = u8> {
    // One slot CCW (`+1`) or CW (`+7 ≡ −1`) around the 8-slot loop, staying in `u8`.
    let step = if advance > 0 { 1 } else { 7 };
    let mut s = start;
    (1..advance.unsigned_abs()).filter_map(move |_| {
        s = (s + step) % 8;
        s.is_multiple_of(2).then_some(s)
    })
}

/// How many corners [`corner_slots`] yields, in O(1): of the `|advance| − 1` intermediate slots,
/// every other one is a corner, starting with the first when `start` is odd.
fn corner_count(start: u8, advance: i64) -> u64 {
    let n = advance.unsigned_abs().saturating_sub(1);
    if start.is_multiple_of(2) {
        n / 2
    } else {
        n.div_ceil(2)
    }
}

/// The box's center `y` (overflow-safe midpoint) — the height of the winding ray.
pub(crate) fn center_y(min: Coord<i32>, max: Coord<i32>) -> i32 {
    i32::midpoint(min.y, max.y)
}

/// How to bridge one dropped gap between kept arcs.
enum Bridge {
    /// Keep the gap's original vertices verbatim (no more of them than a detour would need).
    Keep,
    /// Replace the gap with the synthetic `B⁺` corners from [`corner_slots`]`(start, advance)`.
    Detour { start: u8, advance: i64 },
}

/// Decide how to bridge a gap of `gap_len` dropped vertices from exit vertex `u` to entry vertex `v`
/// (both outside the box) whose excursion winds `w_exc` times around the box center ([`ray_crossing`]
/// summed over its edges). The detour reproduces that winding, so it is homotopic to the excursion in
/// the box exterior: the CCW corner path from `u`'s slot to `v`'s is the reference, and whole `∂B⁺`
/// loops (±8 slots each) make up the winding difference.
///
/// Every gap vertex is outside the box (both its edges miss it — that's why it was dropped), so keeping
/// the gap verbatim reproduces the real excursion with its exact winding, entirely outside the box.
/// Synthesizing only ever trades those originals for `B⁺` corners, so it's worth doing only when it
/// *saves* vertices — a longer excursion that would otherwise drag its far geometry into this tile.
/// Either way `Mosaic` recovers the real geometry from the tiles that own it.
fn plan_bridge(
    u: Coord<i32>,
    v: Coord<i32>,
    w_exc: i64,
    gap_len: usize,
    min: Coord<i32>,
    max: Coord<i32>,
    cy: i32,
) -> Result<Bridge, TileError> {
    let su = slot(outcode(u, min, max));
    let sv = slot(outcode(v, min, max));
    let base = i64::from((sv + 8 - su) % 8); // 0..8, CCW distance su → sv
    // Winding of the reference detour (at most three corners).
    let mut w_ref = 0;
    let mut prev = u;
    for s in corner_slots(su, base) {
        let c = corner(s, min, max)?;
        w_ref += ray_crossing(prev, c, cy, max.x);
        prev = c;
    }
    w_ref += ray_crossing(prev, v, cy, max.x);
    let advance = (w_exc - w_ref)
        .checked_mul(8)
        .and_then(|loops| loops.checked_add(base))
        .ok_or(TileError::Overflow)?;
    let keep = u64::try_from(gap_len).is_ok_and(|g| g <= corner_count(su, advance));
    Ok(if keep {
        Bridge::Keep
    } else {
        Bridge::Detour { start: su, advance }
    })
}

/// Append to `out` the distinct vertices of `ring` in cyclic order — consecutive duplicate positions
/// and a repeated closing vertex dropped, so zero-length edges can't distort the clip (matching the
/// polyline slicer's handling of consecutive duplicates). Returns how many were appended.
pub(crate) fn push_distinct<V: Vertex>(ring: &[V], out: &mut Vec<V>) -> usize {
    let start = out.len();
    for &v in ring {
        if out.len() == start || out.last().map(Vertex::position) != Some(v.position()) {
            out.push(v);
        }
    }
    while out.len() - start >= 2 && out.last().map(Vertex::position) == Some(out[start].position())
    {
        out.pop();
    }
    out.len() - start
}

/// Append the `B⁺` fill box as a closed ring oriented like `orient` (a solid tile fill), all synthetic.
pub(crate) fn push_fill_box<V: PolyVertex>(
    out: &mut Vec<V>,
    orient: Ordering,
    min: Coord<i32>,
    max: Coord<i32>,
) -> Result<(), TileError> {
    let ring = fill_box(orient, corner(0, min, max)?, corner(4, min, max)?);
    out.extend(ring.into_iter().map(V::synthetic_at));
    Ok(())
}

/// The closed fill-box ring through the `B⁺` corners `sw` and `ne`, oriented like `orient`.
pub(crate) fn fill_box(orient: Ordering, sw: Coord<i32>, ne: Coord<i32>) -> [Coord<i32>; 5] {
    let se = Coord { x: ne.x, y: sw.y };
    let nw = Coord { x: sw.x, y: ne.y };
    // Counter-clockwise for a CCW ring, clockwise otherwise, so the fill matches the ring's sense.
    if orient == Ordering::Less {
        [sw, nw, ne, se, sw]
    } else {
        [sw, se, ne, nw, sw]
    }
}

/// Close one ring's kept arcs into a single closed ring (first vertex repeated at the end), appended
/// to `out` in the ring's own coordinate frame.
///
/// `ring` holds the ring's distinct vertices in cyclic order (see [`push_distinct`]); edge `i` runs
/// `ring[i] → ring[(i + 1) % m]`. `touching` lists, ascending and non-empty, the edges that touch the
/// box `[min, max]` — a vertex is kept iff an incident edge touches. `winding(first, count)` must return
/// [`ray_crossing`] summed over the `count` consecutive edges from edge `first` (cyclically), for this
/// box's ray; the caller supplies it so the single-tile clip can walk the edges while the all-tiles
/// slicer answers from its per-row crossing index. `arcs` is caller-owned scratch.
///
/// Output starts at the first arc in index order, so it is a pure function of the inputs.
///
/// # Errors
///
/// [`TileError::Overflow`] if a synthetic corner can't be placed strictly outside the box.
#[inline]
pub(crate) fn close_ring<V: PolyVertex>(
    ring: &[V],
    touching: &[u32],
    min: Coord<i32>,
    max: Coord<i32>,
    mut winding: impl FnMut(usize, usize) -> i64,
    arcs: &mut Vec<(usize, usize)>,
    out: &mut Vec<V>,
) -> Result<(), TileError> {
    let m = ring.len();
    // Kept arcs as inclusive vertex spans `(start, end)`, `end` unwrapped (it may pass `m` by wrapping
    // to the ring's start). Edge `t` keeps vertices `t` and `t + 1`, so two touching edges at most two
    // apart keep one contiguous arc (the edge between them is a single-segment out-and-back bridge).
    arcs.clear();
    for &t in touching {
        let t = t as usize;
        match arcs.last_mut() {
            Some((_, end)) if t <= *end + 1 => *end = t + 1,
            _ => arcs.push((t, t + 1)),
        }
    }
    // The last arc may run on, across the ring's seam, into the first.
    let mut first = 0;
    if let (Some(&(s0, e0)), Some(last)) = (arcs.first(), arcs.len().checked_sub(1))
        && last > 0
        && arcs[last].1 + 1 >= s0 + m
    {
        arcs[last].1 = e0 + m;
        first = 1;
    }
    let arcs = &arcs[first..];
    if let [(s, e)] = *arcs
        && e - s + 1 >= m
    {
        // Every vertex is kept: the ring is returned verbatim, re-closed.
        out.extend_from_slice(ring);
        out.push(ring[0]);
        return Ok(());
    }

    let cy = center_y(min, max);
    let ring_start = out.len();
    for (j, &(s, e)) in arcs.iter().enumerate() {
        out.extend((s..=e).map(|i| ring[i % m]));
        // Bridge the gap to the next arc (cyclically): the dropped vertices `e+1 .. next`.
        let next = arcs.get(j + 1).map_or(arcs[0].0 + m, |a| a.0);
        let (u, v) = (ring[e % m].position(), ring[next % m].position());
        let w_exc = winding(e % m, next - e);
        match plan_bridge(u, v, w_exc, next - e - 1, min, max, cy)? {
            Bridge::Keep => out.extend((e + 1..next).map(|i| ring[i % m])),
            Bridge::Detour { start, advance } => {
                let before = out.len();
                for s in corner_slots(start, advance) {
                    out.push(V::synthetic_at(corner(s, min, max)?));
                }
                debug_assert_eq!(
                    ray_crossings(
                        iter::once(u)
                            .chain(out[before..].iter().map(Vertex::position))
                            .chain(iter::once(v)),
                        cy,
                        max.x
                    ),
                    w_exc,
                    "detour winding must match the excursion it replaces"
                );
            }
        }
    }
    out.push(out[ring_start]); // close the ring
    Ok(())
}

/// The outcome of clipping one ring to a tile box.
pub(crate) enum RingClip<V> {
    /// The ring does not appear in this tile.
    Outside,
    /// The tile lies entirely inside the ring: a solid fill (the synthetic `B⁺` box). For an exterior
    /// ring this means a fully-filled tile; for a hole it means the tile is entirely a hole.
    Covers(Vec<V>),
    /// The ring crosses the tile; the clipped closed ring (original vertices + synthetic detours).
    Clipped(Vec<V>),
}

/// Clip one closed `ring` to the inclusive buffered box `[min, max]`, keeping original vertices and
/// closing the result with synthetic `B⁺` corners. Returns the closed output ring in the input's own
/// coordinate frame (first vertex repeated at the end), distinguishing a whole-tile [`RingClip::Covers`]
/// fill from a partial [`RingClip::Clipped`] crossing (so the caller can drop a tile that a hole
/// entirely covers) and [`RingClip::Outside`] when the ring misses the tile.
///
/// # Errors
///
/// - [`TileError::Overflow`] if the tile sits so close to the `i32` edge that a synthetic corner can't
///   be placed strictly outside the box.
/// - [`TileError::GeometryTooLarge`] if the ring has more than `u32::MAX` distinct vertices.
pub(crate) fn clip_ring<V: PolyVertex>(
    ring: &[V],
    min: Coord<i32>,
    max: Coord<i32>,
) -> Result<RingClip<V>, TileError> {
    // Borrow the ring when it is already distinct (the usual case), copying only to drop duplicates.
    let mut copy = Vec::new();
    let pts = if let Some(n) = distinct_prefix(ring) {
        &ring[..n]
    } else {
        push_distinct(ring, &mut copy);
        copy.as_slice()
    };
    let m = pts.len();
    if m < 3 {
        return Ok(RingClip::Outside);
    }
    u32::try_from(m).map_err(|_| TileError::GeometryTooLarge)?;

    // One pass over the edges. Each vertex's outcode is computed once and shared by its two edges: a
    // shared outside bit rejects an edge (the bounding-box test of `segment_intersects`), an inside
    // endpoint accepts it, and only the rest need the exact line test. The same pass records every
    // edge crossing the winding ray (its endpoints on either side of `y = cy`, the only edges
    // `ray_crossing` can count), so an excursion's winding is a sum over those few, not a walk.
    let cy = center_y(min, max);
    let code = |p: Coord<i32>| outcode(p, min, max) | if p.y > cy { ABOVE } else { 0 };
    let mut touching = Vec::new();
    let mut crossings: Vec<(u32, i64)> = Vec::new();
    let (mut a, mut ca) = (pts[0].position(), code(pts[0].position()));
    for (i, b) in (0..).zip(pts[1..].iter().chain(iter::once(&pts[0]))) {
        let b = b.position();
        let cb = code(b);
        let (oa, ob) = (ca & BOX, cb & BOX);
        if oa & ob == 0 && (oa == 0 || ob == 0 || line_meets_box(a, b, min, max)) {
            touching.push(i);
        }
        if (ca ^ cb) & ABOVE != 0 {
            let w = ray_crossing(a, b, cy, max.x);
            if w != 0 {
                crossings.push((i, w));
            }
        }
        (a, ca) = (b, cb);
    }

    if touching.is_empty() {
        // No edge touches the box → the tile is uniformly inside or outside the ring, and the box
        // center's `+x` ray meets edges only outside the box — exactly the recorded crossings — so their
        // parity is the even-odd containment of the whole tile.
        let inside = crossings.len() % 2 == 1;
        debug_assert_eq!(
            inside,
            point_in_ring(min, &pts.iter().map(Vertex::position).collect::<Vec<_>>()),
            "ray parity must agree with point-in-ring containment"
        );
        return if inside {
            let mut fill = Vec::with_capacity(5);
            push_fill_box(&mut fill, ring_orientation(pts), min, max)?;
            Ok(RingClip::Covers(fill))
        } else {
            Ok(RingClip::Outside)
        };
    }

    // Signed crossings among edges `lo..hi` (both within `0..=m`).
    let sum = |lo: usize, hi: usize| -> i64 {
        let i = crossings.partition_point(|c| (c.0 as usize) < lo);
        let j = crossings.partition_point(|c| (c.0 as usize) < hi);
        crossings[i..j].iter().map(|c| c.1).sum()
    };
    let winding = |first: usize, count: usize| {
        let w = if first + count <= m {
            sum(first, first + count)
        } else {
            sum(first, m) + sum(0, first + count - m)
        };
        debug_assert_eq!(
            w,
            ray_crossings(
                (first..=first + count).map(|i| pts[i % m].position()),
                cy,
                max.x
            ),
            "indexed winding must match walking the excursion"
        );
        w
    };
    let mut out = Vec::new();
    close_ring(pts, &touching, min, max, winding, &mut Vec::new(), &mut out)?;
    Ok(RingClip::Clipped(out))
}

/// The length of `ring` without its repeated closing vertices, if that prefix has no consecutive
/// duplicate positions — exactly what [`push_distinct`] would copy, so it can be borrowed instead.
/// `None` when an inner duplicate needs the copy.
fn distinct_prefix<V: Vertex>(ring: &[V]) -> Option<usize> {
    let first = ring.first()?.position();
    let mut n = ring.len();
    while n >= 2 && ring[n - 1].position() == first {
        n -= 1;
    }
    ring[..n]
        .windows(2)
        .all(|w| w[0].position() != w[1].position())
        .then_some(n)
}

#[cfg(test)]
mod tests {
    use geo_types::Coord;

    use super::*;

    fn c(x: i32, y: i32) -> Coord<i32> {
        Coord { x, y }
    }

    // Tile (0,0), extent 10, buffer 0 → inclusive box [0,0]..[9,9].
    const MIN: Coord<i32> = Coord { x: 0, y: 0 };
    const MAX: Coord<i32> = Coord { x: 9, y: 9 };

    /// Even-odd winding of the *positions* of a clipped ring at `p`, to check the fill is correct.
    fn fill_at(ring: &[Coord<i32>], p: Coord<i32>) -> bool {
        point_in_ring(p, ring)
    }

    fn positions<V: Vertex>(ring: &[V]) -> Vec<Coord<i32>> {
        ring.iter().map(Vertex::position).collect()
    }

    /// The ring of a non-empty clip (panics on [`RingClip::Outside`]).
    fn clipped(rc: RingClip<Coord<i32>>) -> Vec<Coord<i32>> {
        match rc {
            RingClip::Covers(v) | RingClip::Clipped(v) => v,
            RingClip::Outside => panic!("expected geometry, got Outside"),
        }
    }

    #[test]
    fn fully_inside_is_kept_verbatim() {
        let ring = [c(2, 2), c(7, 2), c(7, 7), c(2, 7), c(2, 2)];
        let out = clipped(clip_ring(&ring, MIN, MAX).unwrap());
        assert_eq!(out, ring.to_vec(), "an inside ring is returned unchanged");
    }

    #[test]
    fn single_vertex_gap_is_kept_not_synthesized() {
        // Box [0,9]. This square wraps the tile's SW corner; clipped to the box only its far corner
        // (50,50) is dropped (both its edges miss the box), a one-vertex gap between the exit (5,50)
        // and the re-entry (50,5). That lone vertex must be kept verbatim, not replaced by synthetic
        // `B⁺` corners — so every output vertex is an original input vertex.
        let ring = [c(5, 5), c(5, 50), c(50, 50), c(50, 5), c(5, 5)];
        let out = clipped(clip_ring(&ring, MIN, MAX).unwrap());
        let pts = positions(&out);
        let original: std::collections::BTreeSet<(i32, i32)> =
            [(5, 5), (5, 50), (50, 50), (50, 5)].into_iter().collect();
        assert!(
            pts.iter().all(|q| original.contains(&(q.x, q.y))),
            "no synthetic corners were introduced: {pts:?}"
        );
        assert!(
            pts.iter().any(|q| (q.x, q.y) == (50, 50)),
            "the single dropped vertex is kept intact"
        );
        // Fill is still correct: the tile's SW corner is outside the square, its NE inside.
        assert!(!fill_at(&pts, c(1, 1)));
        assert!(fill_at(&pts, c(8, 8)));
    }

    #[test]
    fn fully_outside_disjoint_is_none() {
        // A small ring far to the right, not enclosing the tile.
        let ring = [
            c(100, 100),
            c(110, 100),
            c(110, 110),
            c(100, 110),
            c(100, 100),
        ];
        assert!(matches!(
            clip_ring(&ring, MIN, MAX).unwrap(),
            RingClip::Outside
        ));
    }

    #[test]
    fn containment_fills_the_box() {
        // A big ring that encloses the whole tile with no edge touching it → solid fill.
        let ring = [c(-50, -50), c(50, -50), c(50, 50), c(-50, 50), c(-50, -50)];
        let rc = clip_ring(&ring, MIN, MAX).unwrap();
        assert!(
            matches!(rc, RingClip::Covers(_)),
            "a fully-enclosed tile is reported as a solid fill"
        );
        let out = clipped(rc);
        let pts = positions(&out);
        // Every vertex is synthetic (strictly outside the box) and the whole tile is filled.
        assert!(pts.iter().all(|q| outcode(*q, MIN, MAX) != 0));
        assert!(fill_at(&pts, c(5, 5)));
        assert!(fill_at(&pts, c(0, 0)));
        assert!(fill_at(&pts, c(9, 9)));
    }

    #[test]
    fn edge_through_tile_all_vertices_outside() {
        // Every vertex is outside the box, but the hypotenuse (line x+y=9) cuts through the tile: a
        // big right triangle covering the lower-left half-plane {x+y<9}. The filled side must win.
        let ring = [c(-100, -100), c(109, -100), c(-100, 109), c(-100, -100)];
        let out = clipped(clip_ring(&ring, MIN, MAX).unwrap());
        let pts = positions(&out);
        assert!(
            fill_at(&pts, c(1, 1)),
            "lower-left of the cut is filled (x+y=2 < 9)"
        );
        assert!(
            !fill_at(&pts, c(8, 8)),
            "upper-right of the cut is not filled (x+y=16 > 9)"
        );
    }

    #[test]
    fn crossing_ring_keeps_original_crossing_vertices() {
        // A ring straddling the right edge: two vertices inside, two outside.
        let ring = [c(4, 3), c(15, 3), c(15, 6), c(4, 6), c(4, 3)];
        let out = clipped(clip_ring(&ring, MIN, MAX).unwrap());
        let pts = positions(&out);
        // Original inside/near vertices are preserved; the fill inside the box is the left strip.
        assert!(pts.contains(&c(4, 3)) && pts.contains(&c(4, 6)));
        assert!(fill_at(&pts, c(6, 4)), "inside the ring, inside the tile");
        assert!(!fill_at(&pts, c(1, 8)), "outside the ring");
    }

    #[test]
    fn encircling_notch_wrap_case() {
        // A ring that surrounds the tile (all around, far outside) with a thin notch stabbing UP into
        // the tile from the bottom. The excursion between the two notch crossings wraps the whole box,
        // so a naive detour would fill only the notch. Correct fill = tile minus the notch.
        let ring = [
            c(3, -50), // up into the tile (notch left side)
            c(3, 6),
            c(6, 6),
            c(6, -50),  // back down (notch right side)
            c(60, -50), // around the outside, far from the box, all the way around …
            c(60, 60),
            c(-60, 60),
            c(-60, -50),
            c(3, -50),
        ];
        let out = clipped(clip_ring(&ring, MIN, MAX).unwrap());
        let pts = positions(&out);
        // Inside the notch (x in 3..6, low y) is NOT filled; the rest of the tile IS filled.
        assert!(!fill_at(&pts, c(4, 2)), "the notch is a hole in the fill");
        assert!(fill_at(&pts, c(1, 5)), "left of the notch is filled");
        assert!(fill_at(&pts, c(8, 5)), "right of the notch is filled");
        assert!(fill_at(&pts, c(4, 8)), "above the notch is filled");
    }
}
