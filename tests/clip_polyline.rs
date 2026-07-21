//! Insta GeoJSON snapshots for the integer polyline clipper ([`clip_polyline::slice_tile`]).
//!
//! Fixtures live in `tests/fixtures/polyline/*.geojson`. Each is a `FeatureCollection` carrying a
//! top-level GeoJSON `"bbox"` member (`[min_x, min_y, max_x, max_y]`, the clip box) and a single
//! `LineString` feature. `slice_tile` works on integer coordinates, so fixture coordinates are
//! whole numbers kept in valid lon/lat range (a small box near the origin) so that both the
//! fixtures and the snapshots render on a map.
//!
//! The line is run through [`clip_polyline::slice_tile`] and the result is snapshotted as a
//! `FeatureCollection`: the input line first, then the clipped output (a `LineString` or
//! `MultiLineString`). When the clip produces nothing, the clip box is emitted in its place as a
//! marker. Because the snapshot ends in `.geojson`, a diff renders on a map. Regenerate with
//! `just bless`.
//!
//! The `polyline/` dir is shared with `geojson_snapshots.rs`; its tile-slicing fixtures (carrying
//! a `"zoom"` member rather than a `"bbox"`) are skipped here.

#![allow(clippy::pedantic, reason = "test/inspection tool")]

use std::fs;
use std::path::Path;

use geo::MapCoords as _;
use geo_types::{coord, Geometry, LineString, MultiLineString, Rect};
use geojson::{Feature, FeatureCollection, GeoJson, JsonValue};
use insta::assert_binary_snapshot;
use map_tile_toolkit::clip_polyline::slice_tile;
use serde_json::json;

mod support;

mod files {
    use test_each_file::test_each_path;

    use super::clip_one_fixture;

    // Generate one test per polyline fixture instead of iterating the directory in a single test.
    test_each_path! { for ["geojson"] in "./tests/fixtures/polyline" => clip_one_fixture }
}

/// Parse a fixture into its clip box and input line (raw i32 coordinates), or `None` when it
/// carries no `bbox` — a tile-slicing fixture that belongs to `geojson_snapshots.rs`.
fn load_fixture(path: &Path) -> Option<(Rect<i32>, LineString<i32>)> {
    let text = fs::read_to_string(path).expect("readable fixture");
    let GeoJson::FeatureCollection(fc) = text.parse().expect("valid GeoJSON") else {
        panic!("fixture must be a FeatureCollection: {}", path.display());
    };
    let b = fc.bbox.as_ref()?;
    let bbox = Rect::new(
        coord! { x: b[0] as i32, y: b[1] as i32 },
        coord! { x: b[2] as i32, y: b[3] as i32 },
    );

    let geom = fc
        .features
        .into_iter()
        .find_map(|f| f.geometry)
        .map(|g| Geometry::<f64>::try_from(g).expect("geometry converts"))
        .expect("fixture has a geometry");
    let Geometry::LineString(line) = geom else {
        panic!("clip_polyline only clips polylines, got a non-LineString: {}", path.display());
    };
    let line = line.map_coords(|c| coord! { x: c.x as i32, y: c.y as i32 });

    Some((bbox, line))
}

/// Wrap an integer geometry as a GeoJSON feature (coordinates are f64, but the values are exact
/// small integers here).
fn feature(geom: &Geometry<i32>, props: Vec<(&str, JsonValue)>) -> Feature {
    let as_f64 = geom.map_coords(|c| coord! { x: f64::from(c.x), y: f64::from(c.y) });
    support::feature(&as_f64, props)
}

/// The clip box drawn as a rectangle, so the snapshot shows what each piece was clipped against.
fn bbox_feature(bbox: Rect<i32>) -> Feature {
    feature(
        &Geometry::Polygon(bbox.to_polygon()),
        vec![
            ("role", json!("bbox")),
            ("stroke", json!("#111111")),
            ("fill", json!("#111111")),
            ("fill-opacity", json!(0.03)),
        ],
    )
}

/// Build the snapshot: the `input` line(s) first, then the clipped `output` — a `LineString`/
/// `MultiLineString` when `slice_tile` produced something (`Ok`), or the clip box as a marker
/// that nothing was generated (`Err`).
fn build_fc(
    input: MultiLineString<i32>,
    output: Result<Geometry<i32>, Rect<i32>>,
) -> FeatureCollection {
    let input = feature(
        &Geometry::MultiLineString(input),
        vec![
            ("role", json!("input")),
            ("stroke", json!("#888888")),
            ("stroke-width", json!(1)),
        ],
    );
    let output = match output {
        Ok(geom) => feature(
            &geom,
            vec![
                ("role", json!("output")),
                ("stroke", json!("#1f77b4")),
                ("stroke-width", json!(3)),
            ],
        ),
        Err(bbox) => bbox_feature(bbox),
    };
    FeatureCollection {
        bbox: None,
        features: vec![input, output],
        foreign_members: None,
    }
}

/// Clip one polyline fixture and snapshot the result.
fn clip_one_fixture([path]: [&Path; 1]) {
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");

    // Skip tile-slicing fixtures (no bbox) that share the dir; they belong to
    // `geojson_snapshots.rs`.
    let Some((bbox, line)) = load_fixture(path) else {
        return;
    };
    // The clipped output, or the clip box when the line fell entirely outside it.
    let output = slice_tile(&line, bbox).ok_or(bbox);
    let input = MultiLineString(vec![line]);

    let bytes = serde_json::to_vec_pretty(&build_fc(input, output)).expect("serializes");
    insta::with_settings!({
        snapshot_path => "snapshots/clip_polyline",
        prepend_module_to_snapshot => false,
    }, {
        assert_binary_snapshot!(&format!("{stem}.geojson"), bytes);
    });
}
