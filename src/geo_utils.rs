//! Geometry helpers used by the clipping pipeline — **stub, not yet implemented**.
//!
//! These mirror the clip-relevant subset of planetiler's `GeoUtils`: the world-coordinate
//! projection (whole world = unit square), polygon→linestring conversion, convexity test,
//! self-intersection repair after snapping, and the min-zoom-for-pixel-size heuristic.

#![allow(
    dead_code,
    unused_variables,
    clippy::unimplemented,
    clippy::panic_in_result_fn,
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    reason = "stub API surface for the not-yet-implemented stripe slicer; tests drive the spec"
)]

use geo_types::{Coord, Geometry, LineString};

use crate::stripe::GeometryError;

/// World X (`0..1`) for a longitude in degrees; `0` at -180°, `1` at +180°.
pub fn get_world_x(longitude: f64) -> f64 {
    unimplemented!()
}

/// World Y (`0..1`) for a latitude in degrees (Web Mercator), `0.5` at the equator.
pub fn get_world_y(latitude: f64) -> f64 {
    unimplemented!()
}

/// Convert a world coordinate back to `(longitude, latitude)` degrees.
pub fn world_to_lat_lon(world: Coord<f64>) -> Coord<f64> {
    unimplemented!()
}

/// Convert `(longitude, latitude)` degrees to a world coordinate.
pub fn lat_lon_to_world(lon_lat: Coord<f64>) -> Coord<f64> {
    unimplemented!()
}

/// Convert a polygon / multipolygon / ring into its boundary as a (multi)linestring.
pub fn polygon_to_linestring(geom: &Geometry<f64>) -> Result<Geometry<f64>, GeometryError> {
    unimplemented!()
}

/// Whether a ring is convex (tolerating duplicate points and tiny concavities), used to pick
/// a fast clipping path.
pub fn is_convex(ring: &LineString<f64>) -> bool {
    unimplemented!()
}

/// Repair a polygon that integer snapping / rounding pinched into an invalid shape
/// (self-touch, sliver), returning a valid, correctly-wound polygonal geometry.
pub fn snap_and_fix_polygon(geom: &Geometry<f64>) -> Result<Geometry<f64>, GeometryError> {
    unimplemented!()
}

/// Lowest zoom at which a feature of `world_geometry_size` (in world units) is at least
/// `min_pixel_size` pixels across.
pub fn min_zoom_for_pixel_size(world_geometry_size: f64, min_pixel_size: f64) -> u8 {
    unimplemented!()
}
