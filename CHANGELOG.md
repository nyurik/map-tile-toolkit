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
  `snap_and_fix_polygon`, `min_zoom_for_pixel_size`).
- Ported planetiler's full geometry-clipping test suite (91 tests, all running, none ignored):
  every `TiledGeometryTest` and `TileExtentsTest` case, the clip-relevant `GeoUtilsTest`
  cases, and every clipping case of `FeatureRendererTest` — including the rotation
  intersection oracle (compared against `geo::BooleanOps`), fill inference, nested/overlapping
  multipolygons, and antimeridian wrapping. Non-clipping cases (simplification, min-size,
  encode-grid rounding, label grids, linear ranges, geometry pipeline) are documented as
  intentionally out of scope, along with `testSpiral` (input needs JTS `buffer()`) and
  `testEmitPointsRespectShape` (needs a planetiler resource; the shape-clip path is covered by
  `tile_extents::shape`). Two cases assert `geo`'s result with a documented `DIVERGENCE FROM
  PLANETILER` note, because JTS `buffer(0)`/`GeometryPrecisionReducer` and `geo`'s overlay
  engine repair self-overlaps to topologically-valid but not bit-identical geometry
  (`snap_and_fix_issue_511` area; `fix_invalid_input_geometry` apex).
- `visualize` example (`cargo run --example visualize` / `just visualize`) that reprojects the
  original geometry, every per-tile slice, and the tile grid back to lon/lat and prints a styled
  GeoJSON feature collection for pasting into geojson.io / QGIS / kepler.gl.
- Visual regression snapshots (`tests/geojson_snapshots.rs`): each `tests/fixtures/geojson/*.geojson`
  fixture is sliced into two binary `.geojson` `insta` snapshots — one from `slice_all_tiles` and
  one from `slice_tile` per covered tile — so both slicing paths are covered (input geometry as the
  first feature, then one feature per tile, with simplestyle colors). The `.geojson` extension makes
  the snapshots render on a map directly in GitHub, so a diff is a visual diff.
- `criterion` benchmarks (`benches/clipping.rs`, run with `cargo bench`) comparing the two
  clipping engines over geometry of increasing complexity — the per-tile `geo`-overlay path
  (`slice_tile`/`slice_all_tiles`) vs the eager `stripe` slicer — plus a stripe fill-detection
  case. (Planetiler ships no clipping benchmark; these follow the shape of its
  `BenchmarkSimplify`.)

### Changed

- `slice_tile` now clips with dedicated axis-aligned rectangle primitives (Sutherland-Hodgman
  for polygons) instead of `geo`'s general boolean overlay — `O(vertices)` and ~3–8× faster on the
  `clip_one_tile` benchmark.
- `slice_tile` clips line strings by **keeping their original vertices** rather than cutting new
  ones at the tile edge: it keeps every vertex inside the buffered tile plus the first vertex just
  outside each boundary crossing, dropping fully-outside stretches. A line that leaves and
  re-enters returns as a `MultiLineString` (one piece per visit), and kept outside vertices may lie
  beyond the buffer. (This affects only the single-tile path; the `slice_all_tiles` batch path
  still splits lines at tile boundaries via the `stripe` slicer.)
- `slice_all_tiles` / `for_each_tile_slice` now slice in a single pass with the eager `stripe`
  slicer (near-linear in geometry size) instead of clipping every candidate tile independently
  (~5–6× faster on the batch benchmark). The two engines produce equivalent slices up to ±1px
  integer snapping, so `slice_all_tiles` and `slice_tile` agree topologically/area-wise rather
  than byte-for-byte.
