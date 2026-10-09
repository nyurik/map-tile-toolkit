# map-tile-toolkit

[![GitHub repo](https://img.shields.io/badge/github-nyurik/map--tile--toolkit-8da0cb?logo=github)](https://github.com/nyurik/map-tile-toolkit)
[![crates.io version](https://img.shields.io/crates/v/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![crate usage](https://img.shields.io/crates/d/map-tile-toolkit)](https://crates.io/crates/map-tile-toolkit)
[![docs.rs status](https://img.shields.io/docsrs/map-tile-toolkit)](https://docs.rs/map-tile-toolkit)
[![crates.io license](https://img.shields.io/crates/l/map-tile-toolkit)](https://github.com/nyurik/map-tile-toolkit/blob/main/LICENSE-APACHE)
[![CI build status](https://github.com/nyurik/map-tile-toolkit/actions/workflows/ci.yml/badge.svg)](https://github.com/nyurik/map-tile-toolkit/actions)
[![Codecov](https://img.shields.io/codecov/c/github/nyurik/map-tile-toolkit)](https://app.codecov.io/gh/nyurik/map-tile-toolkit)

Clip integer **polylines** and **polygons** into per-tile pieces on an integer tile
grid, keeping the geometry's original vertices. No new vertexes are ever created on a line. The result has every vertex inside a tile, plus the first vertex just outside wherever the line crosses an edge. The tile may optionally include a buffer of `[0..extent/2)` size, which increases the number of vertices shared between neighboring tiles. Polygon rings are closed with synthetic corners strictly outside the buffered tile (see [Polygons](#polygons)).

## Usage

* Each feature may contain feature-level and vertex-level attributes (generic).
* `SlicerAll` accumulates every tile a polyline touches, even if there are no vertices in that tile.
* `SlicerOne` accumulates one fixed tile.
* `Mosaic` assembles tiles back into features, joining features that match on both sides of the edge, and rejecting inconsistent tiles. Mosaic does not require all original tiles to be added.

Add each **feature** (polyline), then read the pieces back through borrowed iterators — never owned `Vec`s. Nothing panics: bad input returns a `TileError`.

```rust
use geo_types::coord;
use map_tile_toolkit::{SlicerAll, TileError};

fn example() -> Result<(), TileError> {
    let line = [
        coord!{ x: 5, y: 5 },
        coord!{ x: 20, y: 20 },
        coord!{ x: 60, y: 40 },
    ];

    // `extent = 25` → 25-unit tiles;
    // `buffer = 0` = tight clip box (must be < extent / 2).
    let mut slicer = SlicerAll::new(25, 0)?;
    slicer.add_feature(&line)?;

    // tiles → features → polylines, each polyline in that tile's
    // local frame. A feature can yield several polylines
    // in one tile (the line left and re-entered).
    for tile in slicer.iter_tiles() {
        for feature in tile.iter_features() {
            for polyline in feature.iter_polylines() {
                // polyline: &[Coord<i32>]
                let _ = (tile.tile_id(), polyline);
            }
        }
    }

    Ok(())
}
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
`buffer` and `Mosaic`'s `extent` are in these same units.

### Merging tiles back

`Mosaic` is the stateful inverse of `SlicerAll`: add tiles (each tile's runs, in its local frame) and
it reassembles whole features across borders in the global frame. It rejects a tile that is
inconsistent with those already added — a shared segment that disagrees (coordinates or payload), or
a tile that owns an endpoint's cell yet fails to carry its edge (e.g. a line spanning into a neighbor
the neighbor never corroborates) — naming the conflicting tile ids and leaving the mosaic unchanged.
 `purge` drops a tile; `iter_features` walks every
reassembled feature.

```rust
use geo_types::coord;
use map_tile_toolkit::{Mosaic, SlicerAll, TileError};

fn example() -> Result<(), TileError> {
    let mut slicer = SlicerAll::new(25, 0)?;
    slicer.add_feature([
        coord!{ x: 5, y: 5 },
        coord!{ x: 60, y: 40 }
    ])?;

    let mut mosaic = Mosaic::new(slicer.extent())?;
    for tile in slicer.iter_tiles() {
        let runs: Vec<_> = tile.iter_features().flat_map(|f| f.iter_polylines()).collect();
        mosaic.add(tile.tile_id(), &runs).expect("consistent tiles never conflict");
    }
    for feature in mosaic.iter_features() {
        let _ = feature; // Vec<Coord<i32>> in global coordinates
    }
    Ok(())
}
```

### Polygons

`PolygonSlicerOne::new(extent, buffer, tile)` clips polygons (an exterior ring plus holes) to one
tile. `PolygonSlicerAll::new(extent, buffer)` slices whole **multipolygon** features into every tile
they reach, and yields for each tile exactly what `PolygonSlicerOne` yields there. Original vertices
are kept; where a ring leaves the tile, the gap is bridged by synthetic corners just outside the
buffered box, so each ring stays closed and correctly wound but is valid only inside the box — fine
for renderers that clip to the tile (MVT), not for consumers that need OGC-valid output.
`PolygonMosaic` reassembles the tiles, dropping the synthetic parts.

Tiles a feature covers entirely are never materialized: `PolygonSlicerAll` reports them as
`FillRun { y, x: Range<i32> }` row spans, so a polygon covering 10⁸ tiles costs one run per row, and
time and memory grow with its perimeter rather than its area.

Slicing preserves each ring's winding. `signed_area_2x` gives a ring's exact orientation, so
normalize it once per feature if the output needs a fixed convention. MVT needs exteriors with a
positive area in its y-down tile coordinates, and holes with a negative one.

```rust
use geo_types::coord;
use map_tile_toolkit::{PolygonSlicerAll, TileError, signed_area_2x};

fn example() -> Result<(), TileError> {
    let mut exterior = vec![
        coord! { x: 5, y: 5 }, coord! { x: 120, y: 5 },
        coord! { x: 120, y: 120 }, coord! { x: 5, y: 120 },
    ];
    let mut hole = vec![
        coord! { x: 40, y: 40 }, coord! { x: 40, y: 60 },
        coord! { x: 60, y: 60 }, coord! { x: 60, y: 40 },
    ];
    // MVT winding: exterior positive, holes negative.
    if signed_area_2x(&exterior) < 0 { exterior.reverse(); }
    if signed_area_2x(&hole) > 0 { hole.reverse(); }

    let mut slicer = PolygonSlicerAll::new(25, 2)?;
    // One feature is a multipolygon: each polygon is its rings, exterior first.
    slicer.add_feature([[exterior.as_slice(), hole.as_slice()]])?;
    for feature in slicer.iter_features() {
        // Tiles a ring edge touches: tile → polygons → rings
        // (each ring closed, in the tile's local frame).
        for tile in feature.iter_tiles() {
            for polygon in tile.iter_polygons() {
                for ring in polygon.iter_rings() {
                    let _ = (tile.tile_id(), ring.is_hole(), ring.vertices());
                }
            }
        }
        // Tiles entirely inside the feature, as row spans.
        for run in feature.iter_fill_runs() {
            let _ = (run.y, run.x); // tiles (x, y) for x in run.x
        }
    }
    Ok(())
}
```

A per-worker slicer can be reused across features: `clear()` keeps every buffer's capacity, so it
stops allocating once warmed up, and `shrink_to_fit()` releases what an outlier feature left behind.
`add_feature` is atomic, like the polyline slicers.

### Payloads

The slicers carry data on two independent axes: a **per-vertex** payload (the `Vertex` type) and a
**per-feature** attribute (the `A` type). Use either, both, or neither.

#### Per-vertex payload

The slicers are generic over a `Vertex` (default `Coord<i32>`); `Measured<M>` pairs a position with any
`Copy + PartialEq` payload (an M value, an id) that rides through slicing and merging **unchanged** —
nothing is interpolated, since no new vertices are cut.

```rust
use map_tile_toolkit::{Measured, SlicerAll, TileError};

fn example() -> Result<(), TileError> {
    // Each vertex carries a payload (here an id); it survives slicing untouched.
    let mut slicer = SlicerAll::new(25, 0)?;
    slicer.add_feature([
        Measured::new(5, 5, 100),
        Measured::new(20, 20, 200),
        Measured::new(60, 40, 300),
    ])?;
    for tile in slicer.iter_tiles() {
        for feature in tile.iter_features() {
            for run in feature.iter_polylines() {
                let _ = run.iter().map(|v| (v.position, v.m)).collect::<Vec<_>>();
            }
        }
    }
    Ok(())
}
```

#### Per-feature attributes (MVT-style)

Data that belongs to a whole feature — an optional id, a key/value property map — is *not*
per-vertex, so it rides on the slicer's second generic axis `A` (default `()`, a zero-sized type
that costs nothing) rather than in the vertex payload. Attach it with `add_feature_with(line, attr)`
and read it back per tile-piece with `feature.attr()` — no side table, no per-vertex handles. `A`
only needs to be `Clone`.

```rust
use geo_types::{Coord, coord};
use map_tile_toolkit::{SlicerAll, TileError};

#[derive(Clone)]
struct Attrs {
    id: Option<u64>,
    name: &'static str
}

fn example() -> Result<(), TileError> {
    let mut slicer = SlicerAll::new(25, 0)?;
    slicer.add_feature_with(
        [coord! { x: 5, y: 5 }, coord! { x: 60, y: 40 }],
        Attrs { id: Some(1), name: "Main St" },
    )?;
    for tile in slicer.iter_tiles() {
        for feature in tile.iter_features() {
            let attrs = feature.attr(); // &Attrs, duplicated onto every tile the feature touches
            let _ = (attrs.id, attrs.name);
        }
    }
    Ok(())
}
```

A feature that is split comes out two ways — within one tile its runs stay grouped as a single
feature (render a `MultiLineString`); across tiles its attributes are duplicated onto each tile's
piece. The two axes are orthogonal, so `SlicerAll<Measured<M>, Attrs>` carries per-vertex M *and*
per-feature attributes at once. See [`examples/mvt_features.rs`](examples/mvt_features.rs) for the
full round-trip emitting per-tile `GeoJSON` with ids and properties preserved.

## Development

This project uses [just](https://github.com/casey/just#readme) (`cargo install just`); run `just` for
the command list and `just test` to test. Tests are data-driven: each `tests/polylines/fixtures/*.geojson`
polyline is sliced by both paths (asserted byte-identical) and snapshotted as a `.geojson`
`FeatureCollection` (original line plus every per-tile piece) that renders on a map. Polygon fixtures
in `tests/polygons/fixtures/` are snapshotted per tile by `PolygonSlicerOne` (`snapshots*/`) and by
`PolygonSlicerAll` (`snapshots-all*/`, with fill runs drawn as green rectangles), and every
`PolygonSlicerAll` tile is checked against `PolygonSlicerOne`. Run `just bless`
to regenerate snapshots. To inspect them, load `tests/polylines/fixtures/grid.geojson` (the tile grid, offset
0.5px to sit between integer coordinates) and the `tests/polylines/snapshots/*.geojson` files in QGIS or any
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
