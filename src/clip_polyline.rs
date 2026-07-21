//! Integer polyline clipping against a tile bounding box.
//!
//! Unlike [`crate::clip`], which clips arbitrary Web Mercator geometry and cuts new vertices at
//! the tile edge, this path works directly on an already-integer [`LineString<i32>`] and keeps
//! only its **original** vertices. Given a tile bounding box it walks the polyline once and emits
//! the runs that touch the box: every vertex inside the box, plus the single vertex just outside
//! each time the line enters or leaves it. A line that leaves and later re-enters comes back as
//! separate pieces, so the result is a [`Geometry::LineString`] (one piece) or
//! [`Geometry::MultiLineString`] (several), or `None` when nothing touches the box.

use geo_types::{Coord, Geometry, LineString, MultiLineString, Rect};

/// Clip an integer polyline to a tile bounding box in a single pass, keeping original vertices.
///
/// Vertices identical to their predecessor are dropped. Walking the (deduplicated) vertices in
/// order, a run is collected whenever a vertex lies inside `bbox` — extended backward to include
/// the vertex the line entered from and forward through the first vertex that leaves the box
/// (both kept). The walk continues past a run, so a polyline that re-enters `bbox` yields further
/// pieces.
///
/// Returns [`Geometry::LineString`] for a single run, [`Geometry::MultiLineString`] for several,
/// or `None` when the polyline never touches the box (each run has at least two vertices).
#[must_use]
pub fn slice_tile(line: &LineString<i32>, bbox: Rect<i32>) -> Option<Geometry<i32>> {
    let (min, max) = (bbox.min(), bbox.max());
    // `impl Contains<Coord> for Rect` uses non-inclusive comparisons, so test the closed box here.
    let inside = |c: Coord<i32>| c.x >= min.x && c.x <= max.x && c.y >= min.y && c.y <= max.y;

    // Pass through only vertices that differ from the previous one, dropping consecutive dups.
    let mut last: Option<Coord<i32>> = None;
    let mut iter = line
        .coords()
        .copied()
        .filter(move |&c| last.replace(c) != Some(c))
        .peekable();

    let mut pieces: Vec<LineString<i32>> = Vec::new();
    let mut prev: Option<Coord<i32>> = None; // last vertex seen outside the box

    while let Some(c) = iter.next() {
        if !inside(c) {
            prev = Some(c); // outside: remember it as the possible entry vertex
            continue;
        }
        // Open a run at the first inside vertex, prepending the vertex we entered from.
        let mut cur: Vec<Coord<i32>> = Vec::new();
        cur.extend(prev.take());
        cur.push(c);
        // Keep copying: inside vertices always; an outside vertex is kept too, but closes the
        // run unless the next vertex is inside again (the line only grazed out — carry on).
        while let Some(n) = iter.next() {
            cur.push(n);
            if !inside(n) && iter.peek().is_none_or(|&p| !inside(p)) {
                break;
            }
        }
        if cur.len() >= 2 {
            pieces.push(LineString(cur));
        }
    }

    match pieces.len() {
        0 => None,
        1 => pieces.pop().map(Geometry::LineString),
        _ => Some(Geometry::MultiLineString(MultiLineString(pieces))),
    }
}
