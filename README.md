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

A line vertex is kept when one of its two segments touches the tile's buffered box, and a run breaks
only where a vertex is dropped. So a single segment outside the tile is kept, and only a longer
excursion splits the line into several runs:

![Which line vertices a tile keeps](https://raw.githubusercontent.com/nyurik/map-tile-toolkit/main/docs/images/clip-lines.svg)

## Usage

* `Slicer` takes **one geometry** at a time — a line, a point or a polygon — and returns a lazy
  iterator over its slices: `line` / `point` / `polygon` for every tile the geometry reaches (even
  tiles holding none of its vertices), `tile_line` / `tile_point` / `tile_polygon` for one given tile.
* Vertices may carry a payload (generic `Vertex`); feature-level data stays with the caller, who
  pairs it with each slice of the feature being sliced.
* `ClipMode::Unclipped` (`Slicer::new(..)?.with_clip_mode(..)`) gives every tile a line or polygon
  reaches the whole geometry instead, moved into that tile's local frame (see [Unclipped](#unclipped)).
* `Mosaic` assembles tiles back into features, joining features that match on both sides of the edge, and rejecting inconsistent tiles. Mosaic does not require all original tiles to be added.

Slicing is lazy: each slice is computed only when the iterator reaches it, so stopping early stops
the work. A line run is never copied — it borrows the caller's vertices, so a line with millions of
vertices costs no extra memory — and a polygon piece is built in the slicer's reused working memory.
Slices come in no particular order: a tile can get several slices of one line (where it left and
re-entered the tile). Nothing panics: bad input yields a `TileError`.

```rust
use geo_types::coord;
use map_tile_toolkit::{Slicer, TileError};

fn main() -> Result<(), TileError> {
    let line = [
        coord!{ x: 5, y: 5 },
        coord!{ x: 20, y: 20 },
        coord!{ x: 60, y: 40 },
    ];

    // `extent = 25` → 25-unit tiles;
    // `buffer = 0` = tight clip box (must be < extent / 2).
    let slicer = Slicer::new(25, 0)?;

    // One slice per run, each in its tile's local frame.
    for slice in slicer.line(&line)? {
        for vertex in slice.run() {
            let _ = (slice.tile(), vertex); // vertex: Coord<i32>
        }
    }

    Ok(())
}
```

`slicer.tile_line(tile, &line)` clips to one tile and yields exactly the slices `line` yields for
that tile.

### Multi-geometries

The slicer has no notion of a multi-geometry. To slice a `MultiLineString`, `MultiPoint` or
`MultiPolygon`, call it **once per element** (`line` for each line, `point` for each point, `polygon`
for each polygon). The caller has to manage the association between a feature's attributes and each
of its slices anyway, tile by tile; doing that for the elements of one multi-feature is no extra
work compared with doing it for several features, and it keeps the slicer free of per-element
indices, and of any state that grows with the feature. Slices of several elements that land in one
tile are simply several slices in that tile, which the caller groups (the runs of a `MultiLineString`
into one geometry in that tile, say).

### Points

`slicer.point(p)` yields the point in every tile whose buffered box holds it: its own tile, plus up
to three neighbors when it lies within `buffer` of an edge (a point at a corner of four tiles lands
in all four). Each item is the tile and the point in that tile's local frame, payload unchanged;
the iterator knows its length up front (one to four items). `slicer.tile_point(tile, p)` asks one
tile and returns the local point, if the tile holds it.

```rust
use geo_types::coord;
use map_tile_toolkit::{Slicer, TileError, TileId};

fn main() -> Result<(), TileError> {
    let slicer = Slicer::new(256, 8)?;

    // One unit past the corner of tile (1, 1): within the buffer of three neighbors as well.
    let point = coord! { x: 257, y: 256 };
    let slices = slicer.point(point)?;
    assert_eq!(slices.len(), 4);
    for slice in slices {
        let _ = (slice.tile(), slice.point()); // TileId, and the point as Coord<i32> in that tile
    }

    assert_eq!(
        slicer.tile_point(TileId::new(1, 1), point)?,
        Some(coord! { x: 1, y: 0 })
    );
    assert_eq!(slicer.tile_point(TileId::new(5, 5), point)?, None);
    Ok(())
}
```

### Coordinate space

The slicer is integer-only and dimensionless: `extent` is both the tile side and its output
resolution (the vector-tile `extent`). A vertex `x` belongs to tile `x.div_euclid(extent)`, whose own
area is `[0, extent)` in its local frame `x − tile·extent`; slices come in that frame, so vertices in
the buffer, a line's first vertex past an edge, and a polygon's synthetic corners lie outside it. Add
`tile · extent` to recover global coordinates. Do
all float/projection work up front (e.g. with [`geo`](https://docs.rs/geo)). For web-mercator at zoom
`z`: project, simplify, then apply one affine — scale `s = 2^z · extent / circumference`, translate
the top-left corner to the origin, flip `y`, round to `i32` — landing data in `[0, 2^z · extent)`.
`buffer` and `Mosaic`'s `extent` are in these same units.

### Limits

Routing walks each segment through exactly the tiles it touches, so cost follows those tiles, never
a segment's bounding box: a world-spanning diagonal at z14 (extent 4096) touches ~35 000 tiles. Rows
a segment crosses between two unbuffered tiles (`buffer == 0`) are skipped at once. What remains,
each reported as a `TileError` rather than a panic:

* **Coordinates are `i32`**, so `2^z · extent` must fit (up to z18 at extent 4096); geometry touching
  a tile whose buffered box leaves the `i32` range (an outermost tile, with `buffer > 0`) is
  `Overflow`. Every slicing call reads the geometry once, to bound it, and returns the error from
  the call, so the iterators cannot fail midway. Bounding is per geometry, so a line or polygon
  spanning more than `2^31` units on one axis is rejected whole.
* **`u32` indexing** (`GeometryTooLarge`) — at most `u32::MAX` rings or edges per polygon, from the
  call, and `u32::MAX` vertices per piece, the one error a polygon iterator can still yield.

There is no budget on how many tiles a geometry may reach: slicing is lazy, so memory stays bounded
however far it reaches, and the work is only what the caller pulls.

### Merging tiles back

`Mosaic` is the stateful inverse of `Slicer::line`: add tiles (each tile's runs, in its local frame) and
it reassembles whole features across borders in the global frame. It rejects a tile that is
inconsistent with those already added — a shared segment that disagrees (coordinates or payload), or
a tile that owns an endpoint's cell yet fails to carry its edge (e.g. a line spanning into a neighbor
the neighbor never corroborates) — naming the conflicting tile ids and leaving the mosaic unchanged.
 `purge` drops a tile; `iter_features` walks every
reassembled feature.

```rust
use std::collections::BTreeMap;

use geo_types::coord;
use map_tile_toolkit::{Mosaic, Slicer, TileError};

fn main() -> Result<(), TileError> {
    let slicer = Slicer::new(25, 0)?;
    let line = [coord!{ x: 5, y: 5 }, coord!{ x: 60, y: 40 }];

    // Gather each tile's runs, then hand each tile to the mosaic.
    let mut tiles = BTreeMap::<_, Vec<_>>::new();
    for slice in slicer.line(&line)? {
        tiles.entry(slice.tile()).or_default().push(slice.run().to_vec());
    }
    let mut mosaic = Mosaic::new(slicer.extent())?;
    for (tile, runs) in &tiles {
        mosaic.add(*tile, runs).expect("consistent tiles never conflict");
    }
    for feature in mosaic.iter_features() {
        let _ = feature; // Vec<Coord<i32>> in global coordinates
    }
    Ok(())
}
```

### Polygons

`slicer.polygon(&rings)` slices a **polygon** — its rings, exterior first — into every tile it
reaches, and `slicer.tile_polygon(tile, &rings)` into one tile (its piece there, if any). A
multipolygon is sliced one polygon at a time.
Original vertices are kept; where a ring leaves the tile, the gap is bridged by synthetic corners
just outside the buffered box, so each ring stays closed and correctly wound but is valid only inside
the box — fine for renderers that clip to the tile (MVT), not for consumers that need OGC-valid
output. `PolygonMosaic` reassembles the tiles, dropping the synthetic parts.

A ring keeps every vertex with an edge touching the box. Each dropped excursion is closed past the
box, by the corners it winds around (a straight chord when it leaves and returns on the same side),
unless it has no more vertices than that detour has corners, and is kept as it is. A tile the polygon covers gets the box's four corners,
and a tile inside a hole gets nothing. Each ring is clipped on its own, so a hole that crosses the
tile keeps its own outside vertices even where the exterior's far part was dropped: past the box, the
hole can reach outside the exterior's chord or detour. That is why a piece is valid only inside the
tile's box:

![Which polygon vertices a tile keeps](https://raw.githubusercontent.com/nyurik/map-tile-toolkit/main/docs/images/clip-polygons.svg)

The polygon is sliced row by row, and nothing outside the current tile row is kept: the call reads
the rings once, to bound and index them, and the iterator holds one row plus a small index of the
polygon's edges by row (a polygon inside one tile skips even that, and is lent as it is). A tile gets
one **piece** — the exterior and the holes that reach the tile — built in one reused buffer, so the
iterator is *lending*: a piece is valid until the next `next()`, and the loop is `while let`. Each
ring is closed, its first vertex repeated at the end (drop it for an MVT `ClosePath`). Polygons need
`&mut Slicer` for that buffer; lines and points need only `&Slicer`.

Tiles a polygon covers entirely are never materialized: they come as `FillRun { y, x }` row
spans, so a polygon covering 10⁸ tiles costs one run per row, and time and memory grow with its
perimeter rather than its area. `slices.fill_ring(&run)` (or `slicer.fill_ring(&run)`, once the
iterator is done) is the one ring `tile_polygon` yields for that polygon in each of the run's tiles —
five tile-local vertices, a box just outside the buffered tile, wound like the polygon — so expanding
the runs reproduces it exactly.

Slicing preserves each ring's winding. `signed_area_2x` gives a ring's exact orientation, so
normalize it once per feature if the output needs a fixed convention. MVT needs exteriors with a
positive area in its y-down tile coordinates, and holes with a negative one.

```rust
use geo_types::coord;
use map_tile_toolkit::{PolygonSlice, Slicer, TileError, signed_area_2x};

fn main() -> Result<(), TileError> {
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

    let mut slicer = Slicer::new(25, 2)?;
    // A polygon is its rings, exterior first.
    let rings = [exterior, hole];
    let mut slices = slicer.polygon(&rings)?;
    while let Some(slice) = slices.next() {
        match slice? {
            // The polygon in one tile: its rings, exterior first, each closed and tile-local.
            PolygonSlice::Piece(piece) => {
                for ring in piece.rings() {
                    let _ = (piece.tile(), ring.to_vec());
                }
            }
            // Tiles (x, y) for x in run.x, entirely inside the polygon: each holds it as the
            // single ring `slices.fill_ring(&run)`.
            PolygonSlice::Run(run) => {
                let _ = (run.y, &run.x, slices.fill_ring(&run));
            }
        }
    }
    Ok(())
}
```

One slicer per worker can be reused across polygons: it keeps its working memory's capacity, so it
stops allocating once warmed up, and `shrink_to_fit()` releases what an outlier polygon left behind.

### Unclipped

A slicer built `with_clip_mode(ClipMode::Unclipped)` does not clip: each tile a line or polygon
reaches — exactly the tiles the default `ClipMode::Clipped` gives anything — gets the **whole**
geometry, once, in that tile's local frame, for renderers that clip to the tile themselves. The calls
and items are the same, so one consumer loop serves both modes: a line comes as one `LineSlice` per
tile whose run is all of the line, and a polygon as one `PolygonPiece` per tile with all of its rings
— in the tiles it covers entirely too, so no `FillRun` is yielded. Nothing is copied or built: the
slices borrow the input, so the cost is finding the tiles (plus, for a line, a set of the tiles found
so far, to give each just once: memory proportional to their number, and on a long line crossing
mostly new tiles, several times a clipped walk's instructions). The consumer's cost is the whole
geometry per tile, so a polygon covering many tiles costs it that many times its vertices. Points are never clipped, so the mode does not change them.

```rust
use geo_types::coord;
use map_tile_toolkit::{ClipMode, Slicer, TileError, TileId};

fn main() -> Result<(), TileError> {
    let slicer = Slicer::new(25, 0)?.with_clip_mode(ClipMode::Unclipped);
    let line = [coord! { x: 5, y: 5 }, coord! { x: 60, y: 10 }];

    // Tiles (0, 0), (1, 0) and (2, 0), each holding the whole line in its own frame.
    let mut tiles: Vec<_> = slicer
        .line(&line)?
        .map(|slice| (slice.tile(), slice.run().to_vec()))
        .collect();
    tiles.sort_unstable_by_key(|(tile, _)| *tile);
    assert_eq!(tiles.len(), 3);
    assert_eq!(
        tiles[1],
        (TileId::new(1, 0), vec![coord! { x: -20, y: 5 }, coord! { x: 35, y: 10 }])
    );
    Ok(())
}
```

### Payloads

Data rides along on two independent axes: a **per-vertex** payload (the `Vertex` type), and
**per-feature** attributes, which the caller pairs with each slice. Use either, both, or neither.

#### Per-vertex payload

The slicer is generic over a `Vertex` (default `Coord<i32>`); `Measured<M>` pairs a position with any
`Copy + PartialEq` payload (an M value, an id) that rides through slicing and merging **unchanged** —
nothing is interpolated, since no new vertices are cut.

```rust
use map_tile_toolkit::{Measured, Slicer, TileError};

fn main() -> Result<(), TileError> {
    // Each vertex carries a payload (here an id); it survives slicing untouched.
    let slicer = Slicer::new(25, 0)?;
    let line = [
        Measured::new(5, 5, 100),
        Measured::new(20, 20, 200),
        Measured::new(60, 40, 300),
    ];
    for slice in slicer.line(&line)? {
        let _ = slice.run().iter().map(|v| (v.position, v.m)).collect::<Vec<_>>();
    }
    Ok(())
}
```

#### Per-feature attributes (MVT-style)

Data that belongs to a whole feature — an optional id, a key/value property map — is *not*
per-vertex. The slicer takes one geometry at a time, so the caller pairs every slice with the data
of the feature it came from: no side table, no per-vertex handles, nothing for the slicer to carry.
For a multi-feature that means the same loop over its elements (see [Multi-geometries](#multi-geometries)).

```rust
use geo_types::coord;
use map_tile_toolkit::{Slicer, TileError};

struct Attrs {
    id: Option<u64>,
    name: &'static str
}

fn main() -> Result<(), TileError> {
    let slicer = Slicer::new(25, 0)?;
    let attrs = Attrs { id: Some(1), name: "Main St" };
    let line = [coord! { x: 5, y: 5 }, coord! { x: 60, y: 40 }];
    for slice in slicer.line(&line)? {
        // Every slice of this feature goes with `attrs`, in whichever tile it lands.
        let _ = (slice.tile(), attrs.id, attrs.name);
    }
    Ok(())
}
```

A line that is split comes out in pieces: within one tile, each run is its own slice (group
them by tile to render a `MultiLineString`); across tiles, the feature's attributes go with each
tile's piece. `Slicer<Measured<M>>` carries per-vertex M alongside. See [`examples/mvt_features.rs`](examples/mvt_features.rs) for the
full round-trip emitting per-tile `GeoJSON` with ids and properties preserved.

## Development

This project uses [just](https://github.com/casey/just#readme) (`cargo install just`); run `just` for
the command list and `just test` to test. Tests are data-driven: each `tests/polylines/fixtures/*.geojson`
polyline is sliced by both paths (asserted byte-identical) and snapshotted as a `.geojson`
`FeatureCollection` (original line plus every per-tile piece) that renders on a map. Polygon fixtures
in `tests/polygons/fixtures/` are snapshotted per tile (`snapshots*/`); `tile_polygon` and
`polygon` (its fill runs expanded into fill rings) must produce identical output, also checked on
finer grids and under every rotation and mirror image. Point fixtures in `tests/points/fixtures/` are
snapshotted the same way; `point`, `tile_point`, and the closed buffered box rule must agree. The
fixtures' multi-geometries are sliced one element at a time, as a caller would. Every line and polygon
fixture is also sliced unclipped (`tests/unclipped.rs`): both paths must give each tile the clipped
slicer reaches the whole geometry, and nothing elsewhere; its snapshots (`snapshots-unclipped*/`)
show the input and the boxes of the tiles it reaches. Run `just bless`
to regenerate snapshots. To inspect them, run `just visualize-tests` and open `tests/visualize.qgz` in QGIS (every
fixture and snapshot as a layer, over `tests/grid.geojson`, the tile grid offset 0.5px to sit between integer
coordinates), or load the `.geojson` files in any `GeoJSON` viewer.

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
