# map-tile-toolkit

[![GitHub repo](https://img.shields.io/badge/github-nyurik/map--tile--toolkit-8da0cb?logo=github)](https://github.com/nyurik/map-tile-toolkit)
[![crates.io version](https://img.shields.io/crates/v/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![crate usage](https://img.shields.io/crates/d/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![docs.rs status](https://img.shields.io/docsrs/map-tile-toolkit)](https://docs.rs/map-tile-toolkit)
[![crates.io license](https://img.shields.io/crates/l/map-tile-toolkit)](https://github.com/nyurik/map-tile-toolkit/blob/main/LICENSE-APACHE)
[![CI build status](https://github.com/nyurik/map-tile-toolkit/actions/workflows/ci.yml/badge.svg)](https://github.com/nyurik/map-tile-toolkit/actions)
[![Codecov](https://img.shields.io/codecov/c/github/nyurik/map-tile-toolkit)](https://app.codecov.io/gh/nyurik/map-tile-toolkit)

Convert `geo` geometries into per-tile "slices" — pieces clipped to a tile (plus a buffer),
snapped to the integer tile grid, and ready to hand straight to an MVT encoder.

## Usage

Slice a Web Mercator (EPSG:3857) geometry into every tile it touches at a zoom. Each result
is a `geo_types::Geometry<i32>` in tile-local `0..extent` coordinates:

```rust
use std::num::NonZeroU32;
use geo_types::{Geometry, LineString, Polygon};
use map_tile_toolkit::{slice_all_tiles, slice_tile, SliceOptions, TileId};

// A polygon in Web Mercator meters.
let poly = Geometry::Polygon(Polygon::new(
    LineString::from(vec![
        (-1e6, -1e6), (1e6, -1e6), (1e6, 1e6), (-1e6, 1e6), (-1e6, -1e6),
    ]),
    vec![],
));
let opts = SliceOptions::new(NonZeroU32::new(4096).unwrap(), 64); // extent 4096, 64px buffer

// Batch: every tile the geometry touches at zoom 4.
for (tile, tile_geom) in slice_all_tiles(&poly, 4, opts) {
    // `tile_geom` is a Geometry<i32> in 0..4096 tile-local coords, ready for MVT encoding.
    let _ = (tile, tile_geom);
}

// Single tile (tile-server style): clip to one specific tile, or `None` if it's empty there.
if let Some(tile_geom) = slice_tile(&poly, TileId::new(8, 8, 4), opts) {
    let _ = tile_geom;
}
```

For batch/whole-tileset generation there is also an eager stripe slicer in the [`stripe`]
module (planetiler/geojson-vt style: interior fill detection, antimeridian wrapping), with
tile-bounds filtering in [`extents`] and geometry helpers in [`geo_utils`].

[`stripe`]: https://docs.rs/map-tile-toolkit/latest/map_tile_toolkit/stripe/
[`extents`]: https://docs.rs/map-tile-toolkit/latest/map_tile_toolkit/extents/
[`geo_utils`]: https://docs.rs/map-tile-toolkit/latest/map_tile_toolkit/geo_utils/

## Development

* This project is easier to develop with [just](https://github.com/casey/just#readme), a modern alternative to `make`.
  Install it with `cargo install just`.
* To get a list of available commands, run `just`.
* To run tests, use `just test`.

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
