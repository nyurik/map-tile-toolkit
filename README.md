# map-tile-toolkit

[![GitHub repo](https://img.shields.io/badge/github-nyurik/map--tile--toolkit-8da0cb?logo=github)](https://github.com/nyurik/map-tile-toolkit)
[![crates.io version](https://img.shields.io/crates/v/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![crate usage](https://img.shields.io/crates/d/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![docs.rs status](https://img.shields.io/docsrs/map-tile-toolkit)](https://docs.rs/map-tile-toolkit)
[![crates.io license](https://img.shields.io/crates/l/map-tile-toolkit)](https://github.com/nyurik/map-tile-toolkit/blob/main/LICENSE-APACHE)
[![CI build status](https://github.com/nyurik/map-tile-toolkit/actions/workflows/ci.yml/badge.svg)](https://github.com/nyurik/map-tile-toolkit/actions)
[![Codecov](https://img.shields.io/codecov/c/github/nyurik/map-tile-toolkit)](https://app.codecov.io/gh/nyurik/map-tile-toolkit)

Clip integer **polylines** (`LineString`/`MultiLineString`) into per-tile pieces on a simple
integer tile grid. A tile of side `size` covers the closed square `[x·size, x·size + size − 1]`
on each axis, so tile boundaries sit halfway between integer coordinates and every vertex belongs
to exactly one tile. Clipping keeps the geometry's **original vertices** — every vertex inside a
tile, plus the first vertex just outside each time the line enters or leaves — rather than cutting
new vertices at the tile edge.

## Usage

The slicer never panics: invalid input (a non-polyline geometry, an oversized polyline, or
coordinates that overflow the tile math) returns a `map_tile_toolkit::Error` instead.

```rust
use geo_types::{Geometry, LineString};
use map_tile_toolkit::{Slicer, TileId};

// An integer polyline. With `divider = 25`,
// tiles are 25 units wide; `buffer` grows each tile's
// clip box outward (0 = tight against the grid).
let line = Geometry::LineString(LineString::from(vec![(5, 5), (20, 20), (60, 40)]));
let slicer = Slicer::new(25, 0)?;

// Batch: every tile the polyline touches.
// Each piece is in that tile's local coordinates — the tile's [0, 0] corner
// is the origin, so add `(tile.x, tile.y) * divider` to recover global coords.
for (tile, piece) in slicer.slice_all(&line)? {
    let _ = (tile, piece);
}

// Single tile: clip to one tile (tile-local coords), or `None` when
// the line does not touch it.
if let Some(piece) = slicer.slice(&line, TileId::new(0, 0))? {
    let _ = piece;
}

// Stitch two tiles' local pieces back into one geometry (in a shared local frame).
// `merge` folds, so you can keep merging the result with further
// tiles to reconstruct the whole polyline.
let tiles = slicer.slice_all(&line)?;
if let [a, b, ..] = tiles.as_slice() {
    let _merged = slicer.merge((a.0, &a.1), (b.0, &b.1))?;
}
# Ok::<(), map_tile_toolkit::Error>(())
```

`slice_all` and `slice` agree by construction: the pair `slice_all` yields for a tile equals what
`slice` returns for that tile.

### Per-vertex payloads (M values)

`geo-types` coordinates are just `x`/`y`, but the slicer is generic over a `Vertex` trait, so a
vertex can carry any `Copy + PartialEq` payload (an M/measure value, an id, …) that rides through slicing
and merging **unchanged** — the slicer never cuts new vertices, so there is nothing to interpolate.
`Coord<i32>` implements `Vertex` (the `Geometry<i32>` API above), and `Measured<M>` pairs a position
with a payload:

```rust
use map_tile_toolkit::{Measured, Slicer, TileId};

let slicer = Slicer::new(25, 0)?;
// The payload must be `Copy + PartialEq`, so use an integer id
// or a fixed-point (e.g. millimetre) measure rather than a raw `f32`.
let line = vec![vec![
    Measured::new(5, 5, 1_000_u32),
    Measured::new(20, 20, 2_500),
    Measured::new(60, 40, 4_000),
]];

// `slice_lines` / `slice_all_lines` take any `&[impl AsRef<[V]>]` and return runs
// of `V`, each vertex's payload preserved. `merge_lines` stitches two tiles' pieces
// back together.
let per_tile = slicer.slice_all_lines(&line)?;
for (tile, runs) in per_tile {
    let _ = (tile, runs); // runs: Vec<Vec<Measured<u32>>> in that tile's local frame
}
# Ok::<(), map_tile_toolkit::Error>(())
```

## Development

* This project is easier to develop with [just](https://github.com/casey/just#readme), a modern alternative to `make`.
  Install it with `cargo install just`.
* To get a list of available commands, run `just`.
* To run tests, use `just test`.
* Tests are data-driven: each `tests/fixtures/inputs/*.geojson` polyline is sliced with both the
  batch and per-tile paths (asserted byte-identical) and snapshotted as a `.geojson`
  `FeatureCollection` (the original line plus every per-tile piece) that renders on a map.
  `tests/fixtures/grid.geojson` overlays the tile grid. Run `just bless` to regenerate snapshots.

## Visualizing Tests

All input tests are in `tests/fixtures` dir, and integration test converts each to a snapshot in `tests/snapshots` dir also as a `.geojson` file. The snapshot contains original input geometry as the first feature, followed by all slices.

Use QGIS or some other .geojson file viewer to inspect
* Browse to `tests` dir in QGIS "Browser" panel
* Add `grid.geojson` to "Layers" panel to have a reference. Note that the grid uses .5 pixel offset to show tile boundaries between integer coordinates.
* Select all .geojson files from the `snapshots` subdir and add them to "Layers" panel (you may need to click "Accept layers" a few times)
* Select the added snap files except for the grid one and hide them (click on first, shift+click on last, space bar)
* Click on one test case, and make sure it is both clicked and there is a checked checkbox next to it to show it.
* Enable "identify features" tool (ctrl+shift+i), and drag a big box over all geometries
* use "identify results" panel to inspect each segment

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)
  at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual-licensed as above, without any
additional terms or conditions.
