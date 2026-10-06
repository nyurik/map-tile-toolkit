//! Exact integer geometry predicates shared by clipping, ring orientation, and point-in-ring tests.
//!
//! Every predicate is exact across the full `i32` coordinate range and never panics (no floats, no
//! division). A 2-D cross product of full-`i32` points can reach `2^65`, so exactness needs 128-bit
//! arithmetic *somewhere*; following [`i_overlay`]/`i_float` — which widen `i32 → i64` and
//! cross-multiply the edge *difference* vectors within a bounded coordinate domain — [`orient`]
//! evaluates the cross in `i64` on the difference vectors and only widens to `i128` when a difference
//! does not fit `i32` (the rare near-full-`i32`-span case). Ordinary tile-local geometry therefore
//! never pays the 128-bit width, while the crate keeps its full-`i32` contract.
//!
//! [`i_overlay`]: https://crates.io/crates/i_overlay

use core::cmp::Ordering;

use geo_types::Coord;

/// Orientation of the turn `a → b → c`: the sign of the cross product `(b − a) × (c − a)`.
///
/// [`Ordering::Greater`] is a left turn (counter-clockwise), [`Ordering::Less`] a right turn
/// (clockwise), and [`Ordering::Equal`] means the three points are collinear.
#[inline]
pub(crate) fn orient(a: Coord<i32>, b: Coord<i32>, c: Coord<i32>) -> Ordering {
    // Edge vectors as `i64` — an `i32 − i32` difference always fits `i64`.
    let ex1 = i64::from(b.x) - i64::from(a.x);
    let ey1 = i64::from(b.y) - i64::from(a.y);
    let ex2 = i64::from(c.x) - i64::from(a.x);
    let ey2 = i64::from(c.y) - i64::from(a.y);
    // Fast path: when every component fits `i32`, each product is `≤ (2^31 − 1)^2 < 2^62`, so the two
    // `i64` products compare exactly with no risk of overflow. Otherwise widen the two products to
    // `i128`. Comparing the products (rather than subtracting) avoids a possible `i64` overflow in the
    // difference and needs no wider type on the fast path.
    if fits_i32(ex1) && fits_i32(ey1) && fits_i32(ex2) && fits_i32(ey2) {
        (ex1 * ey2).cmp(&(ey1 * ex2))
    } else {
        (i128::from(ex1) * i128::from(ey2)).cmp(&(i128::from(ey1) * i128::from(ex2)))
    }
}

/// Whether an `i64` value fits back into an `i32`.
#[inline]
fn fits_i32(v: i64) -> bool {
    i32::try_from(v).is_ok()
}

/// The winding of a closed `ring` from the sign of its signed area: [`Ordering::Greater`] for
/// counter-clockwise, [`Ordering::Less`] for clockwise, [`Ordering::Equal`] for a degenerate ring
/// (zero area). Works whether or not the caller repeats the first vertex to close the ring.
///
/// Exact for any `i32` ring: the doubled signed area is accumulated in `i128`, which cannot overflow
/// for the crate's capped polyline length (`≤ 2^16` vertices, each cross term `< 2^63`).
pub(crate) fn ring_orientation(ring: &[Coord<i32>]) -> Ordering {
    if ring.len() < 3 {
        return Ordering::Equal;
    }
    // Shoelace measured about the first vertex, so each term is a cross product of edge vectors from
    // `v0` (smaller magnitudes keep the terms well inside `i128`). Measured this way, the closing edge
    // `v_{n-1} → v0` and the opening edge `v0 → v1` both contribute zero, so iterating `windows(2)`
    // gives the full doubled area whether or not the ring repeats its first vertex.
    let v0 = ring[0];
    let mut area2: i128 = 0;
    for w in ring.windows(2) {
        let (p, q) = (w[0], w[1]);
        let px = i128::from(p.x) - i128::from(v0.x);
        let py = i128::from(p.y) - i128::from(v0.y);
        let qx = i128::from(q.x) - i128::from(v0.x);
        let qy = i128::from(q.y) - i128::from(v0.y);
        area2 += px * qy - py * qx;
    }
    area2.cmp(&0)
}

