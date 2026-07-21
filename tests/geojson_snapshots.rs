//! Visual regression snapshots: slice each GeoJSON fixture and snapshot the result as a
//! binary `.geojson` snapshot.
//!
//! Each fixture produces **two** snapshots, one per slicing entry point, each in its own tree:
//! * `snapshots/slice_all_tiles/<kind>/<name>.geojson` — the batch [`slice_all_tiles`] (eager
//!   `stripe` slicer).
//! * `snapshots/slice_tile/<kind>/<name>.geojson` — [`slice_tile`] (single-tile rectangle clip)
//!   called once per tile that the batch produced, so the two snapshots cover the same tiles
//!   through the two different code paths (which clip lines differently: the batch splits at tile
//!   boundaries, the single-tile path keeps original vertices).
//!
//! Each snapshot is a GeoJSON `FeatureCollection` whose **first** feature is the original input
//! geometry (thick dark outline) followed by **one feature per tile** (thinner, alternating
//! fill colors), all reprojected back to WGS84 lon/lat. Because the snapshot file ends in
//! `.geojson`, GitHub (and geojson.io, QGIS, kepler.gl) renders it directly on a map — so a
//! snapshot diff is a visual diff of the clipping output. The [simplestyle-spec] properties
//! (`stroke`/`fill`/…) drive the colors.
//!
//! Fixtures live in `tests/fixtures/<kind>/*.geojson` (lon/lat), each carrying a top-level
//! `"zoom"` member — a tree shared with `clip_polyline.rs`, whose `bbox`-bearing fixtures are
//! skipped here. Regenerate snapshots with `just bless` (or `INSTA_UPDATE=always cargo test
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
use geojson::{Feature, FeatureCollection, GeoJson, JsonValue};
use insta::assert_binary_snapshot;
use map_tile_toolkit::{SliceOptions, TileId, slice_all_tiles, slice_tile};
use serde_json::json;

mod support;
use support::feature;

mod files {
    use test_each_file::test_each_path;

    use super::snapshot_one_fixture;

    // Generate one test per fixture.
    // The fixture tree is shared by geometry kind; fixtures carrying a `bbox`
    test_each_path! { for ["geojson"] in "./tests/fixtures/polyline" as polyline => snapshot_one_fixture }
    test_each_path! { for ["geojson"] in "./tests/fixtures/polygon" as polygon => snapshot_one_fixture }
    test_each_path! { for ["geojson"] in "./tests/fixtures/polygon_with_holes" as polygon_with_holes => snapshot_one_fixture }
}

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

/// Parse a GeoJSON fixture into a single geometry (a `GeometryCollection` if it holds several)
/// plus its `"zoom"` member (default 3). Returns `None` for a fixture carrying a `bbox` — a
/// polyline-clip fixture that belongs to `clip_polyline.rs`.
fn load_fixture(path: &Path) -> Option<(Geometry<f64>, u8)> {
    let text = fs::read_to_string(path).expect("readable fixture");
    let gj: GeoJson = text.parse().expect("valid GeoJSON");

    let (features, foreign) = match gj {
        GeoJson::FeatureCollection(fc) if fc.bbox.is_some() => return None,
        GeoJson::FeatureCollection(fc) => (fc.features, fc.foreign_members),
        GeoJson::Feature(f) => (vec![f], None),
        GeoJson::Geometry(g) => {
            return Some((Geometry::<f64>::try_from(g).expect("geometry converts"), 3));
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
    Some((geom, zoom))
}

/// The input geometry as the first feature: a thick dark outline distinct from the slices.
fn input_feature(input: &Geometry<f64>) -> Feature {
    feature(
        input,
        vec![
            ("role", json!("input")),
            ("stroke", json!("#111111")),
            ("stroke-width", json!(3)),
            ("fill", json!("#111111")),
            ("fill-opacity", json!(0.03)),
        ],
    )
}

/// One tile's slice, reprojected to lon/lat, colored by tile parity so neighbors contrast.
fn tile_feature(id: TileId, sliced: &Geometry<i32>) -> Feature {
    let lonlat = sliced
        .map_coords(|c| tile_local_to_mercator(id, c))
        .map_coords(mercator_to_lonlat);
    let color = if (id.x + id.y).is_multiple_of(2) {
        "#1f77b4"
    } else {
        "#ff7f0e"
    };
    feature(
        &lonlat,
        vec![
            ("role", json!("tile")),
            ("tile", json!(format!("{}/{}/{}", id.z, id.x, id.y))),
            ("stroke", json!(color)),
            ("stroke-width", json!(1)),
            ("fill", json!(color)),
            ("fill-opacity", json!(0.4)),
        ],
    )
}

/// Build the output `FeatureCollection`: the input first, then one feature per tile slice.
fn build_fc(input: &Geometry<f64>, tiles: &[(TileId, Geometry<i32>)]) -> FeatureCollection {
    let mut features = vec![input_feature(input)];
    for (id, sliced) in tiles {
        features.push(tile_feature(*id, sliced));
    }
    FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    }
}

/// Slice one fixture through both entry points and snapshot each result. Skips polyline-clip
/// fixtures (a `bbox` member) that share the type dirs; those belong to `clip_polyline.rs`.
fn snapshot_one_fixture([path]: [&Path; 1]) {
    let Some((geom, zoom)) = load_fixture(path) else {
        return;
    };
    let kind = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .expect("fixture lives in a kind dir");
    let stem = path.file_stem().expect("stem").to_str().expect("utf8");
    let opts = SliceOptions::new(NonZeroU32::new(EXTENT).expect("nonzero"), BUFFER_PX);
    let mercator = geom.map_coords(lonlat_to_mercator);

    // Batch path: slice into every tile at once with the eager stripe slicer.
    let batch: Vec<_> = slice_all_tiles(&mercator, zoom, opts).collect();
    let bytes = serde_json::to_vec_pretty(&build_fc(&geom, &batch)).expect("serializes");
    insta::with_settings!({
        snapshot_path => format!("snapshots/slice_all_tiles/{kind}"),
        prepend_module_to_snapshot => false,
    }, {
        assert_binary_snapshot!(&format!("{stem}.geojson"), bytes);
    });

    // Single-tile path: re-slice each tile the batch produced, one `slice_tile` per id.
    let single: Vec<(TileId, Geometry<i32>)> = batch
        .iter()
        .filter_map(|(id, _)| slice_tile(&mercator, *id, opts).map(|g| (*id, g)))
        .collect();
    let bytes = serde_json::to_vec_pretty(&build_fc(&geom, &single)).expect("serializes");
    insta::with_settings!({
        snapshot_path => format!("snapshots/slice_tile/{kind}"),
        prepend_module_to_snapshot => false,
    }, {
        assert_binary_snapshot!(&format!("{stem}.geojson"), bytes);
    });
}
