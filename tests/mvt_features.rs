//! Preserving arbitrary **MVT-style feature attributes** — an optional id and a key/value property
//! map — through slicing, via the slicer's per-feature attribute axis `A` (see
//! `examples/mvt_features.rs`). Attributes are attached with `add_feature_with` and read back with
//! [`FeatureView::attr`], independent of the vertex type: the final test proves the attribute axis
//! and a per-vertex M value (`Measured`) compose without interfering.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::BTreeMap;

use geo_types::Coord;
use map_tile_toolkit::{Measured, SlicerAll, SlicerOne, TileId};

/// A feature's non-geometric content: an optional MVT id and its properties — the per-feature
/// attribute type `A`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attrs {
    id: Option<u64>,
    props: BTreeMap<&'static str, &'static str>,
}

impl Attrs {
    fn new(id: Option<u64>, props: &[(&'static str, &'static str)]) -> Self {
        Self {
            id,
            props: props.iter().copied().collect(),
        }
    }
}

/// One input feature: attributes plus a polyline of global `(x, y)` vertices.
type Input = (Attrs, Vec<Coord<i32>>);

/// One feature after slicing: the tile, the (duplicated) attributes read from `FeatureView::attr`,
/// and its runs in that tile as global coordinates.
#[derive(Debug, Clone, PartialEq)]
struct Tiled {
    tile: TileId,
    attrs: Attrs,
    runs: Vec<Vec<Coord<i32>>>,
}

fn coords(pts: &[(i32, i32)]) -> Vec<Coord<i32>> {
    pts.iter().map(|&(x, y)| Coord { x, y }).collect()
}

/// Slice `inputs` with a fresh attributed [`SlicerAll`], then rebuild the per-tile features, reading
/// each piece's attribute directly off the view.
fn slice(extent: u32, inputs: &[Input]) -> Vec<Tiled> {
    let mut slicer = SlicerAll::<Coord<i32>, Attrs>::new(extent, 0).expect("valid config");
    for (attrs, line) in inputs {
        slicer.add_feature_with(line, attrs.clone()).expect("slice");
    }

    let mut out = Vec::new();
    for tile in slicer.iter_tiles() {
        let id = tile.tile_id();
        let origin = id.origin(extent).expect("tile in range");
        for view in tile.iter_features() {
            let runs = view
                .iter_polylines()
                .map(|run| run.iter().map(|&c| c + origin).collect())
                .collect();
            out.push(Tiled {
                tile: id,
                attrs: view.attr().clone(),
                runs,
            });
        }
    }
    out
}

/// Every vertex appearing in any run of any tiled piece, deduplicated.
fn all_vertices(tiled: &[Tiled]) -> std::collections::BTreeSet<(i32, i32)> {
    tiled
        .iter()
        .flat_map(|t| t.runs.iter().flatten().map(|c| (c.x, c.y)))
        .collect()
}

#[test]
fn attributes_duplicate_across_tiles() {
    let attrs = Attrs::new(Some(42), &[("name", "Main St"), ("kind", "primary")]);
    let line = coords(&[(5, 5), (30, 20), (55, 30)]);
    let tiled = slice(25, &[(attrs.clone(), line.clone())]);

    // The single feature spans several tiles; each tile gets its own piece with the *same*
    // attributes (duplicated), never merged or dropped.
    assert!(tiled.len() >= 2, "feature should span multiple tiles");
    assert!(
        tiled.iter().all(|t| t.attrs == attrs),
        "every tiled piece keeps the original id + properties"
    );

    // Every original vertex survives verbatim somewhere.
    let seen = all_vertices(&tiled);
    for c in &line {
        assert!(
            seen.contains(&(c.x, c.y)),
            "original vertex {c:?} preserved"
        );
    }
}

