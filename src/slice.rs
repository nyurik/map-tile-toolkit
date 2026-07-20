//! Public slicing entry points.
//!
//! [`slice_tile`] clips a geometry to a single tile (the tile-server path, via the per-tile
//! rectangle clip). [`slice_all_tiles`] and [`for_each_tile_slice`] slice a geometry into
//! every tile it touches at a zoom in one pass with the eager [`crate::stripe`] slicer, which
//! is near-linear in the geometry size rather than `O(tiles × geometry)`. All three produce
//! per-tile [`Geometry<i32>`] in tile-local integer coordinates, ready for tile encoding with
//! no further geometry processing.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "coordinate scaling between Web Mercator, tile units, and the integer grid"
)]

use std::collections::BTreeMap;

use geo::{BoundingRect as _, MapCoords as _};
use geo_types::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};

use crate::clip::{clip_geometry_to_tile, finalize_area, to_i32, validate_and_simplify};
use crate::extents::ForZoom;
use crate::stripe::{CoordSeqGroups, TILE_SIZE, TiledGeometry};
use crate::tile::{EARTH_CIRCUMFERENCE, ORIGIN_SHIFT, SliceOptions, TileId, tile_index_mercator};

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

/// Slice a geometry into every tile it touches at `zoom`, yielding each non-empty tile in
/// row-major order.
pub fn slice_all_tiles(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
) -> impl Iterator<Item = (TileId, Geometry<i32>)> {
    slice_all_map(geom, zoom, opts).into_iter()
}

/// Slice a geometry into every tile it touches at `zoom`, invoking `f` for each non-empty
/// tile (row-major order).
pub fn for_each_tile_slice(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
    mut f: impl FnMut(TileId, Geometry<i32>),
) {
    for (tile, g) in slice_all_map(geom, zoom, opts) {
        f(tile, g);
    }
}

// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Point,
    Line,
    Area,
}

/// The slicing kind for a non-collection geometry, or `None` for unsupported variants.
fn geom_kind(geom: &Geometry<f64>) -> Option<Kind> {
    match geom {
        Geometry::Point(_) | Geometry::MultiPoint(_) => Some(Kind::Point),
        Geometry::LineString(_) | Geometry::MultiLineString(_) => Some(Kind::Line),
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => Some(Kind::Area),
        _ => None,
    }
}

/// Slice a geometry into a per-tile map of integer geometry (the shared batch engine).
fn slice_all_map(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
) -> BTreeMap<TileId, Geometry<i32>> {
    match geom {
        Geometry::GeometryCollection(gc) => {
            let mut acc: BTreeMap<TileId, Geometry<i32>> = BTreeMap::new();
            for member in &gc.0 {
                for (tile, g) in slice_all_map(member, zoom, opts) {
                    acc.entry(tile)
                        .and_modify(|existing| *existing = combine(existing.clone(), g.clone()))
                        .or_insert(g);
                }
            }
            acc
        }
        _ => slice_non_collection(geom, zoom, opts),
    }
}

/// Merge two per-tile geometries into a flattened [`Geometry::GeometryCollection`].
fn combine(a: Geometry<i32>, b: Geometry<i32>) -> Geometry<i32> {
    let mut members = match a {
        Geometry::GeometryCollection(gc) => gc.0,
        other => vec![other],
    };
    match b {
        Geometry::GeometryCollection(gc) => members.extend(gc.0),
        other => members.push(other),
    }
    Geometry::GeometryCollection(GeometryCollection(members))
}

fn slice_non_collection(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
) -> BTreeMap<TileId, Geometry<i32>> {
    let Some(kind) = geom_kind(geom) else {
        return BTreeMap::new();
    };
    let scale = f64::from(1u32 << zoom);
    let tile_units = geom.map_coords(|c| Coord {
        x: (c.x + ORIGIN_SHIFT) / EARTH_CIRCUMFERENCE * scale,
        y: (ORIGIN_SHIFT - c.y) / EARTH_CIRCUMFERENCE * scale,
    });
    let n = 1i32 << zoom;
    let extents = ForZoom::new(zoom, 0, 0, n, n, None);
    let buffer = opts.buffer_fraction();

    let Ok(tiled) = TiledGeometry::slice_geometry(&tile_units, 0.0, buffer, zoom, &extents) else {
        // A polygon that the stripe slicer cannot fill-resolve falls back to the per-tile
        // rectangle clip, which handles arbitrary input without a fill step.
        return slice_per_tile(geom, zoom, opts);
    };

    let px = f64::from(opts.extent.get()) / TILE_SIZE;
    let mut out = BTreeMap::new();
    for (tile, groups) in tiled.tile_data() {
        if let Some(g) = reassemble_tile(groups, kind, px) {
            out.insert(*tile, g);
        }
    }
    for tile in tiled.filled_tiles() {
        if let Some(g) = fill_tile(opts) {
            out.insert(tile, g);
        }
    }
    out
}

