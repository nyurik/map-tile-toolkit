//! Geometry helpers used by the clipping pipeline, ported from planetiler's `GeoUtils`.
//!
//! World coordinates put the whole world in the unit square: X `0..1` from -180°..180°,
//! Y `0..1` from north..south (Web Mercator), `0.5` at the equator/prime meridian.

use std::f64::consts::PI;

use geo::orient::Direction;
use geo::{MapCoords as _, Orient, Validation, unary_union};
use geo_types::{Coord, Geometry, LineString, MultiLineString, MultiPolygon, coord};

use crate::stripe::GeometryError;

const RADIANS_PER_DEGREE: f64 = PI / 180.0;
const DEGREES_PER_RADIAN: f64 = 180.0 / PI;
/// Planetiler `PlanetilerConfig.MAX_MAXZOOM`.
const MAX_MAXZOOM: u8 = 15;
/// Planetiler `TILE_PRECISION` grid size: `4096 / 256`.
const TILE_PRECISION_GRID: f64 = 4096.0 / 256.0;

/// World X (`0..1`) for a longitude in degrees.
#[must_use]
pub fn get_world_x(longitude: f64) -> f64 {
    (longitude + 180.0) / 360.0
}

/// World Y (`0..1`) for a latitude in degrees (Web Mercator).
#[must_use]
pub fn get_world_y(latitude: f64) -> f64 {
    // Clamp beyond the map edges exactly as planetiler does.
    if latitude <= get_world_lat(1.1) {
        return 1.1;
    }
    if latitude >= get_world_lat(-0.1) {
        return -0.1;
    }
    let sin = (latitude * RADIANS_PER_DEGREE).sin();
    0.5 - 0.25 * ((1.0 + sin) / (1.0 - sin)).ln() / PI
}

/// Longitude in degrees for a world X coordinate.
#[must_use]
pub fn get_world_lon(x: f64) -> f64 {
    x * 360.0 - 180.0
}

/// Latitude in degrees for a world Y coordinate.
#[must_use]
pub fn get_world_lat(y: f64) -> f64 {
    let n = PI - 2.0 * PI * y;
    DEGREES_PER_RADIAN * (0.5 * (n.exp() - (-n).exp())).atan()
}

/// Convert a world coordinate to `(longitude, latitude)` degrees.
#[must_use]
pub fn world_to_lat_lon(world: Coord<f64>) -> Coord<f64> {
    coord! { x: get_world_lon(world.x), y: get_world_lat(world.y) }
}

/// Convert `(longitude, latitude)` degrees to a world coordinate.
#[must_use]
pub fn lat_lon_to_world(lon_lat: Coord<f64>) -> Coord<f64> {
    coord! { x: get_world_x(lon_lat.x), y: get_world_y(lon_lat.y) }
}

/// Convert a polygon / multipolygon / ring into its boundary as a (multi)linestring.
///
/// # Errors
/// Returns [`GeometryError::BadPolygonFill`] if `geom` has no rings/lines to extract.
pub fn polygon_to_linestring(geom: &Geometry<f64>) -> Result<Geometry<f64>, GeometryError> {
    let mut lines = Vec::new();
    collect_line_strings(geom, &mut lines);
    match lines.len() {
        0 => Err(GeometryError::BadPolygonFill("no line strings".into())),
        1 => {
            Ok(Geometry::LineString(lines.into_iter().next().ok_or_else(
                || GeometryError::BadPolygonFill("empty".into()),
            )?))
        }
        _ => Ok(Geometry::MultiLineString(MultiLineString(lines))),
    }
}

fn collect_line_strings(geom: &Geometry<f64>, out: &mut Vec<LineString<f64>>) {
    match geom {
        Geometry::LineString(ls) => out.push(ls.clone()),
        Geometry::Polygon(p) => {
            out.push(p.exterior().clone());
            out.extend(p.interiors().iter().cloned());
        }
        Geometry::MultiPolygon(mp) => {
            for p in &mp.0 {
                out.push(p.exterior().clone());
                out.extend(p.interiors().iter().cloned());
            }
        }
        Geometry::GeometryCollection(gc) => {
            for g in &gc.0 {
                collect_line_strings(g, out);
            }
        }
        _ => {}
    }
}

