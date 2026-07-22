//! Low-level polyline clipping against a single tile box (the engine behind [`crate::Slicer`]).
//!
//! Clipping keeps the **original** vertices — never cutting new ones at the tile edge. Every
//! segment that touches the box contributes both of its endpoints, so a line shows up in every tile
//! it passes through, even ones it merely crosses with no vertex inside. A stretch that leaves the
//! box and re-enters comes back as separate pieces.

use geo_types::{Coord, Geometry, LineString, MultiLineString};

use crate::Error;
use crate::vertex::Vertex;

/// The component lines of a polyline geometry, borrowed (no allocation). Errors for any other
/// geometry kind rather than panicking.
pub(crate) fn each_line(geom: &Geometry<i32>) -> Result<&[LineString<i32>], Error> {
    match geom {
        Geometry::LineString(ls) => Ok(std::slice::from_ref(ls)),
        Geometry::MultiLineString(mls) => Ok(&mls.0),
        other => Err(Error::UnsupportedGeometry(other.inner_type_name())),
    }
}

/// The name of a geometry variant, for error messages.
fn geometry_kind(geom: &Geometry<i32>) -> &'static str {
    match geom {
        Geometry::Point(_) => "Point",
        Geometry::Line(_) => "Line",
        Geometry::LineString(_) => "LineString",
        Geometry::Polygon(_) => "Polygon",
        Geometry::MultiPoint(_) => "MultiPoint",
        Geometry::MultiLineString(_) => "MultiLineString",
        Geometry::MultiPolygon(_) => "MultiPolygon",
        Geometry::GeometryCollection(_) => "GeometryCollection",
        Geometry::Rect(_) => "Rect",
        Geometry::Triangle(_) => "Triangle",
    }
}

/// Is coordinate `c` inside the closed rectangle `[min, max]`?
fn inside(c: Coord<i32>, min: Coord<i32>, max: Coord<i32>) -> bool {
    c.x >= min.x && c.x <= max.x && c.y >= min.y && c.y <= max.y
}

/// Does segment `a`–`b` touch the closed integer rectangle `[min, max]`?
///
/// Integer-only (no division, no floats): reject when the segment's bounding box is disjoint from
/// the box, accept when an endpoint is inside, otherwise test whether the box straddles the
/// segment's supporting line via i128 cross products (so full `i32` coordinates cannot overflow).
#[allow(
    clippy::many_single_char_names,
    reason = "conventional short names for a geometric predicate"
)]
pub(crate) fn segment_intersects(
    a: Coord<i32>,
    b: Coord<i32>,
    min: Coord<i32>,
    max: Coord<i32>,
) -> bool {
    // Quick reject: the segment's bounding box is disjoint from the tile box.
    if a.x.min(b.x) > max.x || a.x.max(b.x) < min.x || a.y.min(b.y) > max.y || a.y.max(b.y) < min.y
    {
        return false;
    }
    // Quick accept: an endpoint lies inside the (closed) box.
    if inside(a, min, max) || inside(b, min, max) {
        return true;
    }
    // Both endpoints outside and the bounding boxes overlap: the segment meets the box iff its
    // corners are not all strictly on one side of the segment's supporting line.
    let (dx, dy) = (
        i128::from(b.x) - i128::from(a.x),
        i128::from(b.y) - i128::from(a.y),
    );
    let side = |x: i32, y: i32| {
        dx * (i128::from(y) - i128::from(a.y)) - dy * (i128::from(x) - i128::from(a.x))
    };
    let s = [
        side(min.x, min.y),
        side(max.x, min.y),
        side(min.x, max.y),
        side(max.x, max.y),
    ];
    !(s.iter().all(|&v| v > 0) || s.iter().all(|&v| v < 0))
}

/// Clip one line (a slice of [`Vertex`]) to the closed integer rectangle `[min, max]`, appending
/// each kept run to `out` in the tile-local frame whose `[0, 0]` corner is `origin` (each kept
/// vertex is stored with its position offset by `origin`, its payload untouched, so no separate
/// localization pass is needed).
///
/// Streams the vertices with no scratch allocation: consecutive duplicates (same position) are
/// skipped inline, and a run grows across **consecutive segments that touch the box** — both
/// endpoints of every segment in the run. A segment that misses the box ends the current run, so a
/// segment crossing the box with no vertex inside is still kept, while a stretch that leaves and
/// re-enters comes back as separate pieces. Each run of ≥2 vertices is moved out as one output run.
///
/// The box test uses the original (global) positions; only the stored vertices are localized.
///
/// # Errors
///
/// [`Error::Overflow`] if a kept vertex lies more than an `i32` span from `origin` (its local
/// position would not fit `i32`).
pub(crate) fn clip_line<V: Vertex>(
    line: &[V],
    min: Coord<i32>,
    max: Coord<i32>,
    origin: Coord<i32>,
    out: &mut Vec<Vec<V>>,
) -> Result<(), Error> {
    let mut prev: Option<V> = None;
    let mut cur: Vec<V> = Vec::new();
    for &c in line {
        if prev.map(|v| v.position()) == Some(c.position()) {
            continue; // drop a consecutive duplicate vertex (same position)
        }
        if let Some(a) = prev {
            if segment_intersects(a.position(), c.position(), min, max) {
                if cur.is_empty() {
                    cur.push(to_local(a, origin)?);
                }
                cur.push(to_local(c, origin)?);
            } else if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
        prev = Some(c);
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    Ok(())
}

/// Re-express vertex `v` in the tile-local frame whose `[0, 0]` corner is `origin`: its position
/// becomes `position − origin`, its payload unchanged. [`Error::Overflow`] if the offset leaves the
/// `i32` range — possible only when a far crossing-segment endpoint lies more than a full `i32` span
/// from the tile.
pub(crate) fn to_local<V: Vertex>(v: V, origin: Coord<i32>) -> Result<V, Error> {
    let p = v.position();
    Ok(v.with_position(Coord {
        x: p.x.checked_sub(origin.x).ok_or(Error::Overflow)?,
        y: p.y.checked_sub(origin.y).ok_or(Error::Overflow)?,
    }))
}

/// Wrap kept runs of `Coord` as a single geometry: `None`, one `LineString`, or a
/// `MultiLineString`. Used by the `geo-types` convenience API to turn generic runs back into a
/// `Geometry` (the generic API returns the runs directly).
pub(crate) fn assemble(mut runs: Vec<Vec<Coord<i32>>>) -> Option<Geometry<i32>> {
    match runs.len() {
        0 => None,
        1 => runs.pop().map(|r| Geometry::LineString(LineString(r))),
        _ => Some(Geometry::MultiLineString(MultiLineString(
            runs.into_iter().map(LineString).collect(),
        ))),
    }
}
