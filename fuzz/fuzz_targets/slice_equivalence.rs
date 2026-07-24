//! Fuzz the polyline slicer: it must **never panic**, and every `Ok` result must satisfy the
//! library's invariants.
//!
//! For a structured input (some polylines, a slicer config, and an arbitrary probe tile):
//!
//! 1. **No panic, ever.** A single-tile `SlicerOne` on an arbitrary (possibly extreme) tile, and a
//!    `SlicerAll`, must return `Ok`/`Err` — never panic or overflow. The fuzz build enables overflow
//!    checks, so any unchecked arithmetic surfaces here as a crash.
//! 2. Per polyline, when `SlicerAll` accepts it, the results hold up:
//!    - **All-tiles round-trips through single-tile:** every tile's runs equal what a `SlicerOne`
//!      bound to that tile produces.
//!    - **All-tiles == exhaustive per-tile scan** over the reachable span (skipped when a tiny
//!      extent makes the span too large to scan cheaply).
//!    - **Duplicate-vertex invariance:** repeating every vertex changes nothing.
//! 3. Accumulating all polylines never panics, and **merge is order-independent:** `merge(a, b)`
//!    reconstructs the same connectivity (directed-edge set) as `merge(b, a)` for every adjacent
//!    pair of accumulated tiles. (The *order* of the returned runs is unspecified.)
//!
//! Coordinates are `i8` (small, so slicing stays fast and the invariants get many cheap
//! iterations); the extent/buffer range freely and the probe tile is a full `i32`, so the
//! single-tile box-overflow handling is stressed too. The oversize/too-many-tiles error paths are
//! covered deterministically by `tests/errors.rs` rather than here.

#![no_main]

use std::collections::{BTreeMap, HashSet};

use arbitrary::Arbitrary;
use geo_types::Coord;
use libfuzzer_sys::fuzz_target;
use map_tile_toolkit::{SlicerAll, SlicerOne, TileId, merge};

/// Cap on tiles scanned by the exhaustive oracle per run (each scanned tile re-walks the polyline),
/// so a tiny extent can't blow up time.
const SCAN_CAP: i64 = 4_096;

#[derive(Arbitrary, Debug)]
struct Input {
    extent: u16,
    buffer: u16,
    /// Polylines of small (`i8`) coordinates, so slicing stays fast and the invariants run cheaply.
    lines: Vec<Vec<(i8, i8)>>,
    /// An arbitrary tile to probe with a single-tile slicer — may be extreme, to stress the tile-box
    /// math.
    probe: (i32, i32),
}

type Runs = Vec<Vec<Coord<i32>>>;

/// Flatten a `SlicerAll`'s accumulated features into `tile → combined runs`.
fn drain_all(acc: &SlicerAll<Coord<i32>>) -> BTreeMap<TileId, Runs> {
    acc.iter_tiles()
        .map(|t| {
            let runs = t
                .iter_features()
                .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
                .collect();
            (t.id(), runs)
        })
        .collect()
}

/// Slice one polyline into all touched tiles, flattened to `tile → runs`. `None` if the slicer
/// rejects it (oversized/overflowing — a valid outcome, not a bug).
fn slice_all_map(extent: u32, buffer: u16, poly: &[Coord<i32>]) -> Option<BTreeMap<TileId, Runs>> {
    let mut acc = SlicerAll::new(extent, buffer).expect("extent validated");
    acc.add_feature(poly).ok()?;
    Some(drain_all(&acc))
}

/// Clip one polyline to a single tile, flattened to its runs. Must succeed (callers only use tiles
/// the all-tiles pass produced, or tiles inside the reachable span).
fn slice_one_runs(extent: u32, buffer: u16, tile: TileId, poly: &[Coord<i32>]) -> Runs {
    let mut one = SlicerOne::new(extent, buffer, tile).expect("extent validated");
    one.add_feature(poly).expect("slice must succeed for an in-range tile");
    one.iter_features()
        .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
        .collect()
}