/// Whether a ring is convex, tolerating repeated points and tiny concavities relative to the
/// overall shape (planetiler `isConvex`).
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "exact coordinate comparison, faithful to planetiler"
)]
pub fn is_convex(ring: &LineString<f64>) -> bool {
    const THRESHOLD: f64 = 1e-3;
    const MIN_POINTS_TO_CHECK: usize = 10;
    let seq = &ring.0;
    let size = seq.len();
    if size <= 3 {
        return false;
    }

    // Skip leading repeated points.
    let (c0x, c0y) = (seq[0].x, seq[0].y);
    let (mut c1x, mut c1y) = (f64::NAN, f64::NAN);
    let mut i: usize = 1;
    while i < size {
        c1x = seq[i].x;
        c1y = seq[i].y;
        if c1x != c0x || c1y != c0y {
            break;
        }
        i += 1;
    }

    let mut dx1 = c1x - c0x;
    let mut dy1 = c1y - c0y;
    let mut neg_z = 1e-20;
    let mut pos_z = 1e-20;

    // Wrap around so the triangle formed by the last and first points is also checked.
    while i <= size + 1 {
        let idx = if i < size { i } else { i + 1 - size };
        let c2x = seq[idx].x;
        let c2y = seq[idx].y;
        let dx2 = c2x - c1x;
        let dy2 = c2y - c1y;
        let z = dx1 * dy2 - dy1 * dx2;
        let abs_z = z.abs();

        let mut extended = false;
        if z < 0.0 && abs_z > neg_z {
            neg_z = abs_z;
            extended = true;
        } else if z > 0.0 && abs_z > pos_z {
            pos_z = abs_z;
            extended = true;
        }

        if i == MIN_POINTS_TO_CHECK || (i > MIN_POINTS_TO_CHECK && extended) {
            let ratio = if neg_z < pos_z {
                neg_z / pos_z
            } else {
                pos_z / neg_z
            };
            if ratio > THRESHOLD {
                return false;
            }
        }

        c1x = c2x;
        c1y = c2y;
        dx1 = dx2;
        dy1 = dy2;
        i += 1;
    }
    (if neg_z < pos_z {
        neg_z / pos_z
    } else {
        pos_z / neg_z
    }) < THRESHOLD
}

/// Snap a polygon to the tile precision grid and repair self-intersections the snap may
/// introduce, returning a valid, CW-wound polygonal geometry.
///
/// Note: planetiler uses JTS `buffer(0)` + `GeometryPrecisionReducer`; this uses `geo`'s
/// overlay engine (`unary_union`) for the repair, which is topologically equivalent but not
/// bit-identical to JTS.
///
/// # Errors
/// Returns [`GeometryError::BadPolygonFill`] if the input is not polygonal.
pub fn snap_and_fix_polygon(geom: &Geometry<f64>) -> Result<Geometry<f64>, GeometryError> {
    let snapped = geom.map_coords(|c| Coord {
        x: (c.x * TILE_PRECISION_GRID).round() / TILE_PRECISION_GRID,
        y: (c.y * TILE_PRECISION_GRID).round() / TILE_PRECISION_GRID,
    });
    let multi: MultiPolygon<f64> = match snapped {
        Geometry::Polygon(p) => MultiPolygon(vec![p]),
        Geometry::MultiPolygon(mp) => mp,
        _ => return Err(GeometryError::BadPolygonFill("not polygonal".into())),
    };
    let resolved = if multi.is_valid() {
        multi
    } else {
        unary_union([&multi])
    };
    // planetiler's output winds exterior rings clockwise (negative signed area).
    let resolved = resolved.orient(Direction::Reversed);
    if resolved.0.len() == 1 {
        match resolved.0.into_iter().next() {
            Some(p) => Ok(Geometry::Polygon(p)),
            None => Err(GeometryError::BadPolygonFill("empty after repair".into())),
        }
    } else {
        Ok(Geometry::MultiPolygon(resolved))
    }
}

/// Lowest zoom at which a feature of `world_geometry_size` (world units, `1` = whole planet)
/// spans at least `min_pixel_size` pixels, clamped to `[0, MAX_MAXZOOM]` (planetiler
/// `minZoomForPixelSize`).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "zoom is range-checked to 0..=MAX_MAXZOOM before the cast"
)]
pub fn min_zoom_for_pixel_size(world_geometry_size: f64, min_pixel_size: f64) -> u8 {
    let world_pixels = world_geometry_size * 256.0;
    let zoom = ((min_pixel_size / world_pixels).ln() / 2.0_f64.ln()).ceil();
    if zoom.is_nan() || zoom < 0.0 {
        0
    } else if zoom > f64::from(MAX_MAXZOOM) {
        MAX_MAXZOOM
    } else {
        zoom as u8
    }
}
