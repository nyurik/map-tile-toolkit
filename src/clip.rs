//! Per-tile clipping: turn a Web Mercator geometry into one tile's integer geometry.
//!
//! This is the engine behind [`crate::slice_tile`]. Tiles are axis-aligned rectangles in Web
//! Mercator, so the geometry is clipped to the (buffered) tile rectangle in `O(vertices)`,
//! avoiding the cost of a general boolean overlay. Polygon rings are cut with Sutherland-Hodgman
//! (new vertices at the tile edge). Line strings are handled differently: rather than cutting new
//! vertices at the boundary, they keep their original vertices — every vertex inside the buffered
//! tile plus the first vertex just outside each time the line crosses the boundary (see
//! [`Rect::clip_line_string`]). The clipped geometry is then transformed to the integer tile grid
//! (flipping Y), any self-touch the snap introduced is repaired, rings are oriented for tile
//! winding, the result is validated, and finally cast to `i32`.

#![allow(
    clippy::float_cmp,
    clippy::collapsible_if,
    clippy::needless_range_loop,
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    reason = "axis-aligned clip math with intentional exact coordinate comparisons"
)]

use geo::orient::Direction;
use geo::{MapCoords as _, Orient as _, Simplify as _, Validation as _, unary_union};
use geo_types::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};

use crate::tile::{SliceOptions, TileId, tile_bbox, tile_length_from_zoom};

/// Tolerance for dropping duplicate/collinear points after snapping. Matches `martin`.
const SIMPLIFY_EPS: f64 = 1e-9;

/// Clip a Web Mercator (EPSG:3857) geometry to one tile plus its buffer, snap it to the
/// integer tile grid, repair, orient for tile winding, and validate.
///
/// Returns tile-local integer coordinates ready for tile encoding, or `None` when nothing
/// of the geometry falls inside the (buffered) tile.
pub(crate) fn clip_geometry_to_tile(
    geom: &Geometry<f64>,
    tile: TileId,
    opts: SliceOptions,
) -> Option<Geometry<i32>> {
    let mut rect = Rect::from_tile(tile, opts);
    rect.add_buffer();
    let tile_space = rect.clip_transform_validate_geometry(geom)?;
    Some(to_i32(&tile_space))
}

/// Cast an integer-valued tile-space geometry from `f64` to `i32`.
///
/// Callers snap/floor coordinates first, so every value is already a whole number well within
/// `i32` range; the cast is exact.
pub(crate) fn to_i32(geom: &Geometry<f64>) -> Geometry<i32> {
    geom.map_coords(|c| Coord {
        x: c.x as i32,
        y: c.y as i32,
    })
}

/// A single tile in Web Mercator space, carrying the tile resolution it is rendered at.
#[derive(Debug, Clone)]
struct Rect {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    opts: SliceOptions,
}

