//! Insta geojson snapshots for the integer polyline clipper ([`clip_polyline::slice_tile`]).
//!
//! Fixtures live in `tests/fixtures/<kind>/*.geojson`, grouped by input geometry:
//! * `polyline/` — a `LineString`, clipped whole.
//! * `polygon/` — a `Polygon`; its exterior ring is clipped as a closed polyline.
//! * `polygon_with_holes/` — a `Polygon`; every ring (exterior and each hole) is clipped.
//!
//! Each fixture is a `FeatureCollection` carrying a top-level GeoJSON `"bbox"` member
//! (`[min_x, min_y, max_x, max_y]`, the tile box) and a single geometry feature in raw
//! tile-local **i32** coordinates. Every ring of that geometry is run through
//! [`clip_polyline::slice_tile`], and the result is snapshotted as a `FeatureCollection`: the
//! bbox rectangle first, then the input geometry, then one feature per output piece. Because the
//! snapshot ends in `.geojson`, a diff renders on a map. Regenerate with `just bless`.

#![allow(clippy::pedantic, reason = "test/inspection tool")]

use std::fs;
use std::path::Path;

use geo::MapCoords as _;
use geo_types::{Coord, Geometry, LineString, Rect, coord};
use geojson::{Feature, FeatureCollection, GeoJson, GeometryValue, JsonObject, JsonValue};
use insta::assert_binary_snapshot;
use map_tile_toolkit::clip_polyline::slice_tile;
use serde_json::json;

mod files {
    use test_each_file::test_each_path;

    use super::clip_one_fixture;

    // Generate one test per fixture instead of iterating the directory inside a single test. One
    // invocation per kind dir (each becomes a module) so the `other/` fixtures — which belong to
    // `geojson_snapshots.rs` and have no bbox — are never fed to the polyline clipper.
    test_each_path! { for ["geojson"] in "./tests/fixtures/polyline" as polyline => clip_one_fixture }
    test_each_path! { for ["geojson"] in "./tests/fixtures/polygon" as polygon => clip_one_fixture }
    test_each_path! { for ["geojson"] in "./tests/fixtures/polygon_with_holes" as polygon_with_holes => clip_one_fixture }
}

/// Parse a fixture into its clip box and the rings to clip (raw i32 tile coordinates), or `None`
/// when it carries no `bbox` — a tile-slicing fixture that belongs to `geojson_snapshots.rs`.
fn load_fixture(path: &Path) -> Option<(Rect<i32>, Geometry<i32>, Vec<LineString<i32>>)> {
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
        .expect("fixture has a geometry")
        .map_coords(|c| coord! { x: c.x as i32, y: c.y as i32 });

    let rings = match &geom {
        Geometry::LineString(ls) => vec![ls.clone()],
        Geometry::Polygon(p) => std::iter::once(p.exterior().clone())
            .chain(p.interiors().iter().cloned())
            .collect(),
        other => panic!("unsupported fixture geometry: {other:?}"),
    };

    Some((bbox, geom, rings))
}

fn feature(geom: &Geometry<i32>, props: Vec<(&str, JsonValue)>) -> Feature {
    let mut properties = JsonObject::new();
    for (k, v) in props {
        properties.insert(k.to_string(), v);
    }
    // GeoJSON coordinates are f64; the values are exact small integers here.
    let as_f64 = geom.map_coords(|c| coord! { x: f64::from(c.x), y: f64::from(c.y) });
    Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::new(GeometryValue::from(&as_f64))),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    }
}

/// The clip box drawn as a rectangle, so the snapshot shows what each piece was clipped against.
fn bbox_feature(bbox: Rect<i32>) -> Feature {
    let (mn, mx) = (bbox.min(), bbox.max());
    let ring = LineString(vec![
        Coord { x: mn.x, y: mn.y },
        Coord { x: mx.x, y: mn.y },
        Coord { x: mx.x, y: mx.y },
        Coord { x: mn.x, y: mx.y },
        Coord { x: mn.x, y: mn.y },
    ]);
    feature(
        &Geometry::Polygon(geo_types::Polygon::new(ring, vec![])),
        vec![
            ("role", json!("bbox")),
            ("stroke", json!("#111111")),
            ("fill", json!("#111111")),
            ("fill-opacity", json!(0.03)),
        ],
    )
}

/// Build the snapshot: bbox rectangle, the input geometry, then one feature per output piece.
fn build_fc(
    bbox: Rect<i32>,
    input: &Geometry<i32>,
    pieces: &[LineString<i32>],
) -> FeatureCollection {
    let mut features = vec![
        bbox_feature(bbox),
        feature(
            input,
            vec![
                ("role", json!("input")),
                ("stroke", json!("#888888")),
                ("stroke-width", json!(1)),
            ],
        ),
    ];
    for (i, piece) in pieces.iter().enumerate() {
        let color = if i.is_multiple_of(2) {
            "#1f77b4"
        } else {
            "#ff7f0e"
        };
        features.push(feature(
            &Geometry::LineString(piece.clone()),
            vec![
                ("role", json!("output")),
                ("piece", json!(i)),
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

/// Clip one fixture and snapshot the result, grouping snapshots under the fixture's kind dir.
fn clip_one_fixture([path]: [&Path; 1]) {
    let kind = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .expect("fixture lives in a kind dir");
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");

    // Skip tile-slicing fixtures (no bbox) that share the type dirs; they belong to
    // `geojson_snapshots.rs`.
    let Some((bbox, input, rings)) = load_fixture(path) else {
        return;
    };
    let pieces: Vec<LineString<i32>> = rings
        .iter()
        .filter_map(|ring| slice_tile(ring, bbox))
        .flat_map(|g| match g {
            Geometry::LineString(ls) => vec![ls],
            Geometry::MultiLineString(mls) => mls.0,
            other => panic!("unexpected output geometry: {other:?}"),
        })
        .collect();

    let bytes = serde_json::to_vec_pretty(&build_fc(bbox, &input, &pieces)).expect("serializes");
    insta::with_settings!({
        snapshot_path => format!("snapshots/clip_polyline/{kind}"),
        prepend_module_to_snapshot => false,
    }, {
        assert_binary_snapshot!(&format!("{stem}.geojson"), bytes);
    });
}
