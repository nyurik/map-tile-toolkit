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
use geojson::{Feature, FeatureCollection, GeoJson, GeometryValue, JsonObject, JsonValue};
use map_tile_toolkit::{SlicerAll, SlicerOne, TileId};
use serde_json::json;

pub const EXTENT: u32 = 25;

/// A slicer config (extent + buffer) shared by the tests, benches, and example. The slicers now own
/// accumulated state, so the shared value is the *config*, from which each caller spins up a fresh
/// [`SlicerAll`] / [`SlicerOne`].
#[derive(Clone, Copy)]
pub struct Cfg {
    pub extent: u32,
    pub buffer: u16,
}

impl Cfg {
    /// A fresh all-tiles slicer for this config (panics on a bad literal config).
    #[must_use]
    pub fn all(self) -> SlicerAll<Coord<i32>> {
        SlicerAll::new(self.extent, self.buffer).expect("invalid slicer config in test support")
    }

    /// A fresh single-tile slicer bound to `tile` (panics on a bad literal config).
    #[must_use]
    pub fn one(self, tile: TileId) -> SlicerOne<Coord<i32>> {
        SlicerOne::new(self.extent, self.buffer, tile)
            .expect("invalid slicer config in test support")
    }
}

/// A config with the given extent/buffer.
#[must_use]
pub fn slicer(extent: u32, buffer: u16) -> Cfg {
    Cfg { extent, buffer }
}

/// Every permutation of `0..n` — used to prove tile-insertion order independence over a small tile
/// set. Asserts `n <= 10`, so an unexpectedly large set can't explode into `n!` permutations.
#[must_use]
pub fn permutations(n: usize) -> Vec<Vec<usize>> {
    assert!(n <= 10, "permutations of {n} tiles is too many");
    fn go(a: &mut Vec<usize>, k: usize, out: &mut Vec<Vec<usize>>) {
        if k == a.len() {
            out.push(a.clone());
            return;
        }
        for i in k..a.len() {
            a.swap(k, i);
            go(a, k + 1, out);
            a.swap(k, i);
        }
    }
    let mut idx: Vec<usize> = (0..n).collect();
    let mut out = Vec::new();
    go(&mut idx, 0, &mut out);
    out
}

/// Tile extent for the small fixtures (matches the `tests/fixtures/grid.geojson` grid).
#[must_use]
pub fn grid() -> Cfg {
    slicer(EXTENT, 0)
}

/// The grid config with a 5-unit buffer.
#[must_use]
pub fn grid_buffered() -> Cfg {
    slicer(EXTENT, 5)
}

