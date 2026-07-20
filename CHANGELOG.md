# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Geometry slicing: `slice_tile`, `slice_all_tiles`, and `for_each_tile_slice` convert
  Web Mercator (EPSG:3857) `geo` geometries into per-tile `Geometry<i32>` slices, clipped
  to a tile plus buffer, snapped to the integer tile grid, and oriented for tile — ready for
  encoding with no further geometry processing. Plus the `TileId` and `SliceOptions` types.
- Eager stripe slicer (`stripe` module): `TiledGeometry` slices one polygon/polyline into
  every tile it touches at a zoom (per-tile coordinate sequences + interior fill detection +
  antimeridian wrapping), ported from planetiler/geojson-vt. Plus `TileExtents` (`extents`)
  and clip helpers (`geo_utils`: world projection, `polygon_to_linestring`, `is_convex`,
  `snap_and_fix_polygon`, `min_zoom_for_pixel_size`). Validated against planetiler's test
  suite (75 tests). Two assertions are `#[ignore]`d: they depend on JTS `buffer(0)` /
  `GeometryPrecisionReducer` output that `geo`'s overlay engine resolves to a
  topologically-valid but not bit-identical result.
