//! Visual regression snapshots: slice each GeoJSON fixture and snapshot the result as a
//! binary `.geojson` snapshot.
//!
//! Each snapshot is a GeoJSON `FeatureCollection` whose **first** feature is the original input
//! geometry (thick dark outline) followed by **one feature per tile** (thinner, alternating
//! fill colors), all reprojected back to WGS84 lon/lat. Because the snapshot file ends in
//! `.geojson`, GitHub (and geojson.io, QGIS, kepler.gl) renders it directly on a map — so a
//! snapshot diff is a visual diff of the clipping output. The [simplestyle-spec] properties
//! (`stroke`/`fill`/…) drive the colors.
//!
//! Fixtures live in `tests/fixtures/geojson/*.geojson` (lon/lat), each carrying a top-level
//! `"zoom"` member. Regenerate snapshots with `just bless` (or `INSTA_UPDATE=always cargo test
//! --test geojson_snapshots`).
//!
//! [simplestyle-spec]: https://github.com/mapbox/simplestyle-spec

#![allow(clippy::pedantic, reason = "test/inspection tool")]

use std::f64::consts::PI;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;

use geo::MapCoords as _;
use geo_types::{Coord, Geometry, GeometryCollection};
use geojson::{Feature, FeatureCollection, GeoJson, JsonObject, JsonValue};
use map_tile_toolkit::{SliceOptions, TileId, slice_all_tiles};
use serde_json::json;

/// Web Mercator plane width (meters), matching the crate's `EARTH_CIRCUMFERENCE`.
const CIRC: f64 = 40_075_016.685_578_5;
/// Half the plane width: coordinates span `-ORIGIN..=ORIGIN`.
const ORIGIN: f64 = CIRC / 2.0;
/// Sphere radius implied by the plane width, for the lon/lat <-> meters projection.
const R: f64 = ORIGIN / PI;

const EXTENT: u32 = 4096;
const BUFFER_PX: u32 = 64;

// --- projections ---------------------------------------------------------------------------

fn lonlat_to_mercator(c: Coord<f64>) -> Coord<f64> {
    Coord {
        x: R * c.x.to_radians(),
        y: R * (PI / 4.0 + c.y.to_radians() / 2.0).tan().ln(),
    }
}

fn mercator_to_lonlat(c: Coord<f64>) -> Coord<f64> {
    Coord {
        x: (c.x / R).to_degrees(),
        y: (2.0 * (c.y / R).exp().atan() - PI / 2.0).to_degrees(),
    }
}

/// Map a tile-local integer coordinate (`0..extent`, plus buffer) back to Web Mercator.
fn tile_local_to_mercator(tile: TileId, c: Coord<i32>) -> Coord<f64> {
    let tile_len = CIRC / f64::from(1u32 << tile.z);
    let min_x = -ORIGIN + f64::from(tile.x) * tile_len;
    let max_y = ORIGIN - f64::from(tile.y) * tile_len;
    Coord {
        x: min_x + f64::from(c.x) / f64::from(EXTENT) * tile_len,
        y: max_y - f64::from(c.y) / f64::from(EXTENT) * tile_len,
    }
}

// --- feature building ----------------------------------------------------------------------

fn feature(geom: &Geometry<f64>, props: Vec<(&str, JsonValue)>) -> Feature {
    let mut properties = JsonObject::new();
    for (k, v) in props {
        properties.insert(k.to_string(), v);
    }
    Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::new(geojson::Value::from(geom))),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    }
}

/// Parse a GeoJSON fixture into a single geometry (a `GeometryCollection` if it holds several)
/// plus its `"zoom"` member (default 3).
fn load_fixture(path: &Path) -> (Geometry<f64>, u8) {
    let text = fs::read_to_string(path).expect("readable fixture");
    let gj: GeoJson = text.parse().expect("valid GeoJSON");

    let (features, foreign) = match gj {
        GeoJson::FeatureCollection(fc) => (fc.features, fc.foreign_members),
        GeoJson::Feature(f) => (vec![f], None),
        GeoJson::Geometry(g) => {
            let geom = Geometry::<f64>::try_from(g).expect("geometry converts");
            return (geom, 3);
        }
    };

    let mut geoms: Vec<Geometry<f64>> = features
        .into_iter()
        .filter_map(|f| f.geometry)
        .map(|g| Geometry::<f64>::try_from(g).expect("geometry converts"))
        .collect();
    let geom = match geoms.len() {
        1 => geoms.pop().expect("one geometry"),
        _ => Geometry::GeometryCollection(GeometryCollection(geoms)),
    };
    let zoom = foreign
        .as_ref()
        .and_then(|m| m.get("zoom"))
        .and_then(JsonValue::as_u64)
        .and_then(|z| u8::try_from(z).ok())
        .unwrap_or(3);
    (geom, zoom)
}

/// Build the output `FeatureCollection`: input first, then one feature per tile slice.
fn render(input: &Geometry<f64>, zoom: u8) -> FeatureCollection {
    let opts = SliceOptions::new(NonZeroU32::new(EXTENT).expect("nonzero"), BUFFER_PX);
    let mercator = input.map_coords(lonlat_to_mercator);

    // First feature: the original, a thick dark outline distinct from the slices.
    let mut features = vec![feature(
        input,
        vec![
            ("role", json!("input")),
            ("stroke", json!("#111111")),
            ("stroke-width", json!(3)),
            ("fill", json!("#111111")),
            ("fill-opacity", json!(0.03)),
        ],
    )];

    // One feature per tile, reprojected to lon/lat, colored by parity so neighbors contrast.
    for (id, sliced) in slice_all_tiles(&mercator, zoom, opts) {
        let lonlat = sliced
            .map_coords(|c| tile_local_to_mercator(id, c))
            .map_coords(mercator_to_lonlat);
        let color = if (id.x + id.y) % 2 == 0 {
            "#1f77b4"
        } else {
            "#ff7f0e"
        };
        features.push(feature(
            &lonlat,
            vec![
                ("role", json!("tile")),
                ("tile", json!(format!("{}/{}/{}", id.z, id.x, id.y))),
                ("stroke", json!(color)),
                ("stroke-width", json!(1)),
                ("fill", json!(color)),
                ("fill-opacity", json!(0.4)),
            ],
        ));
    }

    FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    }
}

#[test]
fn geojson_fixtures() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geojson");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .expect("fixtures dir exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "geojson"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found in {}", dir.display());

    insta::with_settings!({ snapshot_path => "snapshots/geojson", prepend_module_to_snapshot => false }, {
        for path in paths {
            let stem = path.file_stem().expect("stem").to_str().expect("utf8");
            let (geom, zoom) = load_fixture(&path);
            let fc = render(&geom, zoom);
            let bytes = serde_json::to_vec_pretty(&fc).expect("serializes");
            insta::assert_binary_snapshot!(format!("{stem}.geojson").as_str(), bytes);
        }
    });
}
