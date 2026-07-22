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
use map_tile_toolkit::Slicer;

/// `Slicer::new` for a compile-time-known-valid config (panics at compile time on a bad literal).
const fn slicer(divider: u32, buffer: u16) -> Slicer {
    match Slicer::new(divider, buffer) {
        Ok(s) => s,
        Err(_) => panic!("invalid slicer config in test support"),
    }
}

/// Tile divider for the small fixtures (matches the `tests/fixtures/grid.geojson` grid).
pub const SLICER: Slicer = slicer(25, 0);
pub const SLICER_BUFFER: Slicer = slicer(25, 5);

/// Slicing [`big_polyline`] with each of these yields a different number of output tiles, so the
/// same large geometry can be benchmarked/profiled across output scales (shared by the benchmarks
/// and the `profile` example so both agree). The big polyline spans roughly `[0,420] × [0,535]`:
/// - `multi` (divider 25) → hundreds of tiles;
/// - `few` (divider 300) → a 2×2 grid of 4 tiles;
/// - `single` (divider 1024) → the whole geometry in one tile.
pub const BIG_CONFIGS: [(&str, Slicer); 3] = [
    ("multi", slicer(25, 0)),
    ("few", slicer(300, 0)),
    ("single", slicer(1024, 0)),
];

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

/// A large, deterministic snake-shaped polyline for benchmarking and large-input correctness
/// checks. It sweeps back and forth (boustrophedon) filling a wide area, so it has many vertices
/// **and** touches many tiles — the case where re-clipping the whole geometry once per tile
/// (`O(vertices × tiles)`) diverges sharply from a single routing pass. Small per-step jitter keeps
/// rows off the axis so segments cross tile boundaries at varied angles. ~3.6k vertices spanning
/// roughly a 420×540 area (≈17×22 tiles on a 25-unit grid).
#[must_use]
pub fn big_polyline() -> Geometry<i32> {
    const ROWS: i32 = 60;
    const COLS: i32 = 60;
    const STEP: i32 = 7; // horizontal vertex spacing (< a 25-unit tile, so segments stay short)
    const ROW_H: i32 = 9; // vertical spacing between rows

    let mut coords = Vec::with_capacity(((ROWS * (COLS + 1)) + 1) as usize);
    for r in 0..ROWS {
        let y0 = r * ROW_H;
        for k in 0..=COLS {
            // Even rows sweep left→right, odd rows right→left, so the path stays connected.
            let x = if r % 2 == 0 {
                k * STEP
            } else {
                (COLS - k) * STEP
            };
            let y = y0 + (k * 3) % 5; // jitter in [0, 4]
            coords.push(Coord { x, y });
        }
    }
    Geometry::LineString(LineString(coords))
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
