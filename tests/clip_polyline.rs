//! Insta GeoJSON snapshots for the integer polyline slicer.
//!
//! Each fixture in `tests/fixtures/inputs/*.geojson` is a `FeatureCollection` with one `LineString`
//! or `MultiLineString` feature (whole-number coordinates in valid lon/lat range so the fixtures
//! and snapshots render on a map). Every fixture is sliced two ways and the two must be **byte
//! identical**:
//!
//! 1. `slice_all_tiles` — the whole geometry into every tile it touches, in one pass.
//! 2. For each tile that (1) produced, `slice_tile` re-clips that single tile.
//!
//! The result is snapshotted as a `FeatureCollection`: the original polyline first, then one
//! feature per per-tile piece. `tests/fixtures/grid.geojson` overlays the 25-unit tile grid.
//! Regenerate with `just bless`.
//!
//! Every fixture is snapshotted at two buffer sizes, each into its own directory:
//! - `snapshots/` — buffer 0 (tile boxes flush with the grid);
//! - `snapshots-5/` — buffer 5 (each tile box grown 5 units per side, so near-edge vertices and
//!   crossing segments also land in the neighboring tiles).

#![allow(clippy::pedantic, reason = "test/inspection tool")]

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::{Coord, Geometry, LineString, MultiLineString};
use geojson::FeatureCollection;
use insta::assert_binary_snapshot;
use map_tile_toolkit::{Slicer, TileId};
use serde_json::json;

mod support;
use support::{feature, load_fixture};

/// Buffer sizes each fixture is snapshotted at, paired with the directory to write into. Buffer 0
/// keeps the tile boxes flush with the grid; buffer 5 (a fifth of a tile) grows each box outward so
/// near-edge vertices and crossing segments also land in the neighboring tiles.
static SLICERS: [(Slicer, &str); 2] = [
    (support::SLICER, "snapshots"),
    (support::SLICER_BUFFER, "snapshots-5"),
];

mod files {
    use test_each_file::test_each_path;

    use super::slice_one_fixture;

    // Generate one test per input fixture.
    test_each_path! { for ["geojson"] in "./tests/fixtures" => slice_one_fixture }
}

/// The component lines of a polyline geometry.
fn each_line(geom: &Geometry<i32>) -> Vec<&LineString<i32>> {
    match geom {
        Geometry::LineString(ls) => vec![ls],
        Geometry::MultiLineString(mls) => mls.0.iter().collect(),
        other => panic!("expected a polyline geometry, got {other:?}"),
    }
}

/// Inclusive tile-coordinate bounds covering every vertex of `geom`, padded by one tile so the
/// per-tile scan also checks the empty tiles just outside the geometry.
fn padded_tile_span(geom: &Geometry<i32>) -> (TileId, TileId) {
    let mut lo = TileId::new(i32::MAX, i32::MAX);
    let mut hi = TileId::new(i32::MIN, i32::MIN);
    for line in each_line(geom) {
        for &c in &line.0 {
            let (tx, ty) = (c.x.div_euclid(25), c.y.div_euclid(25));
            lo = TileId::new(lo.x.min(tx), lo.y.min(ty));
            hi = TileId::new(hi.x.max(tx), hi.y.max(ty));
        }
    }
    (
        TileId::new(lo.x - 1, lo.y - 1),
        TileId::new(hi.x + 1, hi.y + 1),
    )
}

/// A copy of `geom` with every vertex repeated once — consecutive duplicates the slicers must
/// transparently drop, so clipping the copy yields the same result as the original.
fn duplicate_vertices(geom: &Geometry<i32>) -> Geometry<i32> {
    let dup = |ls: &LineString<i32>| LineString(ls.0.iter().flat_map(|&c| [c, c]).collect());
    match geom {
        Geometry::LineString(l) => Geometry::LineString(dup(l)),
        Geometry::MultiLineString(m) => {
            Geometry::MultiLineString(MultiLineString(m.0.iter().map(dup).collect()))
        }
        other => panic!("expected a polyline geometry, got {other:?}"),
    }
}

/// Convert an integer polyline geometry back to `f64` for GeoJSON output.
fn to_f64(geom: &Geometry<i32>) -> Geometry<f64> {
    let ls = |ls: &LineString<i32>| {
        LineString(
            ls.0.iter()
                .map(|c| Coord {
                    x: f64::from(c.x),
                    y: f64::from(c.y),
                })
                .collect(),
        )
    };
    match geom {
        Geometry::LineString(l) => Geometry::LineString(ls(l)),
        Geometry::MultiLineString(m) => {
            Geometry::MultiLineString(MultiLineString(m.0.iter().map(ls).collect()))
        }
        other => panic!("expected a polyline geometry, got {other:?}"),
    }
}

/// Build the snapshot `FeatureCollection`: the original polyline first, then one feature per
/// per-tile piece (colored by tile parity so neighbors contrast, tagged with the tile).
fn build_fc(input: &Geometry<i32>, tiles: &BTreeMap<TileId, Geometry<i32>>) -> FeatureCollection {
    let mut features = vec![feature(
        &to_f64(input),
        vec![
            ("role", json!("input")),
            ("stroke", json!("#888888")),
            ("stroke-width", json!(1)),
        ],
    )];
    let mut tiles = tiles.iter().map(|(&k, v)| (k, v)).collect::<Vec<_>>();
    tiles.sort_unstable_by_key(|(k, _)| (k.y, k.x));
    for (tile, piece) in tiles {
        let color = if (tile.x + tile.y).rem_euclid(2) == 0 {
            "#1f77b4"
        } else {
            "#ff7f0e"
        };
        features.push(feature(
            &to_f64(piece),
            vec![
                // ("role", json!("tile")),
                ("role", json!(format!("tile {}/{}", tile.x, tile.y))),
                ("stroke", json!(color)),
                ("stroke-width", json!(3)),
            ],
        ));
    }
    FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    }
}

