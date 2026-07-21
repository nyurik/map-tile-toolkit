//! Emit a GeoJSON `FeatureCollection` for eyeballing the slicer in any GeoJSON viewer.
//!
//! Reprojects the original geometry, every per-tile slice, and the tile grid back to
//! WGS84 lon/lat and prints one styled `FeatureCollection` to stdout. Paste the output into
//! <https://geojson.io>, drop it into QGIS, or load it in kepler.gl to see the original
//! overlaid with its slices and the tiles they land in. The [simplestyle-spec] properties
//! (`stroke`/`fill`/…) make each role visually distinct in geojson.io.
//!
//! Usage: `cargo run --example visualize -- [OPTIONS]`
//! * `--wkt <WKT>`      inline geometry in lon/lat (WGS84), e.g. `"POLYGON((...))"`
//! * `--file <PATH>`    read the WKT geometry from a file instead
//! * `--zoom <Z>`       zoom to slice at (default 4)
//! * `--tile <Z/X/Y>`   also highlight a single-tile retrieval (`slice_tile`)
//! * `--extent <N>`     integer tile grid side (default 4096)
//! * `--buffer <N>`     clip margin in grid units (default 64)
//!
//! With no `--wkt`/`--file`, a sample square-with-a-hole over Europe is used.
//!
//! [simplestyle-spec]: https://github.com/mapbox/simplestyle-spec

#![allow(clippy::pedantic, reason = "example/inspection tool")]

use std::env;
use std::f64::consts::PI;
use std::num::NonZeroU32;

use geo::MapCoords as _;
use geo_types::{Coord, Geometry, LineString, Polygon};
use geojson::{Feature, FeatureCollection, GeometryValue, JsonObject, JsonValue};
use map_tile_toolkit::{SliceOptions, TileId, slice_all_tiles, slice_tile};
use serde_json::json;
use wkt::TryFromWkt as _;

/// Web Mercator plane width (meters), matching the crate's `EARTH_CIRCUMFERENCE`.
const CIRC: f64 = 40_075_016.685_578_5;
/// Half the plane width: coordinates span `-ORIGIN..=ORIGIN`.
const ORIGIN: f64 = CIRC / 2.0;
/// Sphere radius implied by the plane width, for the lon/lat <-> meters projection.
const R: f64 = ORIGIN / PI;

// --- projections ---------------------------------------------------------------------------

/// WGS84 lon/lat (degrees) -> Web Mercator (meters), the crate's input contract.
fn lonlat_to_mercator(c: Coord<f64>) -> Coord<f64> {
    Coord {
        x: R * c.x.to_radians(),
        y: R * (PI / 4.0 + c.y.to_radians() / 2.0).tan().ln(),
    }
}

/// Web Mercator (meters) -> WGS84 lon/lat (degrees), for rendering results on a real map.
fn mercator_to_lonlat(c: Coord<f64>) -> Coord<f64> {
    Coord {
        x: (c.x / R).to_degrees(),
        y: (2.0 * (c.y / R).exp().atan() - PI / 2.0).to_degrees(),
    }
}

/// Map a tile-local integer coordinate (`0..extent`, plus buffer) back to Web Mercator.
fn tile_local_to_mercator(tile: TileId, c: Coord<i32>, extent: f64) -> Coord<f64> {
    let tile_len = CIRC / f64::from(1u32 << tile.z);
    let min_x = -ORIGIN + f64::from(tile.x) * tile_len;
    let max_y = ORIGIN - f64::from(tile.y) * tile_len;
    Coord {
        x: min_x + f64::from(c.x) / extent * tile_len,
        y: max_y - f64::from(c.y) / extent * tile_len,
    }
}

// --- feature building ----------------------------------------------------------------------

/// Build a styled GeoJSON feature from a lon/lat geometry plus simplestyle properties.
fn feature(geom: &Geometry<f64>, props: Vec<(&str, JsonValue)>) -> Feature {
    let mut properties = JsonObject::new();
    for (k, v) in props {
        properties.insert(k.to_string(), v);
    }
    Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::new(GeometryValue::from(geom))),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    }
}

/// simplestyle stroke + fill properties for a role.
fn style(
    stroke: &str,
    fill: &str,
    fill_opacity: f64,
    width: f64,
) -> Vec<(&'static str, JsonValue)> {
    vec![
        ("stroke", json!(stroke)),
        ("stroke-width", json!(width)),
        ("fill", json!(fill)),
        ("fill-opacity", json!(fill_opacity)),
    ]
}

/// The lon/lat outline polygon of a tile (its bounds, without buffer).
fn tile_outline(tile: TileId) -> Geometry<f64> {
    let tile_len = CIRC / f64::from(1u32 << tile.z);
    let min_x = -ORIGIN + f64::from(tile.x) * tile_len;
    let max_y = ORIGIN - f64::from(tile.y) * tile_len;
    let (max_x, min_y) = (min_x + tile_len, max_y - tile_len);
    let ring = LineString(
        [
            (min_x, min_y),
            (max_x, min_y),
            (max_x, max_y),
            (min_x, max_y),
            (min_x, min_y),
        ]
        .map(|(x, y)| mercator_to_lonlat(Coord { x, y }))
        .to_vec(),
    );
    Geometry::Polygon(Polygon::new(ring, vec![]))
}

