//! Fuzz the polygon slicer: it must **never panic**, and every feature it slices without error must
//! agree with the single-tile slicer, tile by tile.
//!
//! For a structured input (up to two multipolygon features of arbitrary — often self-intersecting or
//! overlapping — rings, and a slicer config):
//!
//! 1. **No panic, ever.** `Slicer::polygon` yields `Ok`/`Err` items; the fuzz build enables overflow
//!    checks, so unchecked arithmetic surfaces as a crash.
//! 2. For every feature sliced without error, over the whole reachable tile span (skipped when the
//!    span is too large to scan cheaply), every tile holds exactly what `Slicer::tile_polygon`
//!    yields there: its edge-tile pieces and its fill runs' fill rings, in polygon order. Each
//!    polygon lists a tile once, never as both an edge tile and a fill tile.
//!
//!    The second feature checks that features sliced one after another, by the same slicer, read
//!    back independently.
//! 3. Slicing the same features again reproduces the same output.
//!
//! Coordinates start as `i8`, so each run stays cheap, then are scaled by a power of two (with the
//! extent, so the tile layout keeps its shape) and moved by an offset, so the crossing and box math
//! also runs on large values and near the `i32` limits.

#![no_main]

use std::collections::BTreeMap;

use arbitrary::Arbitrary;
use geo_types::Coord;
use libfuzzer_sys::fuzz_target;
use map_tile_toolkit::{PolygonSlice, Slicer, TileId};

/// Cap on tiles scanned by the single-tile oracle per feature.
const SCAN_CAP: i64 = 2_048;

type Polygons = Vec<Vec<Vec<Coord<i32>>>>;

#[derive(Arbitrary, Debug)]
struct Input {
    extent: u8,
    buffer: u8,
    /// Coordinates and the extent are multiplied by `2^(scale % 24)`.
    scale: u8,
    /// Added to every coordinate after scaling.
    origin: (i32, i32),
    /// Up to two features, each a list of polygons, each a list of rings (exterior first).
    features: Vec<Vec<Vec<Vec<(i8, i8)>>>>,
}

/// One polygon's rings (exterior first), tile-local.
type Rings = Vec<Vec<Coord<i32>>>;

/// Per polygon, its rings.
type Pieces = Vec<Rings>;

/// One feature's output: what every tile holds, edge pieces and fill rings, in polygon order.
type Output = BTreeMap<TileId, Pieces>;

/// What the single-tile slicer yields for `tile`, or `None` if it errors (the tile itself, or a
/// corner or vertex, out of the `i32` range).
fn one_tile(slicer: &mut Slicer<Coord<i32>>, polygons: &Polygons, tile: TileId) -> Option<Pieces> {
    let mut out = Vec::new();
    for rings in polygons {
        if let Some(piece) = slicer.tile_polygon(tile, rings).ok()? {
            out.push(piece.rings().map(|r| r.to_vec()).collect());
        }
    }
    Some(out)
}

/// One multipolygon feature sliced polygon by polygon into every tile, checking that each polygon
/// lists a tile once; `None` if the slicer yields an error (a valid outcome near the `i32` limits).
fn output(slicer: &mut Slicer<Coord<i32>>, polygons: &Polygons) -> Option<Output> {
    let mut tiles: BTreeMap<TileId, Vec<(u32, Rings)>> = BTreeMap::new();
    for (polygon, rings) in (0_u32..).zip(polygons) {
        let mut slices = slicer.polygon(rings).ok()?;
        let mut runs = Vec::new();
        while let Some(slice) = slices.next() {
            match slice.ok()? {
                PolygonSlice::Piece(p) => {
                    let polys = tiles.entry(p.tile()).or_default();
                    assert!(
                        polys.iter().all(|(q, _)| *q != polygon),
                        "{:?} listed twice by polygon {polygon}",
                        p.tile(),
                    );
                    polys.push((polygon, p.rings().map(|r| r.to_vec()).collect()));
                }
                PolygonSlice::Run(run) => runs.push(run),
            }
        }
        for run in runs {
            assert!(!run.x.is_empty(), "empty fill run");
            let ring = slices.fill_ring(&run).to_vec();
            for x in run.x.clone() {
                let polys = tiles.entry(TileId::new(x, run.y)).or_default();
                assert!(
                    polys.iter().all(|(q, _)| *q != polygon),
                    "tile ({x}, {}) is both edge and fill, or filled twice, by polygon {polygon}",
                    run.y,
                );
                polys.push((polygon, vec![ring.clone()]));
            }
        }
    }
    Some(
        tiles
            .into_iter()
            .map(|(tile, mut polys)| {
                polys.sort_by_key(|(q, _)| *q);
                (tile, polys.into_iter().map(|(_, rings)| rings).collect())
            })
            .collect(),
    )
}

