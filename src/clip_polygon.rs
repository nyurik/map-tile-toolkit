//! Clipping one polygon **ring** to a single tile's buffered box, keeping the **original** vertices
//! and closing the result with synthetic clip-boundary corners so the ring stays a closed, correctly
//! wound loop (see `docs/polygon-slicer.md`). The engine behind the polygon slicers.
//!
//! A vertex is kept by the same rule as the polyline clip (an incident edge touches the box); the
//! dropped outside excursions between kept arcs are replaced by detours that hug the box exterior
//! (`B⁺`, one unit outside `B`) via its corners. Each detour is built to be **homotopic** to the
//! excursion it replaces — it reproduces the excursion's winding around the box centre — so the fill
//! inside the box is exact even when a ring wraps the tile. All synthetic geometry lies strictly
//! outside `B`, which lets [`Mosaic`](crate::Mosaic) recognise and drop it geometrically.

use core::cmp::Ordering;

use geo_types::Coord;

use crate::TileError;
use crate::clip_polyline::segment_intersects;
use crate::geom::{point_in_ring, ring_orientation};
use crate::vertex::{PolyVertex, Vertex};

// Cohen–Sutherland outcode bits for a point vs the inclusive box `[min, max]`.
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const BOTTOM: u8 = 4;
const TOP: u8 = 8;

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

/// Signed crossings of the polyline with the ray `{ y = cy, x > max_x }` — the part of the `+x` ray
/// from the box centre that lies outside the box. `+1` for an upward crossing, `-1` for downward
/// (half-open in `y`, so a shared vertex is counted once). This is the winding of the (open) path
/// around the box centre; since every excursion and every detour lives outside the box, only the
/// outside portion of the ray can be crossed.
fn ray_crossings(poly: &[Coord<i32>], cy: i32, max_x: i32) -> i32 {
    let mut w = 0;
    for win in poly.windows(2) {
        let (a, b) = (win[0], win[1]);
        if (a.y > cy) != (b.y > cy) {
            // Crossing `x` compared to `max_x` without division: `x − max_x = num / dy`.
            let dy = i128::from(b.y) - i128::from(a.y);
            let num = (i128::from(a.x) - i128::from(max_x)) * dy
                + (i128::from(b.x) - i128::from(a.x)) * (i128::from(cy) - i128::from(a.y));
            // `x > max_x` iff `num` and `dy` share a sign (their product is positive).
            if (num.signum() * dy.signum()) > 0 {
                w += if b.y > a.y { 1 } else { -1 };
            }
        }
    }
    w
}

/// Corner points visited walking `advance` slots (CCW if positive, CW if negative) from `start`,
/// emitting the `B⁺` corner at every even slot passed except the final one (where the detour's other
/// endpoint sits). `advance == 0` yields no corners (a direct chord).
fn corners_by_advance(
    start: u8,
    advance: i32,
    min: Coord<i32>,
    max: Coord<i32>,
) -> Result<Vec<Coord<i32>>, TileError> {
    let mut out = Vec::new();
    if advance == 0 {
        return Ok(out);
    }
    let forward = advance > 0;
    let steps = advance.unsigned_abs();
    let mut s = start;
    for step in 1..=steps {
        // Step one slot around the 8-slot loop, CCW (`+1`) or CW (`+7 ≡ −1`), staying in `u8`.
        s = if forward { (s + 1) % 8 } else { (s + 7) % 8 };
        if step != steps && s.is_multiple_of(2) {
            out.push(corner(s, min, max)?);
        }
    }
    Ok(out)
}

/// The box's centre `y` (overflow-safe midpoint).
fn center_y(min: Coord<i32>, max: Coord<i32>) -> i32 {
    i32::midpoint(min.y, max.y)
}

