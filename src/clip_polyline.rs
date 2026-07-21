//! Low-level polyline clipping against a single tile box (the engine behind [`crate::Slicer`]).
//!
//! Clipping keeps the **original** vertices — never cutting new ones at the tile edge. Every
//! segment that touches the box contributes both of its endpoints, so a line shows up in every tile
//! it passes through, even ones it merely crosses with no vertex inside. A stretch that leaves the
//! box and re-enters comes back as separate pieces.

use geo_types::{Coord, Geometry, LineString, MultiLineString};

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
