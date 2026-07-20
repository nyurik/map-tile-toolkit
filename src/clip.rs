//! The clip seam: turn a Web Mercator geometry into one tile integer geometry.
//!
//! This is the "A0" engine — it reuses `geo`'s overlay-based clipping ([`BooleanOps`])
//! for the geometrically hard work, and only adds the tile-grid transform, integer snap,
//! and topology repair around it. It is deliberately isolated behind
//! [`clip_geometry_to_tile`] so a faster clipper (a dedicated rectangle clip, or an eager
//! stripe slicer) can replace it later without touching the public [`crate::slice`] API.
//!
//! The pipeline mirrors `martin`'s proven `martin-core` `GeoJSON` tile path: clip in
//! Web Mercator `f64`, snap to the integer grid (flipping Y), repair self-touches the
//! snap may introduce, orient rings for tile winding, validate, and finally cast to `i32`.

use geo::bool_ops::FillRule;
use geo::orient::Direction;
use geo::{
    BooleanOps as _, MapCoords as _, Orient as _, Simplify as _, Validation as _, unary_union,
};
use geo_types::{Coord, Geometry, GeometryCollection, MultiLineString, MultiPoint, Point, Polygon};

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
/// The transform in [`Rect::transform_to_tile_coordinates`] floors coordinates, so every
/// value is already a whole number well within `i32` range; the cast is exact.
#[expect(clippy::cast_possible_truncation)]
fn to_i32(geom: &Geometry<f64>) -> Geometry<i32> {
    geom.map_coords(|c| Coord {
        x: c.x as i32,
        y: c.y as i32,
    })
}

/// A single tile in Web Mercator space, carrying the tile resolution it is rendered at.
///
/// Extracted from `martin`'s `martin-core/src/tiles/geojson/rect.rs`.
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

    /// The (buffered) tile rectangle as a clip polygon in Web Mercator coordinates.
    fn clip_polygon(&self) -> Polygon<f64> {
        geo_types::Rect::new(
            Coord {
                x: self.min_x,
                y: self.min_y,
            },
            Coord {
                x: self.max_x,
                y: self.max_y,
            },
        )
        .to_polygon()
    }

    /// Clip a 1-D geometry to the tile, snap to the integer grid, and validate; `None` if
    /// nothing remains.
    fn clip_lines(&self, lines: &MultiLineString<f64>) -> Option<Geometry<f64>> {
        let clipped = self.clip_polygon().clip(lines, false);
        if clipped.0.is_empty() {
            return None;
        }
        let tile_space = clipped.map_coords(|c| self.to_tile_coord(c));
        validate_and_simplify(tile_space.into())
    }

    /// Intersect a 2-D geometry with the tile, snap to the integer grid, orient for tile,
    /// and validate; `None` if nothing remains.
    fn clip_area(&self, area: &impl geo::BooleanOps<Scalar = f64>) -> Option<Geometry<f64>> {
        let clipped = self
            .clip_polygon()
            .intersection_with_fill_rule(area, FillRule::EvenOdd);
        if clipped.0.is_empty() {
            return None;
        }
        let snapped = clipped.map_coords(|c| self.to_tile_coord(c));
        // The integer snap can pinch a polygon into a self-touch; re-resolve it through the
        // overlay engine so the topology is repaired rather than failing validation and
        // dropping the feature.
        let resolved = if snapped.is_valid() {
            snapped
        } else {
            unary_union([&snapped])
        };
        if resolved.0.is_empty() {
            // The snap collapsed the polygon below tile resolution; drop it rather than emit
            // an empty geometry.
            return None;
        }
        // The snap flips y, reversing ring orientation; re-orient so exterior rings are
        // counter-clockwise (tile required winding once y points down in tile space).
        let tile_space = resolved.orient(Direction::Default);
        validate_and_simplify(tile_space.into())
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
            Geometry::Polygon(polygon) => self.clip_area(polygon),
            Geometry::MultiPolygon(polygons) => self.clip_area(polygons),
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
fn validate_and_simplify(geom: Geometry<f64>) -> Option<Geometry<f64>> {
    geom.is_valid().then(|| simplify_geo(geom))
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