fn slice_one_fixture([path]: [&Path; 1]) {
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    let geom = load_fixture(path);
    for (slicer, snapshot_dir) in &SLICERS {
        slice_at_buffer(slicer, stem, &geom, snapshot_dir);
    }
}

#[test]
#[ignore = "manually save big geometry"]
fn save_big_geometry() {
    let features = vec![feature(&to_f64(&support::big_polyline()), vec![])];
    let fc = FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    };

    let geojson = serde_json::to_vec_pretty(&fc).expect("serializes");
    std::fs::write("tests/fixtures/big-geometry.geojson", geojson).expect("writes");
}

/// The single-pass `slice_all` must still agree with per-tile `slice` on a large geometry that
/// touches many tiles — the case the fixtures are too small to exercise (and where the old
/// re-clip-per-tile algorithm did `O(vertices × tiles)` work). Not snapshotted (it would be huge);
/// this only guards the batch/per-tile equivalence at scale, at both buffer sizes.
#[test]
fn big_geometry_batch_matches_per_tile() {
    let geom = support::big_polyline();

    for (slicer, _) in &SLICERS {
        let all: BTreeMap<TileId, Geometry<i32>> = slicer
            .slice_all(&geom)
            .expect("polyline")
            .into_iter()
            .collect();
        let (lo, hi) = padded_tile_span(&geom);
        let mut one = BTreeMap::new();
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let tile = TileId::new(x, y);
                if let Some(piece) = slicer.slice(&geom, tile).expect("polyline") {
                    one.insert(tile, piece);
                }
            }
        }
        assert_eq!(
            all,
            one,
            "big-geometry batch and per-tile slicing disagree (buffer {})",
            slicer.buffer()
        );
        assert!(
            all.len() > 100,
            "expected the big geometry to touch many tiles"
        );
    }
}

/// The shared `BIG_CONFIGS` slicers must slice the big polyline into the documented number of
/// tiles: `single` → one, `few` → a 2×2 grid of four, `multi` → many. Guards the divider choices
/// the benchmarks and the `profile` example rely on.
#[test]
fn big_config_tile_counts() {
    let geom = support::big_polyline();
    for (name, slicer) in support::BIG_CONFIGS {
        let n = slicer.slice_all(&geom).expect("polyline").len();
        match name {
            "single" => assert_eq!(n, 1, "`single` should keep the whole polyline in one tile"),
            "few" => assert_eq!(n, 4, "`few` should produce a 2×2 grid of tiles"),
            "multi" => assert!(n > 100, "`multi` should produce many tiles, got {n}"),
            other => panic!("unexpected config {other}"),
        }
    }
}

/// Run every cross-check for one fixture at one buffer size, then snapshot the result into
/// `snapshot_dir`.
fn slice_at_buffer(slicer: &Slicer, stem: &str, geom: &Geometry<i32>, snapshot_dir: &str) {
    // (1) Slice the whole geometry into every tile it touches.
    let all: BTreeMap<TileId, Geometry<i32>> = slicer
        .slice_all(geom)
        .expect("polyline")
        .into_iter()
        .collect();

    // (2) Independently, clip one tile at a time across the whole tile span the geometry could
    // reach (padded by one tile). Collecting every non-empty result must reproduce `all` exactly —
    // this checks the batch found no wrong pieces and missed no tile (including tiles a segment
    // only crosses, which both paths must include).
    let (lo, hi) = padded_tile_span(geom);
    let mut one = BTreeMap::new();
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            if let Some(piece) = slicer.slice(geom, tile).expect("polyline") {
                one.insert(tile, piece);
            }
        }
    }
    assert_eq!(
        all,
        one,
        "batch and per-tile slicing disagree for {stem} (buffer {})",
        slicer.buffer()
    );

    // (3) Duplicating every vertex must not change either slicer's output (consecutive dups are
    // dropped), so both paths on the duplicated input still match the original result.
    let duped = duplicate_vertices(geom);
    let all_duped: BTreeMap<TileId, Geometry<i32>> = slicer
        .slice_all(&duped)
        .expect("polyline")
        .into_iter()
        .collect();
    assert_eq!(
        all_duped,
        all,
        "duplicating every vertex changed the batch result for {stem} (buffer {})",
        slicer.buffer()
    );
    for (&tile, piece) in &all {
        let piece_dup = slicer
            .slice(&duped, tile)
            .expect("polyline")
            .unwrap_or_else(|| {
                panic!("tile {tile:?} vanished after vertex duplication for {stem}")
            });
        assert_eq!(
            &piece_dup, piece,
            "duplicated-vertex per-tile differs at {tile:?} for {stem}"
        );
    }

    // The two snapshots must be byte identical; snapshot the (shared) result.
    let all_bytes = serde_json::to_vec_pretty(&build_fc(geom, &all)).expect("serializes");
    let one_bytes = serde_json::to_vec_pretty(&build_fc(geom, &one)).expect("serializes");
    assert_eq!(
        all_bytes,
        one_bytes,
        "batch and per-tile snapshots differ for {stem} (buffer {})",
        slicer.buffer()
    );

    insta::with_settings!({
        snapshot_path => snapshot_dir,
        prepend_module_to_snapshot => false,
    }, {
        let name = if slicer.buffer() > 0 {
            format!("{stem}-{}.geojson", slicer.buffer())
        } else {
            format!("{stem}.geojson")
        };
        assert_binary_snapshot!(&name, all_bytes);
    });
}