/// Scale and move one feature's coordinates, dropping vertices that leave `i32`.
fn place(polygons: &[Vec<Vec<(i8, i8)>>], scale: i64, origin: (i32, i32)) -> Polygons {
    let at = |v: i8, o: i32| i32::try_from(i64::from(v) * scale + i64::from(o)).ok();
    polygons
        .iter()
        .take(4)
        .map(|rings| {
            rings
                .iter()
                .take(4)
                .map(|ring| {
                    ring.iter()
                        .take(24)
                        .filter_map(|&(x, y)| {
                            Some(Coord {
                                x: at(x, origin.0)?,
                                y: at(y, origin.1)?,
                            })
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// Check one accepted feature's output against the single-tile slicer over its reachable span.
fn check(slicer: &mut Slicer<Coord<i32>>, polygons: &Polygons, tiles: &Output) {
    let (extent, buffer) = (slicer.extent(), slicer.buffer());
    let coords = polygons.iter().flatten().flatten();
    let (Some(min_x), Some(max_x)) = (
        coords.clone().map(|c| c.x).min(),
        coords.clone().map(|c| c.x).max(),
    ) else {
        return;
    };
    let (min_y, max_y) = (
        coords.clone().map(|c| c.y).min().expect("non-empty"),
        coords.map(|c| c.y).max().expect("non-empty"),
    );
    // Every piece is within one tile (plus buffer) of a vertex.
    let (e, b) = (i64::from(extent), i64::from(buffer));
    let tile = |v: i32, pad: i64| (i64::from(v) + pad).div_euclid(e);
    let (x0, x1) = (tile(min_x, -b) - 1, tile(max_x, b) + 1);
    let (y0, y1) = (tile(min_y, -b) - 1, tile(max_y, b) + 1);
    if (x1 - x0 + 1) * (y1 - y0 + 1) > SCAN_CAP {
        return;
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (Ok(tx), Ok(ty)) = (i32::try_from(x), i32::try_from(y)) else {
                continue;
            };
            let id = TileId::new(tx, ty);
            let Some(one) = one_tile(slicer, polygons, id) else {
                // A padding tile past the `i32` limits: the all-tiles slicer cannot have used it.
                assert!(!tiles.contains_key(&id), "{id:?} is out of range");
                continue;
            };
            let got = tiles.get(&id).cloned().unwrap_or_default();
            assert_eq!(
                got, one,
                "tile {id:?} disagrees with the single-tile slicer"
            );
        }
    }
}

fuzz_target!(|input: Input| {
    let scale = 1_i64 << (input.scale % 24);
    let Ok(extent) = u32::try_from(i64::from(input.extent) * scale) else {
        return;
    };
    let buffer = u16::from(input.buffer);
    let Ok(mut slicer) = Slicer::<Coord<i32>>::new(extent, buffer) else {
        return; // invalid config — rejected, nothing to test
    };
    let features: Vec<Polygons> = input
        .features
        .iter()
        .take(2)
        .map(|f| place(f, scale, input.origin))
        .collect();

    // One slicer for every feature, as a bulk tile generator uses it.
    let outputs: Vec<Option<Output>> = features.iter().map(|f| output(&mut slicer, f)).collect();
    let again: Vec<Option<Output>> = features.iter().map(|f| output(&mut slicer, f)).collect();
    assert_eq!(again, outputs, "slicing again changed the result");

    for (polygons, out) in features.iter().zip(&outputs) {
        if let Some(out) = out {
            check(&mut slicer, polygons, out);
        }
    }
});
