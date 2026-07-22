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

The core API works on a plain **polyline** — anything sliceable to `[Coord]` (`&[Coord]`, `&Vec<Coord>`,
an array). Two slicers accumulate the results: `SlicerAll` keeps every tile a polyline touches, and
`SlicerOne` keeps a single, fixed tile. Each polyline is added as (part of) a **feature** and read
back with iterators — never owned `Vec`s. The slicer never panics: bad input (an oversized polyline,
or coordinates that overflow the tile math) returns a `map_tile_toolkit::Error` instead.

```rust
use geo_types::Coord;
use map_tile_toolkit::{SlicerAll, TileId};

// An integer polyline. `divider = 25` → 25-unit tiles; `buffer` grows each
// tile's clip box outward (0 = tight against the grid).
let line = [Coord { x: 5, y: 5 }, Coord { x: 20, y: 20 }, Coord { x: 60, y: 40 }];

let mut slicer = SlicerAll::new(25, 0)?;
slicer.add_feature(&line)?;

// Read back: tiles → features → polylines. Polylines are in that tile's local coordinates — the
// tile's [0, 0] corner is the origin, so add `(tile.x, tile.y) * divider` to recover global
// coords. A feature can yield several polylines in a tile (leave + re-enter).
for tile in slicer.iter_tiles() {
    let id: TileId = tile.id();
    for feature in tile.iter_features() {
        for polyline in feature.iter_polylines() {
            let _ = (id, polyline); // polyline: &[Coord<i32>]
        }
    }
}
# Ok::<(), map_tile_toolkit::Error>(())
```

For a single tile, `SlicerOne` skips the tile level — `iter_features` yields the features directly:

```rust
use geo_types::Coord;
use map_tile_toolkit::{SlicerOne, TileId};

let line = [Coord { x: 5, y: 5 }, Coord { x: 20, y: 20 }, Coord { x: 60, y: 40 }];

let mut tile = SlicerOne::new(25, 0, TileId::new(0, 0))?;
tile.add_feature(&line)?;
for feature in tile.iter_features() {
    for polyline in feature.iter_polylines() {
        let _ = polyline; // clipped to tile (0, 0), in its local frame
    }
}
# Ok::<(), map_tile_toolkit::Error>(())
```

`SlicerAll` and `SlicerOne` agree by construction: the polylines `SlicerAll` yields for a tile equal
what a `SlicerOne` bound to that tile yields.

### Features, and merging tiles back

`add_feature` begins a feature; `continue_last_feature` extends the one it opened — so the several
lines of one multi-line geometry become a single feature, while unrelated inputs become separate
features. `merge` is the inverse of slicing.

```rust
use geo_types::Coord;
use map_tile_toolkit::{SlicerAll, TileId, merge};

let mut slicer = SlicerAll::new(25, 0)?;

// One multi-line feature: the first line opens it, each further line extends the same feature.
let part_a = [Coord { x: 5, y: 5 }, Coord { x: 60, y: 40 }];
let part_b = [Coord { x: 8, y: 8 }, Coord { x: 8, y: 70 }];
slicer.add_feature(&part_a)?.continue_last_feature(&part_b)?;

// A separate, unrelated feature.
slicer.add_feature([Coord { x: 40, y: 5 }, Coord { x: 45, y: 90 }])?;

// `merge` stitches two tiles' runs back into a shared local frame; non-adjacent tiles simply stay
// disconnected until a connecting tile is merged in. It is stateless — pass the divider and each
// tile's runs explicitly (here, every polyline of every feature in the tile).
let tiles: Vec<(TileId, Vec<&[Coord<i32>]>)> = slicer
    .iter_tiles()
    .map(|t| (t.id(), t.iter_features().flat_map(|f| f.iter_polylines()).collect()))
    .collect();
if let [(ta, ra), (tb, rb), ..] = tiles.as_slice() {
    let _merged = merge(slicer.divider(), (*ta, ra.as_slice()), (*tb, rb.as_slice()))?;
}
# Ok::<(), map_tile_toolkit::Error>(())
```

### Per-vertex payloads (M values)

The slicers are generic over a `Vertex` trait (defaulting to `Coord<i32>`), so a vertex can carry any
`Copy + PartialEq` payload (an M/measure value, an id, …) that rides through slicing and merging
**unchanged** — the slicer never cuts new vertices, so there is nothing to interpolate. `Measured<M>`
pairs a position with a payload:

```rust
use map_tile_toolkit::{Measured, SlicerAll};

let mut slicer = SlicerAll::new(25, 0)?;
// The payload must be `Copy + PartialEq`; e.g. an integer id or a fixed-point measure.
let line = [
    Measured::new(5, 5, 1_000_u32),
    Measured::new(20, 20, 2_500),
    Measured::new(60, 40, 4_000),
];
slicer.add_feature(&line)?;
for tile in slicer.iter_tiles() {
    for feature in tile.iter_features() {
        for polyline in feature.iter_polylines() {
            let _ = polyline; // &[Measured<u32>], payload preserved
        }
    }
}
# Ok::<(), map_tile_toolkit::Error>(())
```

### `geo-types` geometries

With the default `geo` feature, the slicers bridge to `geo-types`: feed a `LineString` /
`MultiLineString` `Geometry` (as one feature) and read `Geometry` pieces back out.

```rust
# #[cfg(feature = "geo")] {
use geo_types::{Geometry, LineString};
use map_tile_toolkit::SlicerAll;

let mut slicer = SlicerAll::new(25, 0)?;
let geom = Geometry::LineString(LineString::from(vec![(5, 5), (20, 20), (60, 40)]));

slicer.add_geometry(&geom)?;
for (tile, piece) in slicer.iter_geometries() {
    let _ = (tile, piece); // piece: LineString or MultiLineString in the tile's local frame
}
# }
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
