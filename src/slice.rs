//! Public slicing entry points.
//!
//! [`slice_tile`] clips a geometry to a single tile (the tile-server path); [`slice_all_tiles`]
//! and [`for_each_tile_slice`] slice it into every tile it touches at a zoom (the batch path).
//! All three produce per-tile [`Geometry<i32>`] in tile-local integer coordinates, ready for
//! tile encoding with no further geometry processing.

use geo::BoundingRect as _;
use geo_types::Geometry;

use crate::clip::clip_geometry_to_tile;
use crate::tile::{SliceOptions, TileId, tile_index_mercator};

/// Clip a Web Mercator (EPSG:3857) geometry to one tile plus its buffer, snap it to the
/// integer tile grid, orient rings, and validate.
///
/// Returns tile-local integer coordinates, or `None` when nothing of the geometry falls
/// inside the (buffered) tile. A collection input yields a [`Geometry::GeometryCollection`];
/// since one tile feature holds a single geometry, flatten it before encoding.
#[must_use]
pub fn slice_tile(
    geom: &Geometry<f64>,
    tile: impl Into<TileId>,
    opts: SliceOptions,
) -> Option<Geometry<i32>> {
    clip_geometry_to_tile(geom, tile.into(), opts)
}

/// Slice a geometry into every tile it touches at `zoom`.
///
/// Candidate tiles are enumerated from the geometry's bounding box and clipped with
/// [`slice_tile`], so the results are identical to calling [`slice_tile`] per tile. Only
/// non-empty tiles are yielded, in row-major order.
pub fn slice_all_tiles(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
) -> impl Iterator<Item = (TileId, Geometry<i32>)> + '_ {
    let (min_col, min_row, max_col, max_row) = tile_range(geom, zoom);
    (min_row..=max_row).flat_map(move |y| {
        (min_col..=max_col).filter_map(move |x| {
            let tile = TileId::new(x, y, zoom);
            clip_geometry_to_tile(geom, tile, opts).map(|g| (tile, g))
        })
    })
}

/// Slice a geometry into every tile it touches at `zoom`, invoking `f` for each non-empty
/// tile. A zero-allocation alternative to [`slice_all_tiles`] for hot batch loops.
pub fn for_each_tile_slice(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
    mut f: impl FnMut(TileId, Geometry<i32>),
) {
    let (min_col, min_row, max_col, max_row) = tile_range(geom, zoom);
    for y in min_row..=max_row {
        for x in min_col..=max_col {
            let tile = TileId::new(x, y, zoom);
            if let Some(g) = clip_geometry_to_tile(geom, tile, opts) {
                f(tile, g);
            }
        }
    }
}

/// Inclusive `(min_col, min_row, max_col, max_row)` tile range covering the geometry's
/// bounding box at `zoom`. Returns an empty range (`min > max`) for empty geometry.
fn tile_range(geom: &Geometry<f64>, zoom: u8) -> (u32, u32, u32, u32) {
    let Some(bbox) = geom.bounding_rect() else {
        // Empty geometry: an inclusive range with min > max yields nothing.
        return (1, 1, 0, 0);
    };
    let (mn, mx) = (bbox.min(), bbox.max());
    // Take both corners and min/max the indices, so the y-axis flip (mercator up vs tile
    // row down) is handled without special-casing.
    let (xa, ya) = tile_index_mercator(mn.x, mn.y, zoom);
    let (xb, yb) = tile_index_mercator(mx.x, mx.y, zoom);
    (xa.min(xb), ya.min(yb), xa.max(xb), ya.max(yb))
}