/// Whether point `p` lies inside `ring` (a closed polygon ring), counting the boundary as **inside**.
///
/// Even-odd (crossing-number) ray cast along `+x`, evaluated with [`orient`] so there is no division
/// and it is exact for all `i32` inputs. A half-open rule on each edge's `y` span (`[y_lo, y_hi)`)
/// counts every crossing once, so a ray grazing a shared vertex is handled consistently. Points on an
/// edge or vertex return `true`. The ring is treated cyclically; a repeated closing vertex is fine.
pub(crate) fn point_in_ring(p: Coord<i32>, ring: &[Coord<i32>]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    // Drop a repeated closing vertex so the cyclic walk visits each edge once.
    let m = if ring[n - 1] == ring[0] { n - 1 } else { n };
    if m < 3 {
        return false;
    }
    let mut inside = false;
    let mut prev = m - 1;
    for i in 0..m {
        let tail = ring[prev];
        let head = ring[i];
        // On-boundary check: collinear and within the edge's bounding box → treat as inside.
        if orient(tail, head, p) == Ordering::Equal
            && p.x >= tail.x.min(head.x)
            && p.x <= tail.x.max(head.x)
            && p.y >= tail.y.min(head.y)
            && p.y <= tail.y.max(head.y)
        {
            return true;
        }
        // Does the edge straddle the horizontal ray at `p.y`? Half-open: exactly one endpoint is
        // strictly above `p.y`.
        if (tail.y > p.y) != (head.y > p.y) {
            // The crossing is to the right of `p` iff `p` is on the correct side of the directed edge.
            // For an upward edge a left turn (`Greater`) puts `p` left of it → the ray crosses to
            // `p`'s right; a downward edge flips the sense.
            let left = orient(tail, head, p) == Ordering::Greater;
            if left == (head.y > tail.y) {
                inside = !inside;
            }
        }
        prev = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use geo_types::Coord;

    use super::*;

    fn c(x: i32, y: i32) -> Coord<i32> {
        Coord { x, y }
    }

    #[test]
    fn orient_basic_and_extremes() {
        assert_eq!(orient(c(0, 0), c(1, 0), c(0, 1)), Ordering::Greater); // CCW / left
        assert_eq!(orient(c(0, 0), c(1, 0), c(0, -1)), Ordering::Less); // CW / right
        assert_eq!(orient(c(0, 0), c(2, 2), c(1, 1)), Ordering::Equal); // collinear
        // Full-`i32`-span vectors take the `i128` fallback and must stay exact.
        assert_eq!(
            orient(
                c(i32::MIN, i32::MIN),
                c(i32::MAX, i32::MIN),
                c(i32::MIN, i32::MAX)
            ),
            Ordering::Greater
        );
        assert_eq!(
            orient(
                c(i32::MIN, i32::MIN),
                c(i32::MAX, i32::MAX),
                c(i32::MIN + 1, i32::MIN + 1)
            ),
            Ordering::Equal
        );
    }

    #[test]
    fn ring_orientation_ccw_cw() {
        let ccw = [c(0, 0), c(4, 0), c(4, 4), c(0, 4), c(0, 0)];
        let cw = [c(0, 0), c(0, 4), c(4, 4), c(4, 0), c(0, 0)];
        assert_eq!(ring_orientation(&ccw), Ordering::Greater);
        assert_eq!(ring_orientation(&cw), Ordering::Less);
        assert_eq!(ring_orientation(&ccw[..2]), Ordering::Equal); // degenerate
    }

    #[test]
    fn point_in_ring_square() {
        let sq = [c(0, 0), c(10, 0), c(10, 10), c(0, 10), c(0, 0)];
        assert!(point_in_ring(c(5, 5), &sq)); // interior
        assert!(!point_in_ring(c(15, 5), &sq)); // exterior
        assert!(point_in_ring(c(0, 5), &sq)); // on the left edge → boundary counts as inside
        assert!(point_in_ring(c(10, 10), &sq)); // corner vertex
        assert!(!point_in_ring(c(-1, 5), &sq));
    }

    #[test]
    fn point_in_ring_concave() {
        // A C-shape (concave): the notch on the right is outside.
        let c_shape = [
            c(0, 0),
            c(10, 0),
            c(10, 3),
            c(3, 3),
            c(3, 7),
            c(10, 7),
            c(10, 10),
            c(0, 10),
            c(0, 0),
        ];
        assert!(point_in_ring(c(1, 5), &c_shape)); // in the spine
        assert!(!point_in_ring(c(6, 5), &c_shape)); // in the notch → outside
        assert!(point_in_ring(c(6, 1), &c_shape)); // in the bottom arm
    }
}