// --- input ---------------------------------------------------------------------------------

/// A square with a square hole over Europe (lon/lat), spanning several tiles at low zooms.
fn sample() -> Geometry<f64> {
    let exterior = LineString::from(vec![
        (-20.0, 30.0),
        (40.0, 30.0),
        (40.0, 60.0),
        (-20.0, 60.0),
        (-20.0, 30.0),
    ]);
    let hole = LineString::from(vec![
        (5.0, 42.0),
        (15.0, 42.0),
        (15.0, 50.0),
        (5.0, 50.0),
        (5.0, 42.0),
    ]);
    Geometry::Polygon(Polygon::new(exterior, vec![hole]))
}

struct Args {
    geom: Geometry<f64>,
    zoom: u8,
    tile: Option<TileId>,
    opts: SliceOptions,
}

fn parse_tile(s: &str) -> Option<TileId> {
    let parts: Vec<&str> = s.split('/').collect();
    let [z, x, y] = parts.as_slice() else {
        return None;
    };
    Some(TileId::new(
        x.parse().ok()?,
        y.parse().ok()?,
        z.parse().ok()?,
    ))
}

fn parse_args() -> Args {
    let mut geom = None;
    let mut zoom = 4u8;
    let mut tile = None;
    let mut extent = 4096u32;
    let mut buffer = 64u32;

    let mut it = env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut next = || it.next().expect("missing value for flag");
        match flag.as_str() {
            "--wkt" => geom = Some(Geometry::try_from_wkt_str(&next()).expect("valid WKT")),
            "--file" => {
                let path = next();
                let text = std::fs::read_to_string(&path).expect("readable WKT file");
                geom = Some(Geometry::try_from_wkt_str(&text).expect("valid WKT"));
            }
            "--zoom" => zoom = next().parse().expect("zoom is a small integer"),
            "--tile" => tile = Some(parse_tile(&next()).expect("tile is Z/X/Y")),
            "--extent" => extent = next().parse().expect("extent is a positive integer"),
            "--buffer" => buffer = next().parse().expect("buffer is a non-negative integer"),
            other => panic!("unknown flag: {other}"),
        }
    }

    Args {
        geom: geom.unwrap_or_else(sample),
        zoom,
        tile,
        opts: SliceOptions::new(
            NonZeroU32::new(extent).expect("extent must be nonzero"),
            buffer,
        ),
    }
}

// --- main ----------------------------------------------------------------------------------

fn main() {
    let Args {
        geom,
        zoom,
        tile,
        opts,
    } = parse_args();
    let extent = f64::from(opts.extent.get());
    let mercator = geom.map_coords(lonlat_to_mercator);

    let mut features = Vec::new();

    // The original geometry (already lon/lat), a translucent black outline.
    let mut props = style("#111111", "#111111", 0.04, 2.0);
    props.push(("role", json!("original")));
    features.push(feature(&geom, props));

    // Every slice, reprojected to lon/lat, colored by tile parity so neighbors contrast.
    let mut tiles = Vec::new();
    for (id, sliced) in slice_all_tiles(&mercator, zoom, opts) {
        tiles.push(id);
        let lonlat = sliced.map_coords(|c| tile_local_to_mercator(id, c, extent));
        let lonlat = lonlat.map_coords(mercator_to_lonlat);
        let color = if (id.x + id.y).is_multiple_of(2) {
            "#1f77b4"
        } else {
            "#ff7f0e"
        };
        let mut props = style(color, color, 0.35, 1.5);
        props.push(("role", json!("slice")));
        props.push(("tile", json!(format!("{}/{}/{}", id.z, id.x, id.y))));
        features.push(feature(&lonlat, props));
    }

    // The tile grid the geometry covered, thin gray outlines.
    for id in &tiles {
        let mut props = style("#999999", "#999999", 0.0, 1.0);
        props.push(("role", json!("tile-grid")));
        props.push(("tile", json!(format!("{}/{}/{}", id.z, id.x, id.y))));
        features.push(feature(&tile_outline(*id), props));
    }

    // Optional single-tile retrieval, highlighted in red.
    if let Some(id) = tile {
        if let Some(sliced) = slice_tile(&mercator, id, opts) {
            let lonlat = sliced
                .map_coords(|c| tile_local_to_mercator(id, c, extent))
                .map_coords(mercator_to_lonlat);
            let mut props = style("#d62728", "#d62728", 0.5, 2.5);
            props.push(("role", json!("single")));
            props.push(("tile", json!(format!("{}/{}/{}", id.z, id.x, id.y))));
            features.push(feature(&lonlat, props));
        } else {
            eprintln!("note: tile {id:?} has no slice of this geometry");
        }
    }

    let fc = FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    };

    eprintln!(
        "{} slice(s) across {} tile(s) at z{zoom}. Paste stdout into https://geojson.io",
        tiles.len(),
        tiles.len()
    );
    println!("{fc}");
}