impl Rect {
    fn from_tile(tile: TileId, opts: SliceOptions) -> Self {
        let tile_length = tile_length_from_zoom(tile.z);
        let [min_x, min_y, max_x, max_y] = tile_bbox(tile.x, tile.y, tile_length);
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
            opts,
        }
    }

    /// Test if a point is inside the rectangle.
    fn inside(&self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    /// Grow the tile outward by the buffer fraction so geometry just outside the tile is
    /// still clipped into the buffer margin.
    fn add_buffer(&mut self) {
        let fraction = self.opts.buffer_fraction();
        let buffer_x = (self.max_x - self.min_x) * fraction;
        let buffer_y = (self.max_y - self.min_y) * fraction;
        self.min_x -= buffer_x;
        self.min_y -= buffer_y;
        self.max_x += buffer_x;
        self.max_y += buffer_y;
    }

    /// Clip line strings to the tile (keeping their original vertices), snap to the integer
    /// grid, and validate; `None` if nothing remains.
    fn clip_lines(&self, lines: &MultiLineString<f64>) -> Option<Geometry<f64>> {
        let clipped: Vec<LineString<f64>> = lines
            .0
            .iter()
            .flat_map(|ls| self.clip_line_string(ls))
            .collect();
        if clipped.is_empty() {
            return None;
        }
        let tile_space = MultiLineString(clipped).map_coords(|c| self.to_tile_coord(c));
        validate_and_simplify(tile_space.into())
    }

    /// Clip polygons to the tile with Sutherland-Hodgman, snap to the integer grid, orient
    /// for tile winding, and validate; `None` if nothing remains.
    fn clip_area(&self, polys: &[Polygon<f64>]) -> Option<Geometry<f64>> {
        let clipped: Vec<Polygon<f64>> =
            polys.iter().filter_map(|p| self.clip_polygon(p)).collect();
        if clipped.is_empty() {
            return None;
        }
        let snapped = MultiPolygon(clipped).map_coords(|c| self.to_tile_coord(c));
        finalize_area(snapped)
    }

    /// Clip one polygon (exterior plus holes) to the tile rectangle.
    fn clip_polygon(&self, poly: &Polygon<f64>) -> Option<Polygon<f64>> {
        let exterior = self.clip_ring(poly.exterior());
        // A ring needs at least 3 distinct vertices to enclose area.
        if exterior.len() < 3 {
            return None;
        }
        let holes: Vec<LineString<f64>> = poly
            .interiors()
            .iter()
            .filter_map(|hole| {
                let ring = self.clip_ring(hole);
                (ring.len() >= 3).then(|| close_ring(ring))
            })
            .collect();
        Some(Polygon::new(close_ring(exterior), holes))
    }

    /// Sutherland-Hodgman clip of a ring against the four tile edges; returns the clipped
    /// ring's distinct vertices (unclosed), empty if the ring falls entirely outside.
    fn clip_ring(&self, ring: &LineString<f64>) -> Vec<Coord<f64>> {
        let mut poly = distinct_ring(ring);
        for edge in Edge::ALL {
            if poly.is_empty() {
                break;
            }
            poly = self.clip_ring_edge(&poly, edge);
        }
        poly
    }

    /// One Sutherland-Hodgman pass against a single tile edge.
    fn clip_ring_edge(&self, input: &[Coord<f64>], edge: Edge) -> Vec<Coord<f64>> {
        let mut out = Vec::with_capacity(input.len() + 1);
        let n = input.len();
        for i in 0..n {
            let cur = input[i];
            let prev = input[(i + n - 1) % n];
            let cur_in = self.edge_inside(cur, edge);
            let prev_in = self.edge_inside(prev, edge);
            if cur_in {
                if !prev_in {
                    out.push(self.edge_intersect(prev, cur, edge));
                }
                out.push(cur);
            } else if prev_in {
                out.push(self.edge_intersect(prev, cur, edge));
            }
        }
        out
    }

    /// Split a line string into the pieces that touch the (buffered) tile, keeping the
    /// **original** vertices — no new vertices are cut at the boundary.
    ///
    /// A vertex is kept when it lies inside the buffered tile, or is an endpoint of a segment
    /// that crosses it. Consecutive kept vertices form one piece, so each piece's endpoints are
    /// the first vertices just outside the buffer where the line entered/left it. Fully-outside
    /// stretches between two visits are dropped, so a line that exits and later re-enters comes
    /// back as separate pieces (a `MultiLineString`), each keeping its first-outside vertex.
    fn clip_line_string(&self, ls: &LineString<f64>) -> Vec<LineString<f64>> {
        let pts = &ls.0;
        let n = pts.len();
        if n < 2 {
            return Vec::new();
        }

        // Keep both endpoints of every segment that touches the tile. Because a touching segment
        // marks both its endpoints, each kept vertex always has a kept neighbor, so runs are
        // never isolated single vertices.
        let mut keep = vec![false; n];
        for i in 0..n - 1 {
            if self.segment_intersects(pts[i], pts[i + 1]) {
                keep[i] = true;
                keep[i + 1] = true;
            }
        }

        let mut pieces: Vec<Vec<Coord<f64>>> = Vec::new();
        let mut cur: Vec<Coord<f64>> = Vec::new();
        for i in 0..n {
            if keep[i] {
                cur.push(pts[i]);
            } else if cur.len() >= 2 {
                pieces.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
        if cur.len() >= 2 {
            pieces.push(cur);
        }
        pieces.into_iter().map(LineString).collect()
    }

    /// Does segment `a`..`b` intersect the (buffered) tile rectangle? A Liang-Barsky parameter
    /// test with no interpolation — for lines we keep original vertices rather than cut new ones.
    fn segment_intersects(&self, a: Coord<f64>, b: Coord<f64>) -> bool {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let p = [-dx, dx, -dy, dy];
        let q = [
            a.x - self.min_x,
            self.max_x - a.x,
            a.y - self.min_y,
            self.max_y - a.y,
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

    fn edge_inside(&self, c: Coord<f64>, edge: Edge) -> bool {
        match edge {
            Edge::Left => c.x >= self.min_x,
            Edge::Right => c.x <= self.max_x,
            Edge::Bottom => c.y >= self.min_y,
            Edge::Top => c.y <= self.max_y,
        }
    }

    /// Intersection of segment `a`..`b` with the (axis-aligned) line of `edge`.
    fn edge_intersect(&self, a: Coord<f64>, b: Coord<f64>, edge: Edge) -> Coord<f64> {
        match edge {
            Edge::Left | Edge::Right => {
                let x = if matches!(edge, Edge::Left) {
                    self.min_x
                } else {
                    self.max_x
                };
                let t = (x - a.x) / (b.x - a.x);
                Coord {
                    x,
                    y: a.y + t * (b.y - a.y),
                }
            }
            Edge::Bottom | Edge::Top => {
                let y = if matches!(edge, Edge::Bottom) {
                    self.min_y
                } else {
                    self.max_y
                };
                let t = (y - a.y) / (b.y - a.y);
                Coord {
                    x: a.x + t * (b.x - a.x),
                    y,
                }
            }
        }
    }

    /// Transform a Web Mercator coordinate into the integer-snapped tile grid.
    fn to_tile_coord(&self, c: Coord<f64>) -> Coord<f64> {
        let extent = f64::from(self.opts.extent.get());
        let buffer = self.opts.buffer_fraction();

        // Recover the unbuffered tile bounds from the buffered rectangle, then scale into
        // 0..extent, flipping y so it points down as tile requires.
        let max_x = ((1.0 + buffer) * self.max_x + buffer * self.min_x) / (1.0 + 2.0 * buffer);
        let min_x = (self.min_x + max_x * buffer) / (1.0 + buffer);
        let max_y = ((1.0 + buffer) * self.max_y + buffer * self.min_y) / (1.0 + 2.0 * buffer);
        let min_y = (self.min_y + max_y * buffer) / (1.0 + buffer);

        let x_multiplier = extent / (max_x - min_x);
        let y_multiplier = extent / (max_y - min_y);

        Coord {
            x: ((c.x - min_x) * x_multiplier).floor(),
            y: extent - ((c.y - min_y) * y_multiplier).floor(),
        }
    }

    /// Clip a Web Mercator geometry to this (buffered) tile and snap it to the integer tile
    /// grid; `None` when nothing of the geometry remains inside the tile.
    fn clip_transform_validate_geometry(&self, geom: &Geometry<f64>) -> Option<Geometry<f64>> {
        match geom {
            Geometry::Point(p) => self
                .inside(p.x(), p.y())
                .then(|| Geometry::Point(self.to_tile_coord(p.0).into())),
            Geometry::MultiPoint(ps) => {
                let kept: Vec<Point<f64>> = ps
                    .iter()
                    .filter(|p| self.inside(p.x(), p.y()))
                    .map(|p| self.to_tile_coord(p.0).into())
                    .collect();
                (!kept.is_empty()).then_some(Geometry::MultiPoint(MultiPoint(kept)))
            }
            Geometry::LineString(ls) => self.clip_lines(&MultiLineString(vec![ls.clone()])),
            Geometry::MultiLineString(mls) => self.clip_lines(mls),
            Geometry::Polygon(polygon) => self.clip_area(std::slice::from_ref(polygon)),
            Geometry::MultiPolygon(polygons) => self.clip_area(&polygons.0),
            Geometry::GeometryCollection(gs) => {
                let kept: Vec<Geometry<f64>> = gs
                    .iter()
                    .filter_map(|g| self.clip_transform_validate_geometry(g))
                    .collect();
                (!kept.is_empty()).then_some(Geometry::GeometryCollection(GeometryCollection(kept)))
            }
            // GeoJSON never parses into these geometry variants.
            Geometry::Line(_) | Geometry::Rect(_) | Geometry::Triangle(_) => None,
        }
    }
}

/// The four edges of the clip rectangle, as Sutherland-Hodgman half-planes.
#[derive(Clone, Copy)]
enum Edge {
    Left,
    Right,
    Bottom,
    Top,
}

impl Edge {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Bottom, Self::Top];
}

/// A ring's distinct vertices (dropping the closing duplicate, if present).
fn distinct_ring(ring: &LineString<f64>) -> Vec<Coord<f64>> {
    let pts = &ring.0;
    match (pts.first(), pts.last()) {
        (Some(f), Some(l)) if pts.len() > 1 && f == l => pts[..pts.len() - 1].to_vec(),
        _ => pts.clone(),
    }
}

/// Close a ring by repeating its first vertex at the end.
fn close_ring(mut pts: Vec<Coord<f64>>) -> LineString<f64> {
    if let Some(&first) = pts.first() {
        if pts.last() != Some(&first) {
            pts.push(first);
        }
    }
    LineString(pts)
}

/// Drop duplicate/collinear points within [`SIMPLIFY_EPS`]; points/multipoints are unchanged.
fn simplify_geo(geom: Geometry<f64>) -> Geometry<f64> {
    match geom {
        point @ Geometry::Point(_) => point,
        points @ Geometry::MultiPoint(_) => points,
        Geometry::LineString(ls) => Geometry::LineString(ls.simplify(SIMPLIFY_EPS)),
        Geometry::MultiLineString(mls) => Geometry::MultiLineString(mls.simplify(SIMPLIFY_EPS)),
        Geometry::Polygon(p) => Geometry::Polygon(p.simplify(SIMPLIFY_EPS)),
        Geometry::MultiPolygon(mp) => Geometry::MultiPolygon(mp.simplify(SIMPLIFY_EPS)),
        rest => rest,
    }
}

/// Validate a tile-space geometry and drop duplicate points; geometry the integer snap
/// pinched into an invalid shape yields `None`.
pub(crate) fn validate_and_simplify(geom: Geometry<f64>) -> Option<Geometry<f64>> {
    geom.is_valid().then(|| simplify_geo(geom))
}

/// Repair, orient, and validate an already-snapped tile-space polygonal geometry.
///
/// The integer snap can pinch a polygon into a self-touch; re-resolve it through the overlay
/// engine so the topology is repaired rather than dropped. The snap also flips Y (reversing
/// ring orientation), so re-orient exterior rings counter-clockwise for tile winding. Shared
/// by [`Rect::clip_area`] and the stripe-based batch reassembly. `None` if nothing survives.
pub(crate) fn finalize_area(snapped: MultiPolygon<f64>) -> Option<Geometry<f64>> {
    let resolved = if snapped.is_valid() {
        snapped
    } else {
        unary_union([&snapped])
    };
    if resolved.0.is_empty() {
        return None;
    }
    validate_and_simplify(resolved.orient(Direction::Default).into())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use approx::assert_relative_eq;

    use super::*;

    #[test]
    fn transform_matches_martin_reference() {
        // Ported verbatim from martin's rect.rs unit test: this locks the tile transform.
        let opts = SliceOptions::new(NonZeroU32::new(4096).expect("nonzero"), 256);
        let mut rect = Rect::from_tile(TileId::new(70, 43, 7), opts);
        rect.add_buffer();
        let c = rect.to_tile_coord(Coord {
            x: 1_962_772.0,
            y: 6_300_000.0,
        });
        assert_relative_eq!(c.x, 1102.0);
        assert_relative_eq!(c.y, 3596.0);
    }
}
