//! Shared helpers for the snapshot tests and the benchmarks: GeoJSON fixture loading/parsing and
//! feature building. Included by `tests/clip_polyline.rs` (`mod support;`) and by
//! `benches/slicing.rs` (via `#[path = "../tests/support/mod.rs"]`).

#![allow(
    dead_code,
    reason = "shared across the test and bench crates; not every helper is used in each"
)]

use std::fs;
use std::path::Path;

use geo_types::{Coord, Geometry, LineString, MultiLineString};
use geojson::{Feature, GeoJson, GeometryValue, JsonObject, JsonValue};

/// Parse a fixture file into its (integer) polyline geometry. Fixtures are `FeatureCollection`s
/// holding a single `LineString`/`MultiLineString` with whole-number coordinates.
pub fn load_fixture(path: &Path) -> Geometry<i32> {
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

/// Every `tests/fixtures/*.geojson` as `(name, geometry)`, sorted by name for stable ordering.
pub fn load_all_fixtures() -> Vec<(String, Geometry<i32>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<(String, Geometry<i32>)> = fs::read_dir(&dir)
        .expect("fixtures dir exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "geojson"))
        .map(|p| {
            let name = p
                .file_stem()
                .expect("stem")
                .to_str()
                .expect("utf8")
                .to_owned();
            (name, load_fixture(&p))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty(), "no fixtures found in {}", dir.display());
    out
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
