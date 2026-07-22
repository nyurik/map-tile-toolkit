//! Fuzz the polyline slicer: it must **never panic**, and every `Ok` result must satisfy the
//! library's invariants.
//!
//! For a structured input (a polyline, a slicer config, and an arbitrary probe tile):
//!
//! 1. **No panic, ever.** `slice` on an arbitrary (possibly extreme) tile, and `slice_all`, must
//!    return `Ok`/`Err` — never panic or overflow. The fuzz build enables overflow checks, so any
//!    unchecked arithmetic surfaces here as a crash.
//! 2. When `slice_all` returns `Ok`, the results hold up:
//!    - **Batch round-trips through single-tile:** every `(tile, piece)` equals `slice(tile)`.
//!    - **Batch == exhaustive per-tile scan** over the reachable span (skipped when a tiny divider
//!      makes the span too large to scan cheaply).
//!    - **Duplicate-vertex invariance:** repeating every vertex changes nothing.
//!
//! Coordinates are `i8` (small, so slicing stays fast and the invariants get many cheap
//! iterations); the divider/buffer range freely and the probe tile is a full `i32`, so `slice`'s
//! tile-box overflow handling is stressed too. The oversize/too-many-tiles error paths are covered
//! deterministically by `tests/errors.rs` rather than here.

#![no_main]

use std::collections::BTreeMap;

use arbitrary::Arbitrary;
use geo_types::{Coord, Geometry, LineString, MultiLineString};
use libfuzzer_sys::fuzz_target;
use map_tile_toolkit::{Slicer, TileId};

/// Cap on tiles scanned by the exhaustive oracle per run (each scanned tile re-walks the geometry),
/// so a tiny divider can't blow up time.
const SCAN_CAP: i64 = 4_096;

#[derive(Arbitrary, Debug)]
struct Input {
    divider: u16,
    buffer: u16,
    /// Lines of small (`i8`) coordinates, so slicing stays fast and the invariants run cheaply.
    lines: Vec<Vec<(i8, i8)>>,
    /// An arbitrary tile to probe with `slice` — may be extreme, to stress the tile-box math.
    probe: (i32, i32),
}

fuzz_target!(|input: Input| {
    let Ok(slicer) = Slicer::new(u32::from(input.divider), input.buffer) else {
        return; // divider 0 is rejected — nothing to test
    };

    // Bound the geometry size so each run stays fast.
    let lines: Vec<LineString<i32>> = input
        .lines
        .iter()
        .take(8)
        .map(|pts| {
            LineString(
                pts.iter()
                    .take(64)
                    .map(|&(x, y)| Coord {
                        x: i32::from(x),
                        y: i32::from(y),
                    })
                    .collect(),
            )
        })
        .collect();
    let geom = to_geometry(&lines);

    // (1) A single-tile clip on an arbitrary tile must never panic (Ok or Err both fine).
    let _ = slicer.slice(&geom, TileId::new(input.probe.0, input.probe.1));

    // slice_all must never panic; Err is a valid outcome for oversized/overflowing input.
    let Ok(all) = slicer.slice_all(&geom) else {
        return;
    };
    let all: BTreeMap<TileId, Geometry<i32>> = all.into_iter().collect();

    // (2) Every batch result round-trips through single-tile slicing (which must succeed here).
    for (&tile, piece) in &all {
        let one = slicer
            .slice(&geom, tile)
            .expect("slice must succeed for a tile slice_all produced");
        assert_eq!(
            one.as_ref(),
            Some(piece),
            "slice disagrees with slice_all at {tile:?}"
        );
    }

    // (3) Exhaustive per-tile scan of the reachable span reproduces slice_all, when small enough.
    if let Some((lo, hi)) = reachable_span(&lines, slicer) {
        let area = i64::from(hi.x - lo.x + 1) * i64::from(hi.y - lo.y + 1);
        if area <= SCAN_CAP {
            let mut scanned = BTreeMap::new();
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    let tile = TileId::new(x, y);
                    if let Some(piece) = slicer.slice(&geom, tile).expect("slice in span") {
                        scanned.insert(tile, piece);
                    }
                }
            }
            assert_eq!(all, scanned, "slice_all and full per-tile scan disagree");
        }
    }

    // (4) Duplicating every vertex must not change the result.
    let duped: Vec<LineString<i32>> = lines
        .iter()
        .map(|ls| LineString(ls.0.iter().flat_map(|&c| [c, c]).collect()))
        .collect();
    let all_duped: BTreeMap<TileId, Geometry<i32>> = slicer
        .slice_all(&to_geometry(&duped))
        .expect("duplicating vertices cannot make a valid geometry invalid")
        .into_iter()
        .collect();
    assert_eq!(
        all, all_duped,
        "duplicating every vertex changed the result"
    );
});

/// One line → `LineString`; anything else → `MultiLineString` (the two kinds the slicer accepts).
fn to_geometry(lines: &[LineString<i32>]) -> Geometry<i32> {
    if let [only] = lines {
        Geometry::LineString(only.clone())
    } else {
        Geometry::MultiLineString(MultiLineString(lines.to_vec()))
    }
}

/// Tile span any piece can reach: every vertex's tile grown by the buffer (in tiles) plus one tile
/// of slack. `None` if there are no vertices. A superset of what `slice_all` can return, so scanning
/// it must reproduce `slice_all`. Coordinates are `i16`, so the `i64` math here cannot overflow.
fn reachable_span(lines: &[LineString<i32>], slicer: Slicer) -> Option<(TileId, TileId)> {
    let d = i64::from(slicer.divider());
    let b = i64::from(slicer.buffer());
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    let mut seen = false;
    for line in lines {
        for c in &line.0 {
            min_x = min_x.min(i64::from(c.x));
            min_y = min_y.min(i64::from(c.y));
            max_x = max_x.max(i64::from(c.x));
            max_y = max_y.max(i64::from(c.y));
            seen = true;
        }
    }
    if !seen {
        return None;
    }
    let tile = |v: i64| i32::try_from(v.div_euclid(d)).expect("i16 coords keep tiles in i32");
    Some((
        TileId::new(tile(min_x - b) - 1, tile(min_y - b) - 1),
        TileId::new(tile(max_x + b) + 1, tile(max_y + b) + 1),
    ))
}
