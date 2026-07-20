//! Tile addressing, slice options, and the Web Mercator tile-grid math.
//!
//! The math here operates directly on Web Mercator (EPSG:3857) coordinates in meters,
//! matching the crate's input contract. It is the small, stable subset of
//! `martin-tile-utils` the slicer needs, reimplemented so the toolkit stays free of a
//! dependency on the `martin` repository.

use std::num::NonZeroU32;

/// Earth circumference in meters at the equator, the width/height of the Web Mercator plane.
///
/// Matches `martin_tile_utils::EARTH_CIRCUMFERENCE`.
pub(crate) const EARTH_CIRCUMFERENCE: f64 = 40_075_016.685_578_5;

/// Half the [`EARTH_CIRCUMFERENCE`]: the Web Mercator plane spans `-ORIGIN_SHIFT..=ORIGIN_SHIFT`.
pub(crate) const ORIGIN_SHIFT: f64 = EARTH_CIRCUMFERENCE / 2.0;

/// A tile address in the standard XYZ scheme (`z` = zoom, `x`/`y` = column/row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub x: u32,
    pub y: u32,
    pub z: u8,
}

impl TileId {
    /// Create a tile address from its column, row, and zoom.
    #[must_use]
    pub fn new(x: u32, y: u32, z: u8) -> Self {
        Self { x, y, z }
    }
}

impl From<(u32, u32, u8)> for TileId {
    fn from((x, y, z): (u32, u32, u8)) -> Self {
        Self { x, y, z }
    }
}

/// How a tile is rendered: the integer grid resolution and the clip margin kept
/// around the tile edge.
#[derive(Debug, Clone, Copy)]
pub struct SliceOptions {
    /// Side length of the tile integer coordinate grid (e.g. `4096`).
    pub extent: NonZeroU32,
    /// Clip margin retained around the tile edge, in tile units (a fraction of
    /// [`extent`](Self::extent), e.g. `64` → `64/4096`).
    pub buffer: u32,
}

impl SliceOptions {
    /// Create options from an extent and a buffer.
    #[must_use]
    pub fn new(extent: NonZeroU32, buffer: u32) -> Self {
        Self { extent, buffer }
    }

    /// The clip margin as a fraction of the tile width, e.g. `64 / 4096`.
    pub(crate) fn buffer_fraction(self) -> f64 {
        f64::from(self.buffer) / f64::from(self.extent.get())
    }
}

impl Default for SliceOptions {
    /// The tile defaults used by most tile servers: `4096` extent, `64` buffer.
    fn default() -> Self {
        Self {
            extent: NonZeroU32::new(4096).unwrap_or(NonZeroU32::MIN),
            buffer: 64,
        }
    }
}

/// Side length of one tile in Web Mercator meters at the given zoom.
///
/// Mirrors `martin`'s `tile_length_from_zoom`.
pub(crate) fn tile_length_from_zoom(zoom: u8) -> f64 {
    EARTH_CIRCUMFERENCE / f64::from(1_u32 << zoom)
}

/// The Web Mercator bounding box of a tile as `[min_x, min_y, max_x, max_y]`.
///
/// Mirrors `martin_tile_utils::tile_bbox`.
#[expect(clippy::cast_lossless)]
pub(crate) fn tile_bbox(x: u32, y: u32, tile_length: f64) -> [f64; 4] {
    let min_x = -ORIGIN_SHIFT + x as f64 * tile_length;
    let max_y = ORIGIN_SHIFT - y as f64 * tile_length;
    [min_x, max_y - tile_length, min_x + tile_length, max_y]
}

/// Column/row of the tile containing a Web Mercator coordinate at the given zoom.
///
/// This is the projected-space core of `martin_tile_utils::tile_index` (which takes
/// WGS84 and reprojects first); the toolkit works in Web Mercator, so it skips the
/// reprojection. The result is clamped to the valid tile range for the zoom.
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn tile_index_mercator(x: f64, y: f64, zoom: u8) -> (u32, u32) {
    let tile_size = tile_length_from_zoom(zoom);
    let max_index = (1_u32 << zoom) - 1;
    let col = (((x + ORIGIN_SHIFT).abs() / tile_size) as u32).min(max_index);
    let row = (((ORIGIN_SHIFT - y).abs() / tile_size) as u32).min(max_index);
    (col, row)
}

#[cfg(test)]
mod tests {
    use approx::assert_relative_eq;

    use super::*;

    #[test]
    fn bbox_covers_whole_world_at_zoom_0() {
        let bbox = tile_bbox(0, 0, tile_length_from_zoom(0));
        assert_relative_eq!(bbox[0], -ORIGIN_SHIFT);
        assert_relative_eq!(bbox[1], -ORIGIN_SHIFT);
        assert_relative_eq!(bbox[2], ORIGIN_SHIFT);
        assert_relative_eq!(bbox[3], ORIGIN_SHIFT);
    }

    #[test]
    fn tile_index_round_trips_with_bbox() {
        // Center of a known tile must map back to that tile.
        let zoom = 7;
        let (tx, ty) = (70, 43);
        let len = tile_length_from_zoom(zoom);
        let [min_x, min_y, max_x, max_y] = tile_bbox(tx, ty, len);
        let (cx, cy) = (f64::midpoint(min_x, max_x), f64::midpoint(min_y, max_y));
        assert_eq!(tile_index_mercator(cx, cy, zoom), (tx, ty));
    }

    #[test]
    fn tile_index_is_clamped_to_zoom_range() {
        // A point at the far edge stays within the valid tile range.
        assert_eq!(tile_index_mercator(ORIGIN_SHIFT, -ORIGIN_SHIFT, 1), (1, 1));
    }
}
