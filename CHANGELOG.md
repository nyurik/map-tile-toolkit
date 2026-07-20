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

### Changed

- Bumped edition to 2024 and MSRV to 1.88 (required by `geo` 0.33).
