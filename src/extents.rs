//! Which tiles a bounding box (and optional clip shape) covers — **stub, not yet implemented**.
//!
//! Mirrors planetiler's `TileExtents` / `TileExtents.ForZoom`. Coordinates are in world
//! space where the whole world is the unit square `(0,0)..(1,1)` (see [`world_bounds`]).

#![allow(
    dead_code,
    unused_variables,
    clippy::unimplemented,
    clippy::must_use_candidate,
    clippy::unused_self,
    clippy::needless_pass_by_value,
    reason = "stub API surface for the not-yet-implemented stripe slicer; tests drive the spec"
)]

use geo_types::{Coord, Geometry, Rect};

/// The whole world as a unit square in world coordinates, `(0,0)..(1,1)`.
///
/// Equivalent to planetiler's `GeoUtils.WORLD_BOUNDS`.
#[must_use]
pub fn world_bounds() -> Rect<f64> {
    Rect::new(Coord { x: 0.0, y: 0.0 }, Coord { x: 1.0, y: 1.0 })
}

/// Per-zoom in-bounds tile ranges derived from a world bounding box and optional clip shape.
#[derive(Debug, Default, Clone)]
pub struct TileExtents {
    zooms: Vec<ForZoom>,
}

impl TileExtents {
    /// Compute per-zoom extents (0..=`max_zoom`) covering `bounds`.
    pub fn compute_from_world_bounds(max_zoom: u8, bounds: Rect<f64>) -> Self {
        unimplemented!()
    }

    /// Like [`Self::compute_from_world_bounds`] but additionally clipped to an arbitrary shape.
    pub fn compute_from_world_bounds_with_shape(
        max_zoom: u8,
        bounds: Rect<f64>,
        shape: Geometry<f64>,
    ) -> Self {
        unimplemented!()
    }

    /// The extent for a single zoom level.
    pub fn for_zoom(&self, z: u8) -> ForZoom {
        unimplemented!()
    }

    /// Whether tile `(x, y)` at zoom `z` is in bounds (and inside the clip shape, if any).
    pub fn test(&self, x: u32, y: u32, z: u8) -> bool {
        unimplemented!()
    }
}

/// In-bounds tile range for one zoom, plus an optional clip shape.
///
/// The range is half-open on the max side, matching planetiler (`min_x..max_x`). Bounds are
/// signed because the buffer can push the searched range below `0`.
#[derive(Debug, Default, Clone)]
pub struct ForZoom {
    pub z: u8,
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
    shape: Option<Geometry<f64>>,
}

impl ForZoom {
    /// Construct a tile range for zoom `z`, optionally clipped to `shape`.
    #[must_use]
    pub fn new(
        z: u8,
        min_x: i32,
        min_y: i32,
        max_x: i32,
        max_y: i32,
        shape: Option<Geometry<f64>>,
    ) -> Self {
        Self {
            z,
            min_x,
            min_y,
            max_x,
            max_y,
            shape,
        }
    }

    /// Whether tile column `x`, row `y` is in bounds (and inside the clip shape, if any).
    pub fn test(&self, x: u32, y: u32) -> bool {
        unimplemented!()
    }

    /// Whether tile column `x` is within the horizontal range.
    pub fn test_x(&self, x: u32) -> bool {
        unimplemented!()
    }

    /// Whether tile row `y` is within the vertical range.
    pub fn test_y(&self, y: u32) -> bool {
        unimplemented!()
    }
}
