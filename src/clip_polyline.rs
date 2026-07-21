//! One tile's worth of an integer polyline.
//!
//! [`slice_tile`] clips a `LineString`/`MultiLineString` (integer coordinates) to a single tile of
//! the integer grid, keeping the **original** vertices — never cutting new ones at the tile edge.
//! Every segment that touches the tile contributes both of its endpoints, so the line shows up in
//! **every** tile it passes through, even ones it merely crosses with no vertex inside. A line that
//! leaves the tile and later re-enters it comes back as separate pieces.
//!
//! This is the per-tile reference path; [`crate::slice_all_tiles`] slices a whole geometry into
//! every tile it touches and must agree with calling [`slice_tile`] on each of those tiles.

use geo_types::{Coord, Geometry, LineString, MultiLineString};

use crate::tile::{TileId, tile_bounds};

/// Clip an integer polyline geometry to a single tile, keeping original vertices.
///
/// Input is a [`Geometry::LineString`] or [`Geometry::MultiLineString`]; the result is the same
/// kind (a single run stays a `LineString`, several become a `MultiLineString`), or `None` when
/// nothing of the geometry touches the tile.
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

/// Does segment `a`–`b` touch the closed integer rectangle `[min, max]`? A Liang–Barsky parameter
/// test with no interpolation — for lines we keep original vertices rather than cut new ones.
#[allow(
    clippy::many_single_char_names,
    clippy::float_cmp,
    reason = "Liang–Barsky clip: conventional single-letter names and intentional exact comparisons"
)]
pub(crate) fn segment_intersects(
    a: Coord<i32>,
    b: Coord<i32>,
    min: Coord<i32>,
    max: Coord<i32>,
) -> bool {
    let (ax, ay) = (f64::from(a.x), f64::from(a.y));
    let (dx, dy) = (f64::from(b.x) - ax, f64::from(b.y) - ay);
    let p = [-dx, dx, -dy, dy];
    let q = [
        ax - f64::from(min.x),
        f64::from(max.x) - ax,
        ay - f64::from(min.y),
        f64::from(max.y) - ay,
    ];
    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;
    for i in 0..4 {
        if p[i] == 0.0 {
            if q[i] < 0.0 {
                return false; // parallel to this edge and outside it
            }
        } else {
            let t = q[i] / p[i];
            if p[i] < 0.0 {
                if t > t1 {
                    return false;
                }
                if t > t0 {
                    t0 = t;
                }
            } else {
                if t < t0 {
                    return false;
                }
                if t < t1 {
                    t1 = t;
                }
            }
        }
    }
    true
}

/// Clip one line to the closed integer rectangle `[min, max]`, appending each kept run to `out`.
///
/// Consecutive duplicate vertices are dropped. A run is a maximal sequence of **consecutive
/// segments that touch the box**; its vertices (both endpoints of every segment in the run) become
/// one output line. A segment that misses the box ends the current run, so a segment crossing the
/// box with no vertex inside is still kept, while a stretch that leaves the box and re-enters comes
/// back as separate pieces — the intervening outside segments are not drawn in this tile.
pub(crate) fn clip_line(
    line: &LineString<i32>,
    min: Coord<i32>,
    max: Coord<i32>,
    out: &mut Vec<LineString<i32>>,
) {
    // Drop consecutive duplicate vertices.
    let mut pts: Vec<Coord<i32>> = Vec::with_capacity(line.0.len());
    for &c in &line.0 {
        if pts.last() != Some(&c) {
            pts.push(c);
        }
    }
    if pts.len() < 2 {
        return;
    }

    // Grow a run across consecutive segments that touch the box; a missing segment flushes it.
    let mut cur: Vec<Coord<i32>> = Vec::new();
    for w in pts.windows(2) {
        if segment_intersects(w[0], w[1], min, max) {
            if cur.is_empty() {
                cur.push(w[0]);
            }
            cur.push(w[1]);
        } else if cur.len() >= 2 {
            out.push(LineString(std::mem::take(&mut cur)));
        } else {
            cur.clear();
        }
    }
    if cur.len() >= 2 {
        out.push(LineString(cur));
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
