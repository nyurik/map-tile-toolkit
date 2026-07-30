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
}