/// The synthetic detour corners bridging the dropped gap from exit vertex `u` to entry vertex `v`
/// (both outside the box). `excursion` is `[u, dropped…, v]`; the detour is built to reproduce its
/// winding around the box centre, so it is homotopic to the excursion in the box exterior.
fn detour<V: PolyVertex>(
    u: Coord<i32>,
    v: Coord<i32>,
    excursion: &[Coord<i32>],
    min: Coord<i32>,
    max: Coord<i32>,
    cy: i32,
) -> Result<Vec<V>, TileError> {
    let su = slot(outcode(u, min, max));
    let sv = slot(outcode(v, min, max));
    let base_ccw = i32::from((sv + 8 - su) % 8); // 0..8, CCW distance su → sv

    // Reference detour (CCW, no extra loops) and its winding.
    let ref_corners = corners_by_advance(su, base_ccw, min, max)?;
    let mut ref_poly = Vec::with_capacity(ref_corners.len() + 2);
    ref_poly.push(u);
    ref_poly.extend_from_slice(&ref_corners);
    ref_poly.push(v);
    let w_ref = ray_crossings(&ref_poly, cy, max.x);
    let w_exc = ray_crossings(excursion, cy, max.x);

    // Add whole `∂B⁺` loops so the detour's winding matches the excursion's (each loop = ±8 slots).
    let advance = base_ccw + (w_exc - w_ref) * 8;
    let corners = corners_by_advance(su, advance, min, max)?;

    debug_assert!(
        {
            let mut check = Vec::with_capacity(corners.len() + 2);
            check.push(u);
            check.extend_from_slice(&corners);
            check.push(v);
            ray_crossings(&check, cy, max.x) == w_exc
        },
        "detour winding must match the excursion it replaces"
    );

    Ok(corners.into_iter().map(V::synthetic_at).collect())
}

/// The `B⁺` fill box as a closed ring oriented like `orient` (a solid tile fill), all synthetic.
fn fill_box<V: PolyVertex>(
    orient: Ordering,
    min: Coord<i32>,
    max: Coord<i32>,
) -> Result<Vec<V>, TileError> {
    let sw = corner(0, min, max)?;
    let se = corner(2, min, max)?;
    let ne = corner(4, min, max)?;
    let nw = corner(6, min, max)?;
    // Counter-clockwise for a CCW ring, clockwise otherwise, so the fill matches the ring's sense.
    let seq = if orient == Ordering::Less {
        [sw, nw, ne, se, sw]
    } else {
        [sw, se, ne, nw, sw]
    };
    Ok(seq.into_iter().map(V::synthetic_at).collect())
}

