# map-tile-toolkit

[![GitHub repo](https://img.shields.io/badge/github-nyurik/map--tile--toolkit-8da0cb?logo=github)](https://github.com/nyurik/map-tile-toolkit)
[![crates.io version](https://img.shields.io/crates/v/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![crate usage](https://img.shields.io/crates/d/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![docs.rs status](https://img.shields.io/docsrs/map-tile-toolkit)](https://docs.rs/map-tile-toolkit)
[![crates.io license](https://img.shields.io/crates/l/map-tile-toolkit)](https://github.com/nyurik/map-tile-toolkit/blob/main/LICENSE-APACHE)
[![CI build status](https://github.com/nyurik/map-tile-toolkit/actions/workflows/ci.yml/badge.svg)](https://github.com/nyurik/map-tile-toolkit/actions)
[![Codecov](https://img.shields.io/codecov/c/github/nyurik/map-tile-toolkit)](https://app.codecov.io/gh/nyurik/map-tile-toolkit)

Clip integer **polylines** (`LineString`) into per-tile pieces on an integer tile
grid, keeping the geometry's **original vertices** — every vertex inside a tile, plus the first one
just outside wherever the line crosses an edge — instead of cutting new vertices at the boundary.

## Usage

`SlicerAll` accumulates every tile a polyline touches, even if there are no vertices in that tile. `SlicerOne` accumulates one fixed tile. Add
each polyline as an independent **feature**, then read the pieces back through borrowed iterators —
never owned `Vec`s. Nothing panics: bad input returns a `SliceError`.

```rust
use geo_types::Coord;
use map_tile_toolkit::SlicerAll;

let line = [
    Coord { x: 5, y: 5 },
    Coord { x: 20, y: 20 },
    Coord { x: 60, y: 40 },
];

// `extent = 25` → 25-unit tiles; `buffer = 0` = tight clip box (must be < extent / 2).
let mut slicer = SlicerAll::new(25, 0)?;
slicer.add_feature(&line)?;

// tiles → features → polylines, each polyline in that tile's local frame. A feature can yield
// several polylines in one tile (the line left and re-entered).
for tile in slicer.iter_tiles() {
    for feature in tile.iter_features() {
        for polyline in feature.iter_polylines() {
            let _ = (tile.id(), polyline); // polyline: &[Coord<i32>]
        }
    }
}
# Ok::<(), map_tile_toolkit::SliceError>(())
```

`SlicerOne::new(extent, buffer, tile)` clips to one tile and skips the tile level (`iter_features`
directly); it yields exactly what `SlicerAll` yields for that tile. `add_feature` is atomic — a
failed polyline leaves the accumulator unchanged — and `clear()` resets it for reuse.

### Coordinate space

The slicer is integer-only and dimensionless: `extent` is both the tile side and its output
resolution (the vector-tile `extent`). A vertex `x` belongs to tile `x.div_euclid(extent)` and is
emitted at `x − tile·extent ∈ [0, extent)`; add `tile · extent` to recover global coordinates. Do
all float/projection work up front (e.g. with [`geo`](https://docs.rs/geo)). For web-mercator at zoom
`z`: project, simplify, then apply one affine — scale `s = 2^z · extent / circumference`, translate
the top-left corner to the origin, flip `y`, round to `i32` — landing data in `[0, 2^z · extent)`.
`buffer` and `merge`'s `extent` are in these same units.

### Merging tiles back

`merge` is the stateless inverse of slicing: pass two `(tile, runs)` pairs at the same `extent` and it
stitches shared-border duplicates back into connected runs, in a frame anchored at the lower-left
tile. Non-adjacent tiles stay disconnected until a connecting tile is merged in; fold across many
tiles with that min tile as the running anchor.

```rust
use geo_types::Coord;
use map_tile_toolkit::{SlicerAll, TileId, merge};

let mut slicer = SlicerAll::new(25, 0)?;
slicer.add_feature([Coord { x: 5, y: 5 }, Coord { x: 60, y: 40 }])?;

let tiles: Vec<(TileId, Vec<&[Coord<i32>]>)> = slicer
    .iter_tiles()
    .map(|t| (t.id(), t.iter_features().flat_map(|f| f.iter_polylines()).collect()))
    .collect();
if let [(ta, ra), (tb, rb), ..] = tiles.as_slice() {
    let _merged = merge(slicer.extent(), (*ta, ra.as_slice()), (*tb, rb.as_slice()))?;
}
# Ok::<(), map_tile_toolkit::SliceError>(())
```

### Payloads and `geo-types`

The slicers are generic over a `Vertex` (default `Coord<i32>`); `Measured<M>` carries any
`Copy + PartialEq` payload (an M value, an id) that rides through slicing and merging **unchanged** —
nothing is interpolated, since no new vertices are cut. With the default `geo` feature, `add_geometry`
/ `iter_geometries` bridge `Geometry<i32>` in and out (each input line becomes an independent feature).

```rust
# #[cfg(feature = "geo")] {
use geo_types::{Geometry, LineString};
use map_tile_toolkit::SlicerAll;

let mut slicer = SlicerAll::new(25, 0)?;
slicer.add_geometry(&Geometry::LineString(LineString::from(vec![(5, 5), (20, 20), (60, 40)])))?;
for (tile, piece) in slicer.iter_geometries() {
    let _ = (tile, piece); // piece: LineString or MultiLineString in the tile's local frame
}
# }
# Ok::<(), map_tile_toolkit::SliceError>(())
```

## Development

This project uses [just](https://github.com/casey/just#readme) (`cargo install just`); run `just` for
the command list and `just test` to test. Tests are data-driven: each `tests/fixtures/*.geojson`
polyline is sliced by both paths (asserted byte-identical) and snapshotted as a `.geojson`
`FeatureCollection` (original line plus every per-tile piece) that renders on a map. Run `just bless`
to regenerate snapshots. To inspect them, load `tests/fixtures/grid.geojson` (the tile grid, offset
0.5px to sit between integer coordinates) and the `tests/snapshots/*.geojson` files in QGIS or any
`GeoJSON` viewer.

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
