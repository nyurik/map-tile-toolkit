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

```rust
use geo_types::{Geometry, LineString};
use map_tile_toolkit::{slice_all_tiles, slice_tile, TileId};

// An integer polyline. With a tile size of 25, tiles are 25 units wide.
let line = Geometry::LineString(LineString::from(vec![(5, 5), (20, 20), (60, 40)]));
let tile_size = 25;

// Batch: every tile the polyline touches, each piece in the input's coordinate space.
for (tile, piece) in slice_all_tiles(&line, tile_size) {
    let _ = (tile, piece);
}

// Single tile: clip to one tile, or `None` when the line does not touch it.
if let Some(piece) = slice_tile(&line, TileId::new(0, 0), tile_size) {
    let _ = piece;
}
```

`slice_all_tiles` and `slice_tile` agree by construction: `slice_all_tiles(geom, size)[tile]`
equals `slice_tile(geom, tile, size)` for every tile the geometry touches.

## Development

* This project is easier to develop with [just](https://github.com/casey/just#readme), a modern alternative to `make`.
  Install it with `cargo install just`.
* To get a list of available commands, run `just`.
* To run tests, use `just test`.
* Tests are data-driven: each `tests/fixtures/inputs/*.geojson` polyline is sliced with both the
  batch and per-tile paths (asserted byte-identical) and snapshotted as a `.geojson`
  `FeatureCollection` (the original line plus every per-tile piece) that renders on a map.
  `tests/fixtures/grid.geojson` overlays the tile grid. Run `just bless` to regenerate snapshots.

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
