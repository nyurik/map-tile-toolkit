//! Fuzz the all-tiles polygon slicer: it must **never panic**, and every `Ok` result must agree with
//! the single-tile slicer, tile by tile.
//!
//! For a structured input (up to two multipolygon features of arbitrary — often self-intersecting or
//! overlapping — rings, and a slicer config):
//!
//! 1. **No panic, ever.** `PolygonSlicerAll::add_feature` returns `Ok`/`Err`; the fuzz build enables
//!    overflow checks, so unchecked arithmetic surfaces as a crash.
//! 2. For every feature it accepts, over the whole reachable tile span (skipped when the span is too
//!    large to scan cheaply):
//!    - every **edge tile** holds exactly what a `PolygonSlicerOne` bound to it yields for each
//!      polygon, in order;
//!    - every **fill-run** tile holds exactly what the single-tile slicer yields there too, once each
//!      run's fill ring is placed in it (in polygon order), and no tile is both;
//!    - every **other** tile is one the single-tile slicer leaves empty.
//!
//!    The second feature checks that features stored one after another read back independently.
//! 3. Re-adding after `clear()` reproduces the same output.
//!
//! Coordinates start as `i8`, so each run stays cheap, then are scaled by a power of two (with the
//! extent, so the tile layout keeps its shape) and moved by an offset, so the crossing and box math
//! also runs on large values and near the `i32` limits.

#![no_main]

use std::collections::BTreeMap;

use arbitrary::Arbitrary;
use geo_types::Coord;
use libfuzzer_sys::fuzz_target;
use map_tile_toolkit::{PolygonSlicerAll, PolygonSlicerOne, TileId};

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

/// One feature's output: its edge tiles' pieces and its filled tiles' fill rings, as pieces.
type Output = (BTreeMap<TileId, Pieces>, BTreeMap<TileId, Pieces>);

/// What the single-tile slicer yields for `tile`, or `None` if the tile itself is out of range.
fn one_tile(polygons: &Polygons, extent: u32, buffer: u16, tile: TileId) -> Option<Pieces> {
    let mut one = PolygonSlicerOne::<Coord<i32>>::new(extent, buffer, tile).expect("config");
    for rings in polygons {
        let Some((exterior, holes)) = rings.split_first() else {
            continue;
        };
        let holes: Vec<&[Coord<i32>]> = holes.iter().map(Vec::as_slice).collect();
        one.add_feature(exterior, &holes).ok()?;
    }
    Some(
        one.iter_features()
            .map(|f| f.iter_rings().map(|r| r.vertices().to_vec()).collect())
            .collect(),
    )
}

fn output(all: &PolygonSlicerAll<Coord<i32>>) -> Vec<Output> {
    all.iter_features()
        .map(|f| {
            let mut tiles = BTreeMap::new();
            let mut fills: BTreeMap<TileId, Vec<(u32, Rings)>> = BTreeMap::new();
            for t in f.iter_tiles() {
                let pieces = t
                    .iter_polygons()
                    .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
                    .collect();
                assert!(
                    tiles.insert(t.tile_id(), pieces).is_none(),
                    "tile listed twice"
                );
            }
            for run in f.iter_fill_runs() {
                assert!(!run.x.is_empty(), "empty fill run");
                let ring = f.fill_ring(&run).to_vec();
                for x in run.x.clone() {
                    let tile = TileId::new(x, run.y);
                    assert!(!tiles.contains_key(&tile), "{tile:?} is both edge and fill");
                    let polys = fills.entry(tile).or_default();
                    assert!(
                        polys.iter().all(|(p, _)| *p != run.polygon),
                        "{tile:?} filled twice by polygon {}",
                        run.polygon
                    );
                    polys.push((run.polygon, vec![ring.clone()]));
                }
            }
            let fills = fills
                .into_iter()
                .map(|(tile, mut polys)| {
                    polys.sort_by_key(|(p, _)| *p);
                    (tile, polys.into_iter().map(|(_, rings)| rings).collect())
                })
                .collect();
            (tiles, fills)
        })
        .collect()
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
fn check(polygons: &Polygons, (tiles, fills): &Output, extent: u32, buffer: u16) {
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
            let Some(one) = one_tile(polygons, extent, buffer, id) else {
                // A padding tile past the `i32` limits: the all-tiles slicer cannot have used it.
                assert!(
                    !tiles.contains_key(&id) && !fills.contains_key(&id),
                    "{id:?} is out of range"
                );
                continue;
            };
            if let Some(pieces) = tiles.get(&id) {
                assert_eq!(
                    pieces, &one,
                    "edge tile {id:?} disagrees with the single-tile slicer"
                );
            } else if let Some(pieces) = fills.get(&id) {
                assert_eq!(
                    pieces, &one,
                    "fill tile {id:?} disagrees with the single-tile slicer"
                );
            } else {
                assert!(one.is_empty(), "tile {id:?} missing: {one:?}");
            }
        }
    }
}

fuzz_target!(|input: Input| {
    let scale = 1_i64 << (input.scale % 24);
    let Ok(extent) = u32::try_from(i64::from(input.extent) * scale) else {
        return;
    };
    let buffer = u16::from(input.buffer);
    let Ok(mut all) = PolygonSlicerAll::<Coord<i32>>::new(extent, buffer) else {
        return; // invalid config — rejected, nothing to test
    };
    let features: Vec<Polygons> = input
        .features
        .iter()
        .take(2)
        .map(|f| place(f, scale, input.origin))
        .collect();

    // A feature is recorded only if it reaches some tile, so pair the outputs with what was kept.
    let mut kept = Vec::new();
    for polygons in &features {
        let before = all.len();
        if all.add_feature(polygons).is_ok() && all.len() > before {
            kept.push(polygons);
        }
    }
    let outputs = output(&all);
    assert_eq!(outputs.len(), kept.len());

    all.clear();
    for polygons in &kept {
        all.add_feature(*polygons)
            .expect("the same feature slices again");
    }
    assert_eq!(output(&all), outputs, "clear() changed the result");

    for (polygons, out) in kept.iter().zip(&outputs) {
        check(polygons, out, extent, buffer);
    }
});
