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

#![allow(clippy::pedantic, reason = "test/inspection tool")]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use geo_types::{Coord, Geometry, LineString, MultiLineString};
use geojson::{FeatureCollection, GeoJson};
use insta::assert_binary_snapshot;
use map_tile_toolkit::{TileId, slice_all_tiles, slice_tile};
use serde_json::json;
use geojson::{Feature, GeometryValue, JsonObject, JsonValue};

/// Tile size for the test grid (matches `tests/fixtures/grid.geojson`).
const TILE_SIZE: i32 = 25;

mod files {
    use test_each_file::test_each_path;

    use super::slice_one_fixture;

    // Generate one test per input fixture.
    test_each_path! { for ["geojson"] in "./tests/fixtures" => slice_one_fixture }
}

/// A GeoJSON [`Feature`] wrapping `geom` with the given [simplestyle-spec] properties. Because a
/// snapshot file ends in `.geojson`, GitHub and geojson.io render the properties (`stroke`/`fill`/
/// …) directly on a map.
///
/// [simplestyle-spec]: https://github.com/mapbox/simplestyle-spec
pub fn feature(geom: &Geometry<f64>, props: Vec<(&str, JsonValue)>) -> Feature {
    let properties = props
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect::<JsonObject>();
    Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::new(GeometryValue::from(geom))),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    }
}

/// Parse a fixture into its (integer) polyline geometry.
fn load_fixture(path: &Path) -> Geometry<i32> {
    let text = fs::read_to_string(path).expect("readable fixture");
    let GeoJson::FeatureCollection(fc) = text.parse().expect("valid GeoJSON") else {
        panic!("fixture must be a FeatureCollection: {}", path.display());
    };
    let geom = fc
        .features
        .into_iter()
        .find_map(|f| f.geometry)
        .map(|g| Geometry::<f64>::try_from(g).expect("geometry converts"))
        .expect("fixture has a geometry");
    to_i32(&geom)
}

/// Convert a polyline geometry to integer coordinates (fixtures use whole numbers).
fn to_i32(geom: &Geometry<f64>) -> Geometry<i32> {
    let ls = |ls: &LineString<f64>| {
        LineString(
            ls.0.iter()
                .map(|c| Coord {
                    x: c.x as i32,
                    y: c.y as i32,
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
/// per-tile piece (colored by tile parity so neighbours contrast, tagged with the tile).
fn build_fc(input: &Geometry<i32>, tiles: &BTreeMap<TileId, Geometry<i32>>) -> FeatureCollection {
    let mut features = vec![feature(
        &to_f64(input),
        vec![
            ("role", json!("input")),
            ("stroke", json!("#888888")),
            ("stroke-width", json!(1)),
        ],
    )];
    for (tile, piece) in tiles {
        let color = if (tile.x + tile.y).rem_euclid(2) == 0 {
            "#1f77b4"
        } else {
            "#ff7f0e"
        };
        features.push(feature(
            &to_f64(piece),
            vec![
                ("role", json!("tile")),
                ("tile", json!(format!("{}/{}", tile.x, tile.y))),
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

/// Serialize the snapshot for a per-tile map.
fn snapshot_bytes(input: &Geometry<i32>, tiles: &BTreeMap<TileId, Geometry<i32>>) -> Vec<u8> {
    serde_json::to_vec_pretty(&build_fc(input, tiles)).expect("serializes")
}

fn slice_one_fixture([path]: [&Path; 1]) {
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    let geom = load_fixture(path);

    // (1) Slice the whole geometry into every tile it touches.
    let all = slice_all_tiles(&geom, TILE_SIZE);

    // (2) Re-clip each tile "all" produced, one at a time, and require an identical result.
    let one: BTreeMap<TileId, Geometry<i32>> = all
        .keys()
        .map(|&tile| {
            let piece = slice_tile(&geom, tile, TILE_SIZE).unwrap_or_else(|| {
                panic!("tile {tile:?} is in `all` but `slice_tile` returned None")
            });
            (tile, piece)
        })
        .collect();
    assert_eq!(all, one, "batch and per-tile slicing disagree for {stem}");

    // The two snapshots must be byte identical; snapshot the (shared) result.
    let all_bytes = snapshot_bytes(&geom, &all);
    let one_bytes = snapshot_bytes(&geom, &one);
    assert_eq!(
        all_bytes, one_bytes,
        "batch and per-tile snapshots differ for {stem}"
    );

    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
    }, {
        assert_binary_snapshot!(&format!("{stem}.geojson"), all_bytes);
    });
}