/// Slicing [`big_polyline`] with each of these yields a different number of output tiles, so the
/// same large geometry can be benchmarked/profiled across output scales (shared by the benchmarks
/// and the `profile` example so both agree). The big polyline spans roughly `[0,420] × [0,535]`:
/// - `multi` (extent 25) → hundreds of tiles;
/// - `few` (extent 300) → a 2×2 grid of 4 tiles;
/// - `single` (extent 1024) → the whole geometry in one tile.
#[must_use]
pub fn big_configs() -> [(&'static str, Cfg); 3] {
    [
        ("multi", slicer(EXTENT, 0)),
        ("few", slicer(300, 0)),
        ("single", slicer(1024, 0)),
    ]
}

/// A set of independent polylines — the fixture representation. Each is added as its own feature;
/// this replaces the old single-`Geometry` (possibly `MultiLineString`) input.
pub type Polylines = Vec<Vec<Coord<i32>>>;

/// The component polylines (vertex slices) of a polyline geometry.
pub fn lines_of(geom: &Geometry<i32>) -> Vec<&[Coord<i32>]> {
    match geom {
        Geometry::LineString(ls) => vec![ls.0.as_slice()],
        Geometry::MultiLineString(mls) => mls.0.iter().map(|ls| ls.0.as_slice()).collect(),
        other => panic!("expected a polyline geometry, got {other:?}"),
    }
}

/// The component polylines of a geometry, owned.
#[must_use]
pub fn polylines_of(geom: &Geometry<i32>) -> Polylines {
    lines_of(geom).into_iter().map(<[_]>::to_vec).collect()
}

/// Slice a set of polylines into per-tile runs: each polyline becomes its own feature in a fresh
/// [`SlicerAll`], then a tile's features are flattened into their runs (feature order, then run
/// order). Each run is a plain polyline — runs are never assembled into a `MultiLineString`. Geo-free
/// (works with no cargo feature).
pub fn slice_all_runs(
    cfg: &Cfg,
    polylines: &[Vec<Coord<i32>>],
) -> Vec<(TileId, Vec<Vec<Coord<i32>>>)> {
    let mut acc = cfg.all();
    for line in polylines {
        acc.add_feature(line.as_slice()).expect("slice");
    }
    acc.iter_tiles()
        .map(|tile| (tile.tile_id(), flatten(&tile)))
        .filter(|(_, runs)| !runs.is_empty())
        .collect()
}

/// Clip a set of polylines to one tile → its runs (empty if nothing lands there), each polyline a
/// feature in a fresh [`SlicerOne`], then flattened into runs.
pub fn slice_tile_runs(
    cfg: &Cfg,
    polylines: &[Vec<Coord<i32>>],
    tile: TileId,
) -> Vec<Vec<Coord<i32>>> {
    let mut acc = cfg.one(tile);
    for line in polylines {
        acc.add_feature(line.as_slice()).expect("slice");
    }
    acc.iter_features()
        .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
        .collect()
}

/// Flatten all of a tile's features into a single run list (feature order, then run order), matching
/// the combined per-tile output the batch/per-tile equivalence checks compare.
fn flatten(tile: &map_tile_toolkit::TileView<'_, Coord<i32>>) -> Vec<Vec<Coord<i32>>> {
    tile.iter_features()
        .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
        .collect()
}

/// One parsed fixture feature: its `LineString` as integer coordinates plus its GeoJSON properties.
pub struct TestFeature {
    pub line: Vec<Coord<i32>>,
    pub properties: JsonObject,
}

impl TestFeature {}

/// Parse a fixture file into its features: each a `LineString` (whole-number coordinates, truncated
/// to `i32`) and its properties. Fixtures are `FeatureCollection`s; `MultiLineString` is intentionally
/// rejected — express several polylines as several features instead. This is the shared parse/convert
/// core; [`load_fixture_geoms`] keeps only the geometry, other callers also read properties (e.g. a tile id).
pub fn load_fixture(path: &Path) -> Vec<TestFeature> {
    let text = fs::read_to_string(path).expect("readable fixture");
    let GeoJson::FeatureCollection(fc) = text.parse().expect("valid GeoJSON") else {
        panic!("fixture must be a FeatureCollection: {}", path.display());
    };
    let features: Vec<TestFeature> = fc
        .features
        .into_iter()
        .map(|f| {
            let geom = Geometry::<f64>::try_from(f.geometry.expect("feature has geometry"))
                .expect("geometry converts");
            let line = match to_i32(&geom) {
                Geometry::LineString(ls) => ls.0,
                other => panic!(
                    "fixtures must use LineString features, not {other:?} ({}): express multiple \
                     polylines as multiple features",
                    path.display()
                ),
            };
            TestFeature {
                line,
                properties: f.properties.unwrap_or_default(),
            }
        })
        .collect();
    assert!(
        !features.is_empty(),
        "fixture has no features: {}",
        path.display()
    );
    features
}

/// Parse a fixture file into its (integer) polylines — [`load_fixture`] with the properties dropped.
/// Each `LineString` feature is an independent polyline.
pub fn load_fixture_geoms(path: &Path) -> Polylines {
    load_fixture(path).into_iter().map(|f| f.line).collect()
}

/// Every `tests/fixtures/*.geojson` as `(name, polylines)`, sorted by name for stable ordering.
pub fn load_all_fixtures() -> Vec<(String, Polylines)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<(String, Polylines)> = fs::read_dir(&dir)
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
            (name, load_fixture_geoms(&p))
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

/// Convert an integer polyline geometry to `f64` for GeoJSON output. Inverse of [`to_i32`].
pub fn to_f64(geom: &Geometry<i32>) -> Geometry<f64> {
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

/// A GeoJSON `LineString` [`Feature`] for one integer run (converted to `f64`) with the given
/// [`simplestyle-spec`](https://github.com/mapbox/simplestyle-spec) properties.
pub fn line_feature(run: &[Coord<i32>], props: Vec<(&str, JsonValue)>) -> Feature {
    feature(
        &to_f64(&Geometry::LineString(LineString(run.to_vec()))),
        props,
    )
}

/// A `LineString` feature for one run tagged with the simplestyle `role`, `stroke` color, and
/// `stroke-width` — the shape every snapshot feature uses.
fn styled_line(run: &[Coord<i32>], role: &str, stroke: &str, width: u32) -> Feature {
    line_feature(
        run,
        vec![
            ("role", json!(role)),
            ("stroke", json!(stroke)),
            ("stroke-width", json!(width)),
        ],
    )
}

pub fn line_per_tile(tile: &TileId, color: &str, run: &[Coord<i32>]) -> Feature {
    styled_line(run, &format!("tile {}/{}", tile.x, tile.y), color, 2)
}

pub fn feature_line(role: &str, run: &[Coord<i32>]) -> Feature {
    styled_line(run, role, "#1f77b4", 2)
}

pub fn input_feature(run: &[Coord<i32>]) -> Feature {
    styled_line(run, "input", "#888888", 5)
}

/// Serialize `features` as a pretty-printed GeoJSON `FeatureCollection` — the byte form the snapshot
/// tests store and compare.
pub fn feature_collection_bytes(features: Vec<Feature>) -> Vec<u8> {
    serde_json::to_vec_pretty(&FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    })
    .expect("serializes")
}