/// Scale a tile-local (`0..TILE_SIZE`) coordinate to the integer grid and floor it.
fn scaled(c: Coord<f64>, px: f64) -> Coord<f64> {
    Coord {
        x: (c.x * px).floor(),
        y: (c.y * px).floor(),
    }
}

/// Reassemble one tile's stripe output (tile-local coordinate sequences) into integer
/// tile-grid geometry, matching the orientation/validation of the per-tile clip.
fn reassemble_tile(groups: &CoordSeqGroups, kind: Kind, px: f64) -> Option<Geometry<i32>> {
    match kind {
        Kind::Point => {
            let pts: Vec<Point<f64>> = groups
                .iter()
                .flatten()
                .flat_map(|ls| ls.0.iter().map(|&c| Point(scaled(c, px))))
                .collect();
            let geom = match pts.len() {
                0 => return None,
                1 => Geometry::Point(pts[0]),
                _ => Geometry::MultiPoint(MultiPoint(pts)),
            };
            Some(to_i32(&geom))
        }
        Kind::Line => {
            let lines: Vec<LineString<f64>> = groups
                .iter()
                .flatten()
                .map(|ls| LineString(ls.0.iter().map(|&c| scaled(c, px)).collect()))
                .filter(|ls| ls.0.len() >= 2)
                .collect();
            let geom = match lines.len() {
                0 => return None,
                1 => Geometry::LineString(lines.into_iter().next()?),
                _ => Geometry::MultiLineString(MultiLineString(lines)),
            };
            validate_and_simplify(geom).as_ref().map(to_i32)
        }
        Kind::Area => {
            let polys: Vec<Polygon<f64>> = groups
                .iter()
                .map(|group| {
                    let exterior = ring_scaled(group.first(), px, false);
                    let holes = group
                        .iter()
                        .skip(1)
                        .map(|h| ring_scaled(Some(h), px, true))
                        .collect();
                    Polygon::new(exterior, holes)
                })
                .collect();
            finalize_area(MultiPolygon(polys)).as_ref().map(to_i32)
        }
    }
}

/// Scale a ring to the integer grid; `reverse` flips winding (inner rings wind opposite the
/// exterior so the polygon validates without an overlay repair).
fn ring_scaled(ring: Option<&LineString<f64>>, px: f64, reverse: bool) -> LineString<f64> {
    let mut pts: Vec<Coord<f64>> = ring
        .map(|ls| ls.0.iter().map(|&c| scaled(c, px)).collect())
        .unwrap_or_default();
    if reverse {
        pts.reverse();
    }
    LineString(pts)
}

/// A whole-tile fill polygon (buffered) on the integer grid.
fn fill_tile(opts: SliceOptions) -> Option<Geometry<i32>> {
    let extent = f64::from(opts.extent.get());
    let buffer = f64::from(opts.buffer);
    let (lo, hi) = (-buffer, extent + buffer);
    let ring = LineString(vec![
        Coord { x: lo, y: lo },
        Coord { x: hi, y: lo },
        Coord { x: hi, y: hi },
        Coord { x: lo, y: hi },
        Coord { x: lo, y: lo },
    ]);
    finalize_area(MultiPolygon(vec![Polygon::new(ring, vec![])]))
        .as_ref()
        .map(to_i32)
}

/// Fallback batch path: enumerate candidate tiles from the bounding box and clip each with
/// the per-tile rectangle clip. Used only when the stripe slicer reports an unfillable polygon.
fn slice_per_tile(
    geom: &Geometry<f64>,
    zoom: u8,
    opts: SliceOptions,
) -> BTreeMap<TileId, Geometry<i32>> {
    let mut out = BTreeMap::new();
    let Some(bbox) = geom.bounding_rect() else {
        return out;
    };
    let (mn, mx) = (bbox.min(), bbox.max());
    let (xa, ya) = tile_index_mercator(mn.x, mn.y, zoom);
    let (xb, yb) = tile_index_mercator(mx.x, mx.y, zoom);
    for y in ya.min(yb)..=ya.max(yb) {
        for x in xa.min(xb)..=xa.max(xb) {
            let tile = TileId::new(x, y, zoom);
            if let Some(g) = clip_geometry_to_tile(geom, tile, opts) {
                out.insert(tile, g);
            }
        }
    }
    out
}