#[test]
fn split_within_a_tile_becomes_one_multi_run_feature() {
    // A staple that leaves tile (0,0), runs along (0,1) for three vertices, and returns — two
    // dropped segments, so tile (0,0) keeps two separate runs under a single feature (rendered as a
    // MultiLineString). The attribute rides along untouched, including `id: None`.
    let attrs = Attrs::new(None, &[("name", "River")]);
    let line = coords(&[(8, 15), (8, 40), (13, 40), (18, 40), (18, 15)]);
    let tiled = slice(25, &[(attrs.clone(), line)]);

    let in_00: Vec<&Tiled> = tiled
        .iter()
        .filter(|t| t.tile == TileId::new(0, 0))
        .collect();
    assert_eq!(in_00.len(), 1, "one feature (not two) in tile (0,0)");
    let piece = in_00[0];
    assert_eq!(piece.attrs, attrs, "id: None and props preserved");
    assert_eq!(
        piece.runs.len(),
        2,
        "the split yields two runs → MultiLineString"
    );
}

#[test]
fn feature_attrs_stay_correct_when_features_interleave() {
    // Two features sharing the same tiles. Each tiled piece must carry its own feature's attributes;
    // a crossed handle would surface as the wrong `attr()`.
    let p = Attrs::new(Some(1), &[("name", "P")]);
    let q = Attrs::new(Some(2), &[("name", "Q")]);
    let tiled = slice(
        25,
        &[
            (p.clone(), coords(&[(2, 2), (30, 8), (55, 12)])),
            (q.clone(), coords(&[(2, 6), (30, 12), (55, 18)])),
        ],
    );

    assert!(tiled.iter().all(|t| t.attrs == p || t.attrs == q));
    assert!(tiled.iter().any(|t| t.attrs == p));
    assert!(tiled.iter().any(|t| t.attrs == q));
    assert!(
        tiled.iter().filter(|t| t.attrs == p).count() >= 2,
        "P should appear in several tiles, each with P's attrs"
    );
}

#[test]
fn no_attribute_channel_is_still_usable() {
    // The default `A = ()`: `add_feature` needs no attribute and `attr()` yields `&()`.
    let mut slicer = SlicerAll::new(25, 0).expect("valid config");
    slicer
        .add_feature(coords(&[(5, 5), (60, 40)]))
        .expect("slice");
    for tile in slicer.iter_tiles() {
        for f in tile.iter_features() {
            assert_eq!(f.attr(), &());
        }
    }

    // SlicerOne carries the same two entry points.
    let mut one = SlicerOne::<Coord<i32>, Attrs>::new(25, 0, TileId::new(0, 0)).expect("cfg");
    one.add_feature_with(coords(&[(5, 5), (20, 20)]), Attrs::new(Some(9), &[]))
        .expect("slice");
    let got: Vec<Attrs> = one.iter_features().map(|f| f.attr().clone()).collect();
    assert_eq!(got, vec![Attrs::new(Some(9), &[])]);
}

#[test]
fn per_vertex_m_and_per_feature_attrs_compose() {
    // The vertex axis (Measured<M>) and the attribute axis (Attrs) are independent: a
    // `SlicerAll<Measured<i32>, Attrs>` carries both, neither taxing the other.
    let attrs = Attrs::new(Some(100), &[("name", "trail")]);
    let mut slicer = SlicerAll::<Measured<i32>, Attrs>::new(25, 0).expect("cfg");
    slicer
        .add_feature_with(
            [
                Measured::new(5, 5, 1000),
                Measured::new(30, 20, 2000),
                Measured::new(55, 30, 3000),
            ],
            attrs.clone(),
        )
        .expect("slice");

    let mut pieces = 0;
    for tile in slicer.iter_tiles() {
        for f in tile.iter_features() {
            // Per-feature attribute, duplicated onto every tile.
            assert_eq!(f.attr(), &attrs);
            // Per-vertex M values ride through untouched on the same vertices.
            for run in f.iter_polylines() {
                assert!(run.iter().all(|v| [1000, 2000, 3000].contains(&v.m)));
            }
            pieces += 1;
        }
    }
    assert!(pieces >= 2, "the trail should span multiple tiles");
}
