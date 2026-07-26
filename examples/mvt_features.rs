//! Slice arbitrary **MVT-style features** into tiles while preserving each feature's
//! attributes — an optional integer **id** and a map of key/value **properties** — then emit the
//! per-tile pieces as GeoJSON.
//!
//! Feature attributes live at the *feature* level, not the vertex level, so they ride on the
//! slicer's second generic axis `A` (here [`Attrs`]) rather than in the vertex payload. You attach
//! one with [`add_feature_with`], and read it back per tile-piece with [`FeatureView::attr`] — no
//! per-vertex bookkeeping, and nothing to pay when you don't need it (`A` defaults to the zero-sized
//! `()`).
//!
//! A feature that gets split is handled two complementary ways, both shown here:
//! * **within one tile** — where the line left and re-entered, a `FeatureView` yields several runs,
//!   emitted as a single **`MultiLineString`** feature;
//! * **across tiles** — the feature reappears in each tile it touches, its attributes **duplicated**
//!   onto every piece.
//!
//! The two generic axes are orthogonal: to also carry a **per-vertex** M value, change the vertex
//! type from `Coord<i32>` to [`Measured<M>`](map_tile_toolkit::Measured) — the attribute handling
//! below stays exactly the same.
//!
//! Run `cargo run --example mvt_features` to print a GeoJSON `FeatureCollection` (global
//! coordinates) you can drop into <https://geojson.io>.

#![allow(clippy::pedantic, reason = "illustrative example, not library code")]

use std::collections::BTreeMap;

use geo_types::Coord;
use geojson::feature::Id;
use geojson::{Feature, FeatureCollection, GeometryValue, Position};
use map_tile_toolkit::{SlicerAll, TileId};
use serde_json::{Map, json};

/// A feature's non-geometric content: the MVT optional id and its string properties. This is the
/// slicer's per-feature attribute type `A`; it only has to be `Clone`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Attrs {
    id: Option<u64>,
    props: BTreeMap<&'static str, &'static str>,
}

/// One input feature: its attributes and a polyline of `(x, y)` in global tile-space.
struct Input {
    attrs: Attrs,
    line: Vec<Coord<i32>>,
}

/// A feature after slicing: which tile it landed in, the original attributes (duplicated per tile),
/// and its geometry there as one or more runs — several runs means the line left and re-entered this
/// tile. Coordinates are global (`local + tile · extent`).
struct TiledFeature {
    tile: TileId,
    attrs: Attrs,
    runs: Vec<Vec<Coord<i32>>>,
}

fn main() {
    const EXTENT: u32 = 25;

    let inputs = sample_inputs();

    // --- build: one feature per input line, each carrying its attributes ---
    let mut slicer = SlicerAll::<Coord<i32>, Attrs>::new(EXTENT, 0).expect("valid config");
    for input in &inputs {
        slicer
            .add_feature_with(&input.line, input.attrs.clone())
            .expect("valid geometry");
    }

    // --- read back: every per-tile piece already knows its feature's attributes ---
    let tiled = tile_features(&slicer, EXTENT);

    // --- emit: one GeoJSON feature per tiled piece (LineString, or MultiLineString if split) ---
    let features: Vec<Feature> = tiled.iter().map(to_geojson).collect();
    let fc = FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    };
    println!("{}", serde_json::to_string_pretty(&fc).expect("serializes"));

    eprintln!(
        "{} input features → {} tiled pieces across {} tiles",
        inputs.len(),
        tiled.len(),
        slicer.len(),
    );
}

/// Walk the sliced tiles and rebuild every per-tile feature, taking its attributes straight from the
/// [`FeatureView`](map_tile_toolkit::FeatureView) — no side table, no per-vertex handles.
fn tile_features(slicer: &SlicerAll<Coord<i32>, Attrs>, extent: u32) -> Vec<TiledFeature> {
    let mut out = Vec::new();
    for tile in slicer.iter_tiles() {
        let id = tile.tile_id();
        let origin = id.origin(extent).expect("tile in range");
        for view in tile.iter_features() {
            let runs = view
                .iter_polylines()
                .map(|run| run.iter().map(|&c| c + origin).collect())
                .collect();
            out.push(TiledFeature {
                tile: id,
                attrs: view.attr().clone(),
                runs,
            });
        }
    }
    out
}

/// Convert one tiled piece to a GeoJSON feature: a `LineString` for a single run or a
/// `MultiLineString` when the line was split within the tile. The optional MVT id becomes the
/// GeoJSON feature `id`; the properties (plus the tile address) become its `properties`.
fn to_geojson(tf: &TiledFeature) -> Feature {
    let position = |c: &Coord<i32>| Position::from([f64::from(c.x), f64::from(c.y)]);
    let run = |r: &Vec<Coord<i32>>| r.iter().map(position).collect::<Vec<_>>();
    let geometry = if tf.runs.len() == 1 {
        GeometryValue::LineString {
            coordinates: run(&tf.runs[0]),
        }
    } else {
        GeometryValue::MultiLineString {
            coordinates: tf.runs.iter().map(run).collect(),
        }
    };

    let mut props = Map::new();
    props.insert(
        "tile".to_string(),
        json!(format!("{},{}", tf.tile.x, tf.tile.y)),
    );
    for (k, v) in &tf.attrs.props {
        props.insert((*k).to_string(), json!(v));
    }

    Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::new(geometry)),
        id: tf.attrs.id.map(|n| Id::Number(n.into())),
        properties: Some(props),
        foreign_members: None,
    }
}

/// A small dataset that exercises both split strategies:
/// * `Main St` spans several tiles → its attributes are duplicated per tile;
/// * `River` staples out of tile (0,0) and back → one tile holds it as a `MultiLineString`;
/// * `Pier` has no id and stays in a single tile → a plain `LineString`.
fn sample_inputs() -> Vec<Input> {
    let line = |pts: &[(i32, i32)]| pts.iter().map(|&(x, y)| Coord { x, y }).collect();
    vec![
        Input {
            attrs: Attrs {
                id: Some(1),
                props: BTreeMap::from([("name", "Main St"), ("kind", "primary")]),
            },
            // Long diagonal crossing tiles (0,0), (1,0/1), (2,1), (3,1).
            line: line(&[(5, 5), (30, 20), (55, 30), (80, 45)]),
        },
        Input {
            attrs: Attrs {
                id: None,
                props: BTreeMap::from([("name", "River")]),
            },
            // A staple out of tile (0,0) into (0,1) and back — the top runs along (0,1) for three
            // vertices, so tile (0,0) drops two segments and keeps two separate runs → one
            // MultiLineString feature there.
            line: line(&[(8, 15), (8, 40), (13, 40), (18, 40), (18, 15)]),
        },
        Input {
            attrs: Attrs {
                id: Some(7),
                props: BTreeMap::from([("name", "Pier"), ("kind", "path")]),
            },
            // Stays inside tile (0,0): a plain single-run LineString.
            line: line(&[(3, 3), (18, 8)]),
        },
    ]
}