/// Clip one closed `ring` to the inclusive buffered box `[min, max]`, keeping original vertices and
/// closing the result with synthetic `B⁺` corners. Returns the single closed output ring in the
/// input's own coordinate frame (first vertex repeated at the end), or `None` if the ring does not
/// appear in this tile.
///
/// # Errors
///
/// [`TileError::Overflow`] if the tile sits so close to the `i32` edge that a synthetic corner can't
/// be placed strictly outside the box.
pub(crate) fn clip_ring<V: PolyVertex>(
    ring: &[V],
    min: Coord<i32>,
    max: Coord<i32>,
) -> Result<Option<Vec<V>>, TileError> {
    if ring.len() < 3 {
        return Ok(None);
    }
    // Distinct-position vertices in cyclic order (drop consecutive duplicates and the repeated closing
    // vertex), so zero-length edges can't distort the clip — matching the polyline slicer's handling
    // of consecutive duplicates.
    let mut pts: Vec<V> = Vec::with_capacity(ring.len());
    for &v in ring {
        if pts.last().map(Vertex::position) != Some(v.position()) {
            pts.push(v);
        }
    }
    while pts.len() >= 2 && pts[0].position() == pts[pts.len() - 1].position() {
        pts.pop();
    }
    let m = pts.len();
    if m < 3 {
        return Ok(None);
    }
    let pos = |i: usize| pts[i].position();

    // Which edges (`pts[i] → pts[i+1]`, cyclic) touch the box.
    let touches: Vec<bool> = (0..m)
        .map(|i| segment_intersects(pos(i), pos((i + 1) % m), min, max))
        .collect();

    if !touches.iter().any(|&t| t) {
        // No edge touches the box → the tile is uniformly inside or outside the ring.
        let ring_pts: Vec<Coord<i32>> = (0..m).map(pos).collect();
        return if point_in_ring(min, &ring_pts) {
            Ok(Some(fill_box(ring_orientation(&ring_pts), min, max)?))
        } else {
            Ok(None)
        };
    }

    // A vertex is kept iff an incident edge touches the box (same rule as the polyline clip).
    let kept: Vec<bool> = (0..m)
        .map(|i| touches[(i + m - 1) % m] || touches[i])
        .collect();

    if kept.iter().all(|&k| k) {
        // Whole ring is near/inside the box: keep it verbatim, re-closed.
        let mut out: Vec<V> = pts.clone();
        out.push(pts[0]);
        return Ok(Some(out));
    }

    // Start at an arc boundary (a kept vertex whose predecessor is dropped) so the cyclic walk splits
    // cleanly into arcs and the gaps between them.
    let Some(start) = (0..m).find(|&i| kept[i] && !kept[(i + m - 1) % m]) else {
        let mut out: Vec<V> = pts.clone();
        out.push(pts[0]);
        return Ok(Some(out));
    };

    // Collect kept arcs and the dropped gaps between them, in cyclic order from `start`.
    let mut arcs: Vec<Vec<usize>> = Vec::new();
    let mut gaps: Vec<Vec<usize>> = Vec::new();
    let mut cur_arc: Vec<usize> = Vec::new();
    let mut cur_gap: Vec<usize> = Vec::new();
    let mut in_arc = true;
    for step in 0..m {
        let idx = (start + step) % m;
        if kept[idx] {
            if !in_arc {
                gaps.push(std::mem::take(&mut cur_gap));
                in_arc = true;
            }
            cur_arc.push(idx);
        } else {
            if in_arc {
                arcs.push(std::mem::take(&mut cur_arc));
                in_arc = false;
            }
            cur_gap.push(idx);
        }
    }
    // The walk ends inside the final (wrap-around) gap, since `start`'s predecessor is dropped.
    if !cur_arc.is_empty() {
        arcs.push(cur_arc);
    }
    gaps.push(cur_gap);

    let cy = center_y(min, max);
    let mut out: Vec<V> = Vec::new();
    for j in 0..arcs.len() {
        for &vi in &arcs[j] {
            out.push(pts[vi]);
        }
        // Bridge the gap to the next arc (cyclically). Every gap vertex is outside the box (both its
        // edges miss it — that's why it was dropped), so keeping the gap verbatim reproduces the real
        // excursion with its exact winding, entirely outside the box. Synthesizing a winding-matched
        // detour only ever trades those originals for `B⁺` corners, so it's worth doing only when it
        // *saves* vertices: keep the originals whenever the gap is no larger than the detour would be,
        // and synthesize only for a longer excursion (which would otherwise drag its far geometry into
        // this tile). Either way `Mosaic` recovers the real geometry from the tiles that own it.
        let gap = &gaps[j];
        let u = pos(*arcs[j].last().expect("arcs are non-empty"));
        let next = &arcs[(j + 1) % arcs.len()];
        let v = pos(next[0]);
        let mut excursion = Vec::with_capacity(gap.len() + 2);
        excursion.push(u);
        excursion.extend(gap.iter().map(|&gi| pos(gi)));
        excursion.push(v);
        let synthesized = detour::<V>(u, v, &excursion, min, max, cy)?;
        if gap.len() <= synthesized.len() {
            out.extend(gap.iter().map(|&gi| pts[gi]));
        } else {
            out.extend(synthesized);
        }
    }
    out.push(out[0]); // close the ring
    Ok(Some(out))
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

    #[test]
    fn fully_inside_is_kept_verbatim() {
        let ring = [c(2, 2), c(7, 2), c(7, 7), c(2, 7), c(2, 2)];
        let out = clip_ring(&ring, MIN, MAX).unwrap().unwrap();
        assert_eq!(out, ring.to_vec(), "an inside ring is returned unchanged");
    }

    #[test]
    fn single_vertex_gap_is_kept_not_synthesized() {
        // Box [0,9]. This square wraps the tile's SW corner; clipped to the box only its far corner
        // (50,50) is dropped (both its edges miss the box), a one-vertex gap between the exit (5,50)
        // and the re-entry (50,5). That lone vertex must be kept verbatim, not replaced by synthetic
        // `B⁺` corners — so every output vertex is an original input vertex.
        let ring = [c(5, 5), c(5, 50), c(50, 50), c(50, 5), c(5, 5)];
        let out = clip_ring(&ring, MIN, MAX).unwrap().unwrap();
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
        assert_eq!(clip_ring(&ring, MIN, MAX).unwrap(), None);
    }

    #[test]
    fn containment_fills_the_box() {
        // A big ring that encloses the whole tile with no edge touching it → solid fill.
        let ring = [c(-50, -50), c(50, -50), c(50, 50), c(-50, 50), c(-50, -50)];
        let out = clip_ring(&ring, MIN, MAX).unwrap().unwrap();
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
        let out = clip_ring(&ring, MIN, MAX).unwrap().unwrap();
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
        let out = clip_ring(&ring, MIN, MAX).unwrap().unwrap();
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
        let out = clip_ring(&ring, MIN, MAX).unwrap().unwrap();
        let pts = positions(&out);
        // Inside the notch (x in 3..6, low y) is NOT filled; the rest of the tile IS filled.
        assert!(!fill_at(&pts, c(4, 2)), "the notch is a hole in the fill");
        assert!(fill_at(&pts, c(1, 5)), "left of the notch is filled");
        assert!(fill_at(&pts, c(8, 5)), "right of the notch is filled");
        assert!(fill_at(&pts, c(4, 8)), "above the notch is filled");
    }
}
