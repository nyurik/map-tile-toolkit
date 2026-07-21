//! One tile's worth of an integer polyline.
//!
//! [`slice_tile`] clips a `LineString`/`MultiLineString` (integer coordinates) to a single tile of
//! the integer grid, keeping the **original** vertices — every vertex inside the tile, plus the
//! single vertex just outside each time the line enters or leaves it. A line that leaves and later
//! re-enters comes back as separate pieces. Nothing is cut at the tile edge, so a segment that
//! crosses a tile without a vertex inside it is dropped.
//!
//! This is the per-tile reference path; [`crate::slice_all_tiles`] slices a whole geometry into
//! every tile it touches and must agree with calling [`slice_tile`] on each of those tiles.

use geo_types::{Coord, Geometry, LineString, MultiLineString};

use crate::tile::{TileId, tile_bounds};

/// Clip an integer polyline geometry to a single tile, keeping original vertices.
///
/// Input is a [`Geometry::LineString`] or [`Geometry::MultiLineString`]; the result is the same
/// kind (a single run stays a `LineString`, several become a `MultiLineString`), or `None` when
/// nothing of the geometry falls in the tile.
#[must_use]
pub fn slice_tile(geom: &Geometry<i32>, tile: TileId, tile_size: i32) -> Option<Geometry<i32>> {
    let (min, max) = tile_bounds(tile, tile_size);
    let mut pieces = Vec::new();
    for line in each_line(geom) {
        clip_line(line, min, max, &mut pieces);
    }
    assemble(pieces)
}

/// The component lines of a polyline geometry.
pub(crate) fn each_line(geom: &Geometry<i32>) -> Vec<&LineString<i32>> {
    match geom {
        Geometry::LineString(ls) => vec![ls],
        Geometry::MultiLineString(mls) => mls.0.iter().collect(),
        other => panic!("expected a polyline geometry, got {other:?}"),
    }
}

/// Clip one line to the closed integer rectangle `[min, max]`, appending each kept run to `out`.
///
/// Consecutive duplicate vertices are dropped. Walking the vertices, a run is collected whenever a
/// vertex lies inside the box — extended backward to include the vertex the line entered from and
/// forward through the first vertex that leaves the box (both kept). A lone outside vertex whose
/// next neighbor is inside again is a graze and keeps the run going; two outside vertices in a row
/// close it. Each run of ≥2 vertices becomes one output line.
pub(crate) fn clip_line(
    line: &LineString<i32>,
    min: Coord<i32>,
    max: Coord<i32>,
    out: &mut Vec<LineString<i32>>,
) {
    let inside = |c: Coord<i32>| c.x >= min.x && c.x <= max.x && c.y >= min.y && c.y <= max.y;

    // Pass through only vertices that differ from the previous one, dropping consecutive dups.
    let mut last: Option<Coord<i32>> = None;
    let mut iter = line
        .0
        .iter()
        .copied()
        .filter(move |&c| last.replace(c) != Some(c))
        .peekable();

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
            out.push(LineString(cur));
        }
    }
}

/// Wrap kept runs as a single geometry: `None`, one `LineString`, or a `MultiLineString`.
pub(crate) fn assemble(mut pieces: Vec<LineString<i32>>) -> Option<Geometry<i32>> {
    match pieces.len() {
        0 => None,
        1 => pieces.pop().map(Geometry::LineString),
        _ => Some(Geometry::MultiLineString(MultiLineString(pieces))),
    }
}
