//! Shared helpers for the snapshot tests and the benchmarks: GeoJSON fixture loading/parsing and
//! feature building. Included by `tests/clip_polyline.rs` (`mod support;`) and by
//! `benches/slicing.rs` (via `#[path = "../tests/support/mod.rs"]`).

#![allow(
    dead_code,
    reason = "shared across the test and bench crates; not every helper is used in each"
)]

use std::fs;
use std::path::Path;

use geo_types::{Coord, Geometry, LineString, MultiLineString, Polygon};
use geojson::{Feature, FeatureCollection, GeoJson, GeometryValue, JsonObject, JsonValue};
use map_tile_toolkit::{FillRun, PolygonSlicerAll, PolygonSlicerOne, SlicerAll, SlicerOne, TileId};
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

impl Cfg {
    /// A fresh all-tiles polygon slicer for this config (panics on a bad literal config).
    #[must_use]
    pub fn poly_all(self) -> PolygonSlicerAll<Coord<i32>> {
        PolygonSlicerAll::new(self.extent, self.buffer)
            .expect("invalid slicer config in test support")
    }

    /// A fresh single-tile polygon slicer bound to `tile` (panics on a bad literal config).
    #[must_use]
    pub fn poly_one(self, tile: TileId) -> PolygonSlicerOne<Coord<i32>> {
        PolygonSlicerOne::new(self.extent, self.buffer, tile)
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

/// Tile extent for the small fixtures (matches the `tests/polylines/fixtures/grid.geojson` grid).
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

/// Every `tests/polylines/fixtures/*.geojson` as `(name, polylines)`, sorted by name for stable ordering.
pub fn load_all_fixtures() -> Vec<(String, Polylines)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/polylines/fixtures");
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

/// A large, deterministic polygon for benchmarking: a jagged blob of ~3.6k vertices (radius
/// 150–200 around `(220, 270)`, so roughly the [`big_polyline`] footprint) with a ~900-vertex hole
/// — many vertices, many edge tiles, and a wide fully covered interior.
#[must_use]
pub fn big_polygon() -> FixturePolygon {
    let blob = |n: i32, (cx, cy): (f64, f64), r: f64, jitter: f64| -> Vec<Coord<i32>> {
        (0..n)
            .map(|k| {
                let angle = std::f64::consts::TAU * f64::from(k) / f64::from(n);
                let radius = r + jitter * f64::from((k * 37) % 50) / 50.0;
                Coord {
                    x: (cx + radius * angle.cos()).round() as i32,
                    y: (cy + radius * angle.sin()).round() as i32,
                }
            })
            .collect()
    };
    let mut hole = blob(900, (240.0, 250.0), 40.0, 12.0);
    hole.reverse(); // holes wind opposite to the exterior
    FixturePolygon {
        exterior: blob(3600, (220.0, 270.0), 150.0, 50.0),
        holes: vec![hole],
    }
}

/// A z14-like "ocean": a square covering 16384 × 16384 tiles of extent 4096 (2^28 tiles) with a
/// 21 × 21-tile hole, so nearly every tile is fully covered.
#[must_use]
pub fn huge_square() -> FixturePolygon {
    const N: i32 = 1 << 14;
    const E: i32 = 4096;
    let ring = |lo: i32, hi: i32| {
        vec![
            Coord { x: lo, y: lo },
            Coord { x: hi, y: lo },
            Coord { x: hi, y: hi },
            Coord { x: lo, y: hi },
        ]
    };
    let mut hole = ring((N / 2 - 10) * E + 2000, (N / 2 + 10) * E + 2000);
    hole.reverse();
    FixturePolygon {
        exterior: ring(100, N * E - 100),
        holes: vec![hole],
    }
}

/// A 100×100-tile square (extent 256) with 2000 small holes, one per tile: the shape of a forest or
/// lake multipolygon with many inner rings, whose cost must follow its hits, not its ring count.
#[must_use]
pub fn many_holes() -> FixturePolygon {
    const E: i32 = 256;
    let square = |x: i32, y: i32, side: i32| {
        vec![
            Coord { x, y },
            Coord { x, y: y + side },
            Coord {
                x: x + side,
                y: y + side,
            },
            Coord { x: x + side, y },
        ]
    };
    FixturePolygon {
        exterior: square(0, 0, 100 * E),
        holes: (0..2000)
            .map(|i| square((i % 98 + 1) * E + 10, (i / 98 + 1) * E + 10, 20))
            .collect(),
    }
}

/// The rings of a polygon, exterior first — the per-polygon input [`PolygonSlicerAll`] takes.
#[must_use]
pub fn rings(p: &FixturePolygon) -> Vec<&[Coord<i32>]> {
    std::iter::once(p.exterior.as_slice())
        .chain(p.holes.iter().map(Vec::as_slice))
        .collect()
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

pub fn feature_line(run: &[Coord<i32>], role: &str) -> Feature {
    styled_line(run, role, "#261fb5", 2)
}

pub fn input_feature(run: &[Coord<i32>]) -> Feature {
    styled_line(run, "input", "#f6fd31", 15)
}

// ---- Polygon fixtures & rendering (shared by the polygon snapshot tests) ----

/// One polygon fixture feature: an exterior ring plus zero or more interior rings (holes), in integer
/// coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixturePolygon {
    pub exterior: Vec<Coord<i32>>,
    pub holes: Vec<Vec<Coord<i32>>>,
}

/// A ring's integer coordinates (whole-number lon/lat truncated to `i32`).
fn ring_i32(ls: &LineString<f64>) -> Vec<Coord<i32>> {
    ls.0.iter()
        .map(|c| Coord {
            x: c.x as i32,
            y: c.y as i32,
        })
        .collect()
}

/// Parse a polygon fixture into its features: a `FeatureCollection` of `Polygon` or `MultiPolygon`
/// features, each yielding its polygons (an exterior ring plus optional interior rings / holes).
pub fn load_polygon_features(path: &Path) -> Vec<Vec<FixturePolygon>> {
    let text = fs::read_to_string(path).expect("readable fixture");
    let GeoJson::FeatureCollection(fc) = text.parse().expect("valid GeoJSON") else {
        panic!(
            "polygon fixture must be a FeatureCollection: {}",
            path.display()
        );
    };
    let polygon = |p: &Polygon<f64>| FixturePolygon {
        exterior: ring_i32(p.exterior()),
        holes: p.interiors().iter().map(ring_i32).collect(),
    };
    let features: Vec<Vec<FixturePolygon>> = fc
        .features
        .into_iter()
        .map(|f| {
            let geom = Geometry::<f64>::try_from(f.geometry.expect("feature has geometry"))
                .expect("geometry converts");
            match geom {
                Geometry::Polygon(p) => vec![polygon(&p)],
                Geometry::MultiPolygon(mp) => mp.0.iter().map(polygon).collect(),
                other => panic!(
                    "polygon fixtures must use Polygon or MultiPolygon features, not {other:?} ({})",
                    path.display()
                ),
            }
        })
        .collect();
    assert!(
        !features.is_empty(),
        "polygon fixture has no features: {}",
        path.display()
    );
    features
}

/// Every polygon of a fixture, a `MultiPolygon` feature contributing each of its parts — for the
/// single-tile slicer, which takes one polygon per feature.
pub fn load_polygon_fixture(path: &Path) -> Vec<FixturePolygon> {
    load_polygon_features(path).into_iter().flatten().collect()
}

/// A fill run's tiles as one rectangle over their core cells (green), tagged `fill y/x0..x1`.
pub fn fill_run_polygon(run: &FillRun, extent: u32) -> Feature {
    let e = extent as i32;
    let (x0, x1, y0, y1) = (
        run.x.start * e,
        run.x.end * e - 1,
        run.y * e,
        run.y * e + e - 1,
    );
    let rect = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)].map(|(x, y)| Coord { x, y });
    let role = format!("fill {}/{}..{}", run.y, run.x.start, run.x.end);
    styled_polygon(&rect, &[], &role, "#1fb53a", "#0b6b1f")
}