fuzz_target!(|input: Input| {
    let extent = u32::from(input.extent);
    let buffer = input.buffer;
    if SlicerAll::<Coord<i32>>::new(extent, buffer).is_err() {
        return; // extent 0 is rejected — nothing to test
    }

    // Bound the input size so each run stays fast.
    let polylines: Vec<Vec<Coord<i32>>> = input
        .lines
        .iter()
        .take(8)
        .map(|pts| {
            pts.iter()
                .take(64)
                .map(|&(x, y)| Coord {
                    x: i32::from(x),
                    y: i32::from(y),
                })
                .collect()
        })
        .collect();
    let probe = TileId::new(input.probe.0, input.probe.1);

    for poly in &polylines {
        // (1) A single-tile clip on an arbitrary tile must never panic (Ok or Err both fine).
        if let Ok(mut one) = SlicerOne::new(extent, buffer, probe) {
            let _ = one.add_feature(poly);
        }

        // The all-tiles pass must never panic; Err is a valid outcome for oversized/overflowing input.
        let Some(all) = slice_all_map(extent, buffer, poly) else {
            continue;
        };

        // (2) Every all-tiles result round-trips through single-tile slicing (which must succeed).
        for (&tile, runs) in &all {
            let one = slice_one_runs(extent, buffer, tile, poly);
            assert_eq!(&one, runs, "single-tile disagrees with all-tiles at {tile:?}");
        }

        // (3) Exhaustive per-tile scan of the reachable span reproduces the all-tiles result, when
        // small enough.
        if let Some((lo, hi)) = reachable_span(poly, extent, buffer) {
            let area = i64::from(hi.x - lo.x + 1) * i64::from(hi.y - lo.y + 1);
            if area <= SCAN_CAP {
                let mut scanned = BTreeMap::new();
                for y in lo.y..=hi.y {
                    for x in lo.x..=hi.x {
                        let tile = TileId::new(x, y);
                        let runs = slice_one_runs(extent, buffer, tile, poly);
                        if !runs.is_empty() {
                            scanned.insert(tile, runs);
                        }
                    }
                }
                assert_eq!(all, scanned, "all-tiles and full per-tile scan disagree");
            }
        }

        // (4) Duplicating every vertex must not change the result.
        let duped: Vec<Coord<i32>> = poly.iter().flat_map(|&c| [c, c]).collect();
        let all_duped = slice_all_map(extent, buffer, &duped)
            .expect("duplicating vertices cannot make a valid polyline invalid");
        assert_eq!(all, all_duped, "duplicating every vertex changed the result");
    }

    // (5) Accumulating all polylines never panics; merge reconstructs the same connectivity on the
    // result regardless of the order its two tiles are passed.
    let mut acc = SlicerAll::new(extent, buffer).expect("extent validated");
    for poly in &polylines {
        if acc.add_feature(poly).is_err() {
            return; // oversized/overflowing — nothing more to check
        }
    }
    let map = drain_all(&acc);
    let ids: Vec<TileId> = map.keys().copied().collect();
    for &t in &ids {
        for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
            let n = TileId::new(t.x + dx, t.y + dy);
            let (Some(a), Some(b)) = (map.get(&t), map.get(&n)) else {
                continue;
            };
            let ab = merge(extent, (t, a.as_slice()), (n, b.as_slice()));
            let ba = merge(extent, (n, b.as_slice()), (t, a.as_slice()));
            // `merge` reconstructs the same *connectivity* regardless of input order; the *order* of
            // the returned runs is unspecified (disconnected components come out first-seen), so
            // compare directed-edge sets, not the run vectors — same contract as `tests/merge.rs`.
            match (ab, ba) {
                (Ok(ab), Ok(ba)) => assert_eq!(
                    edge_set(&ab),
                    edge_set(&ba),
                    "merge connectivity differs by input order for {t:?}/{n:?}"
                ),
                (ab, ba) => assert_eq!(
                    ab.is_ok(),
                    ba.is_ok(),
                    "merge succeeds in only one input order for {t:?}/{n:?}"
                ),
            }
        }
    }
});

/// Directed-edge set of a run list, skipping zero-length edges — the connectivity `merge` must
/// preserve regardless of the order its two inputs are given. Mirrors the `tests/merge.rs` oracle.
fn edge_set(runs: &[Vec<Coord<i32>>]) -> HashSet<(Coord<i32>, Coord<i32>)> {
    let mut set = HashSet::new();
    for run in runs {
        for w in run.windows(2) {
            if w[0] != w[1] {
                set.insert((w[0], w[1]));
            }
        }
    }
    set
}

/// Tile span any piece of `poly` can reach: every vertex's tile grown by the buffer (in tiles) plus
/// one tile of slack. `None` if `poly` is empty. A superset of what the all-tiles pass can return, so
/// scanning it must reproduce that result. Coordinates are `i8`, so the `i64` math cannot overflow.
fn reachable_span(poly: &[Coord<i32>], extent: u32, buffer: u16) -> Option<(TileId, TileId)> {
    let d = i64::from(extent);
    let b = i64::from(buffer);
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for c in poly {
        min_x = min_x.min(i64::from(c.x));
        min_y = min_y.min(i64::from(c.y));
        max_x = max_x.max(i64::from(c.x));
        max_y = max_y.max(i64::from(c.y));
    }
    if poly.is_empty() {
        return None;
    }
    let tile = |v: i64| i32::try_from(v.div_euclid(d)).expect("i8 coords keep tiles in i32");
    Some((
        TileId::new(tile(min_x - b) - 1, tile(min_y - b) - 1),
        TileId::new(tile(max_x + b) + 1, tile(max_y + b) + 1),
    ))
}
