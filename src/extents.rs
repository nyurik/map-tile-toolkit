//! Which tiles a bounding box (and optional clip shape) covers, ported from planetiler's
//! `TileExtents`. Coordinates are world space where the whole world is the unit square
//! `(0,0)..(1,1)` (see [`world_bounds`]).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "quantizing world bounds to integer tile ranges"
)]

use geo::{AffineOps as _, AffineTransform};
use geo_types::{Coord, Geometry, Rect};

use crate::stripe::{CoveredTiles, TiledGeometry};

/// The whole world as a unit square in world coordinates, `(0,0)..(1,1)`.
#[must_use]
pub fn world_bounds() -> Rect<f64> {
    Rect::new(Coord { x: 0.0, y: 0.0 }, Coord { x: 1.0, y: 1.0 })
}

fn quantize_down(value: f64, levels: i32) -> i32 {
    ((value * f64::from(levels)).floor() as i32).clamp(0, levels)
}

fn quantize_up(value: f64, levels: i32) -> i32 {
    ((value * f64::from(levels)).ceil() as i32).clamp(0, levels)
}

/// Per-zoom in-bounds tile ranges derived from a world bounding box and optional clip shape.
#[derive(Debug, Default, Clone)]
pub struct TileExtents {
    zooms: Vec<ForZoom>,
}

impl TileExtents {
    /// Compute per-zoom extents (`0..=max_zoom`) covering `bounds` (world coordinates).
    #[must_use]
    pub fn compute_from_world_bounds(max_zoom: u8, bounds: Rect<f64>) -> Self {
        Self::compute(max_zoom, bounds, None)
    }

    /// Like [`Self::compute_from_world_bounds`] but additionally clipped to `shape`
    /// (also given in world coordinates).
    ///
    /// # Panics
    /// Panics if `shape` is invalid in a way that prevents computing its covered tiles.
    #[must_use]
    pub fn compute_from_world_bounds_with_shape(
        max_zoom: u8,
        bounds: Rect<f64>,
        shape: &Geometry<f64>,
    ) -> Self {
        Self::compute(max_zoom, bounds, Some(shape))
    }

    fn compute(max_zoom: u8, bounds: Rect<f64>, shape: Option<&Geometry<f64>>) -> Self {
        let (min, max) = (bounds.min(), bounds.max());
        let mut zooms = Vec::with_capacity(usize::from(max_zoom) + 1);
        for zoom in 0..=max_zoom {
            let levels = 1i32 << zoom;
            let mut for_zoom = ForZoom::new(
                zoom,
                quantize_down(min.x, levels),
                quantize_down(min.y, levels),
                quantize_up(max.x, levels),
                quantize_up(max.y, levels),
                None,
            );
            if let Some(shape) = shape {
                let scale = f64::from(levels);
                let scaled =
                    shape.affine_transform(&AffineTransform::new(scale, 0.0, 0.0, 0.0, scale, 0.0));
                let covered =
                    TiledGeometry::get_covered_tiles(&scaled, zoom, &for_zoom).unwrap_or_default();
                for_zoom = for_zoom.with_shape(covered);
            }
            zooms.push(for_zoom);
        }
        Self { zooms }
    }

    /// The extent for a single zoom level.
    ///
    /// # Panics
    /// Panics if `z` exceeds the `max_zoom` this was computed for.
    #[must_use]
    pub fn for_zoom(&self, z: u8) -> ForZoom {
        self.zooms[usize::from(z)].clone()
    }

    /// Whether tile `(x, y)` at zoom `z` is in bounds (and inside the clip shape, if any).
    #[must_use]
    pub fn test(&self, x: u32, y: u32, z: u8) -> bool {
        self.zooms[usize::from(z)].test(x as i32, y as i32)
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
    shape: Option<CoveredTiles>,
}

impl ForZoom {
    /// Construct a tile range for zoom `z`, optionally clipped to a set of covered tiles.
    #[must_use]
    pub fn new(
        z: u8,
        min_x: i32,
        min_y: i32,
        max_x: i32,
        max_y: i32,
        shape: Option<CoveredTiles>,
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

    #[must_use]
    fn with_shape(self, shape: CoveredTiles) -> Self {
        Self {
            shape: Some(shape),
            ..self
        }
    }

    pub(crate) fn min_y(&self) -> i32 {
        self.min_y
    }

    pub(crate) fn max_y(&self) -> i32 {
        self.max_y
    }

    /// Whether tile column `x`, row `y` is in bounds (and inside the clip shape, if any).
    #[must_use]
    pub fn test(&self, x: i32, y: i32) -> bool {
        self.test_x(x) && self.test_y(y) && self.test_shape(x, y)
    }

    fn test_shape(&self, x: i32, y: i32) -> bool {
        match &self.shape {
            Some(covered) => match (u32::try_from(x), u32::try_from(y)) {
                (Ok(xu), Ok(yu)) => covered.test(xu, yu),
                _ => false,
            },
            None => true,
        }
    }

    /// Whether tile column `x` is within the horizontal range.
    #[must_use]
    pub fn test_x(&self, x: i32) -> bool {
        x >= self.min_x && x < self.max_x
    }

    /// Whether tile row `y` is within the vertical range.
    #[must_use]
    pub fn test_y(&self, y: i32) -> bool {
        y >= self.min_y && y < self.max_y
    }
}