/// Inclusive tile-coordinate bounds covering every vertex of `rings`, padded by one tile so a per-tile
/// scan also visits the empty tiles just outside the geometry. Works for polyline or polygon rings.
#[must_use]
pub fn padded_tile_span(rings: &[Vec<Coord<i32>>]) -> (TileId, TileId) {
    let mut lo = TileId::new(i32::MAX, i32::MAX);
    let mut hi = TileId::new(i32::MIN, i32::MIN);
    let e = EXTENT as i32;
    for ring in rings {
        for &c in ring {
            let (tx, ty) = (c.x.div_euclid(e), c.y.div_euclid(e));
            lo = TileId::new(lo.x.min(tx), lo.y.min(ty));
            hi = TileId::new(hi.x.max(tx), hi.y.max(ty));
        }
    }
    (
        TileId::new(lo.x - 1, lo.y - 1),
        TileId::new(hi.x + 1, hi.y + 1),
    )
}

/// A ring's `f64` coordinates for GeoJSON output.
fn ring_f64(ring: &[Coord<i32>]) -> LineString<f64> {
    LineString(
        ring.iter()
            .map(|c| Coord {
                x: f64::from(c.x),
                y: f64::from(c.y),
            })
            .collect(),
    )
}

/// A filled GeoJSON `Polygon` feature (exterior plus `holes`, so holes render punched out), styled so
/// it renders on a map and tagged with `role`.
fn styled_polygon(
    exterior: &[Coord<i32>],
    holes: &[Vec<Coord<i32>>],
    role: &str,
    fill: &str,
    stroke: &str,
) -> Feature {
    let poly = Polygon::new(
        ring_f64(exterior),
        holes.iter().map(|h| ring_f64(h)).collect(),
    );
    feature(
        &Geometry::Polygon(poly),
        vec![
            ("role", json!(role)),
            ("fill", json!(fill)),
            ("fill-opacity", json!(0.35)),
            ("stroke", json!(stroke)),
            ("stroke-width", json!(2)),
        ],
    )
}

