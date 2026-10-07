//! Fuzz the all-tiles polygon slicer: it must **never panic**, and every `Ok` result must agree with
//! the single-tile slicer, tile by tile.
//!
//! For a structured input (a multipolygon of arbitrary — often self-intersecting or overlapping —
//! rings, and a slicer config):
//!
//! 1. **No panic, ever.** `PolygonSlicerAll::add_feature` returns `Ok`/`Err`; the fuzz build enables
//!    overflow checks, so unchecked arithmetic surfaces as a crash.
//! 2. When it accepts the feature, over the whole reachable tile span (skipped when a tiny extent
//!    makes the span too large to scan cheaply):
//!    - every **edge tile** holds exactly what a `PolygonSlicerOne` bound to it yields for each
//!      polygon, in order;
//!    - every **fill-run** tile is one the single-tile slicer fills with whole-tile boxes only, and
//!      no tile is both;
//!    - every **other** tile is one the single-tile slicer leaves empty.
//! 3. Re-adding after `clear()` reproduces the same output.
//!
//! Coordinates are `i8` so each run is cheap; the oversize/overflow error paths are covered
//! deterministically by `tests/errors.rs`.

#![no_main]

use std::collections::{BTreeMap, BTreeSet};

use arbitrary::Arbitrary;
use geo_types::Coord;
use libfuzzer_sys::fuzz_target;
use map_tile_toolkit::{PolygonSlicerAll, PolygonSlicerOne, TileId};

/// Cap on tiles scanned by the single-tile oracle per run.
const SCAN_CAP: i64 = 2_048;

#[derive(Arbitrary, Debug)]
struct Input {
    extent: u8,
    buffer: u8,
    /// Polygons, each a list of rings (exterior first) of small coordinates.
    polygons: Vec<Vec<Vec<(i8, i8)>>>,
}

/// Per polygon, its rings (exterior first), tile-local.
type Pieces = Vec<Vec<Vec<Coord<i32>>>>;

fn one_tile(polygons: &[Vec<Vec<Coord<i32>>>], extent: u32, buffer: u16, tile: TileId) -> Pieces {
    let mut one = PolygonSlicerOne::<Coord<i32>>::new(extent, buffer, tile).expect("config");
    for rings in polygons {
        let Some((exterior, holes)) = rings.split_first() else {
            continue;
        };
        let holes: Vec<&[Coord<i32>]> = holes.iter().map(Vec::as_slice).collect();
        one.add_feature(exterior, &holes)
            .expect("a tile in the reachable span never overflows");
    }
    one.iter_features()
        .map(|f| f.iter_rings().map(|r| r.vertices().to_vec()).collect())
        .collect()
}

fn output(all: &PolygonSlicerAll<Coord<i32>>) -> (BTreeMap<TileId, Pieces>, BTreeSet<TileId>) {
    let mut tiles = BTreeMap::new();
    let mut fills = BTreeSet::new();
    for f in all.iter_features() {
        for t in f.iter_tiles() {
            let pieces = t
                .iter_polygons()
                .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
                .collect();
            assert!(tiles.insert(t.tile_id(), pieces).is_none(), "tile listed twice");
        }
        for run in f.iter_fill_runs() {
            assert!(!run.x.is_empty(), "empty fill run");
            for x in run.x {
                let tile = TileId::new(x, run.y);
                assert!(!tiles.contains_key(&tile), "{tile:?} is both edge and fill");
                assert!(fills.insert(tile), "{tile:?} filled twice");
            }
        }
    }
    (tiles, fills)
}

/// Whether every piece is a single all-synthetic ring outside the tile's core — a fill box.
fn only_fill_boxes(pieces: &Pieces, extent: u32) -> bool {
    let e = i32::try_from(extent).expect("small extent");
    !pieces.is_empty()
        && pieces.iter().all(|p| {
            p.len() == 1
                && p[0].len() == 5
                && p[0].iter().all(|c| !(0..e).contains(&c.x) && !(0..e).contains(&c.y))
        })
}

fuzz_target!(|input: Input| {
    let extent = u32::from(input.extent);
    let buffer = u16::from(input.buffer);
    let Ok(mut all) = PolygonSlicerAll::<Coord<i32>>::new(extent, buffer) else {
        return; // invalid config — rejected, nothing to test
    };
    let polygons: Vec<Vec<Vec<Coord<i32>>>> = input
        .polygons
        .iter()
        .take(4)
        .map(|rings| {
            rings
                .iter()
                .take(4)
                .map(|ring| {
                    ring.iter()
                        .take(24)
                        .map(|&(x, y)| Coord {
                            x: i32::from(x),
                            y: i32::from(y),
                        })
                        .collect()
                })
                .collect()
        })
        .collect();

    if all.add_feature(&polygons).is_err() {
        return;
    }
    let (tiles, fills) = output(&all);

    all.clear();
    all.add_feature(&polygons).expect("the same feature slices again");
    assert_eq!(output(&all), (tiles.clone(), fills.clone()), "clear() changed the result");

    // Every piece is within one tile (plus buffer) of a vertex, and coordinates are `i8`.
    let d = i64::from(extent);
    let tile = |v: i64| i32::try_from(v.div_euclid(d)).expect("i8 coords keep tiles in i32");
    let (lo, hi) = (tile(-128 - i64::from(buffer)) - 1, tile(127 + i64::from(buffer)) + 1);
    let side = i64::from(hi - lo + 1);
    if side * side > SCAN_CAP {
        return;
    }
    for y in lo..=hi {
        for x in lo..=hi {
            let id = TileId::new(x, y);
            let one = one_tile(&polygons, extent, buffer, id);
            if let Some(pieces) = tiles.get(&id) {
                assert_eq!(pieces, &one, "edge tile {id:?} disagrees with the single-tile slicer");
            } else if fills.contains(&id) {
                assert!(only_fill_boxes(&one, extent), "fill tile {id:?}, single-tile: {one:?}");
            } else {
                assert!(one.is_empty(), "tile {id:?} missing: {one:?}");
            }
        }
    }
});