/// The original input polygon (bright yellow), the reference every per-tile piece should tile back to.
pub fn input_polygon(exterior: &[Coord<i32>], holes: &[Vec<Coord<i32>>]) -> Feature {
    styled_polygon(exterior, holes, "input", "#f6fd31", "#b5a300")
}

/// One tile's clipped polygon piece (exterior + holes), filled and colored by tile parity so neighbors
/// contrast, tagged `tile x/y`.
pub fn tile_polygon(exterior: &[Coord<i32>], holes: &[Vec<Coord<i32>>], tile: TileId) -> Feature {
    let role = format!("tile {}/{}", tile.x, tile.y);
    let fill = if (tile.x + tile.y).rem_euclid(2) == 0 {
        "#261fb5"
    } else {
        "#b5211f"
    };
    styled_polygon(exterior, holes, &role, fill, "#111111")
}

/// Serialize `features` as a pretty-printed GeoJSON `FeatureCollection` — the byte form the snapshot
/// tests store and compare. Features are sorted by their `role` property: inputs, then tiles, then
/// fills, then any other role, each group by role text (stable, so pieces sharing a role keep the
/// order they were produced in).
pub fn feature_collection_bytes(mut features: Vec<Feature>) -> Vec<u8> {
    features.sort_by_cached_key(|f| {
        let role = f
            .property("role")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let group = if role == "input" {
            0
        } else if role.starts_with("tile ") {
            1
        } else if role.starts_with("fill ") {
            2
        } else {
            3
        };
        (group, role.to_owned())
    });
    serde_json::to_vec_pretty(&FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    })
    .expect("serializes")
}
