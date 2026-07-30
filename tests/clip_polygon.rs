//! Insta GeoJSON snapshots for the integer **polygon** slicer, mirroring `clip_polyline.rs`.
//!
//! Each fixture in `tests/polygons/fixtures/*.geojson` is a `FeatureCollection` with one or more
//! `Polygon` features (whole-number coordinates in valid lon/lat range so fixtures and snapshots
//! render on a map). No holes yet. Every fixture is clipped, one tile at a time with
//! [`PolygonSlicerOne`], across the whole tile span the polygon could reach (padded by one tile) —
//! this single per-tile pass already fills tiles that sit fully inside the polygon (the containment
//! case) as well as border tiles.
//!
//! The surviving per-tile rings are snapshotted as a `FeatureCollection`: the input polygon(s) first
//! (yellow), then one filled `Polygon` feature per per-tile ring, colored by tile parity. Regenerate
//! with `just bless`.
//!
//! Every fixture is snapshotted at two buffer sizes: `polygons/snapshots/` (buffer 0) and
//! `polygons/snapshots-5/` (buffer 5, each tile box grown 5 units per side).

#![allow(clippy::pedantic, reason = "test/inspection tool")]

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::Coord;
use insta::assert_binary_snapshot;
use map_tile_toolkit::{PolygonSlicerOne, TileId};

mod support;

use crate::support::{EXTENT, FixturePolygon};

/// Buffer sizes each fixture is snapshotted at, paired with the directory to write into.
fn buffers() -> [(u16, &'static str); 2] {
    [(0, "polygons/snapshots"), (5, "polygons/snapshots-5")]
}

mod files {
    use test_each_file::test_each_path;

    // One test per input fixture.
    test_each_path! { for ["geojson"] in "./tests/polygons/fixtures" => super::snapshot_polygon_fixture }
}

/// One clipped polygon piece in a tile: its exterior ring plus any surviving holes, in the **global**
/// frame (tile-local `+ tile · extent`), ready to render.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TiledFeature {
    exterior: Vec<Coord<i32>>,
    holes: Vec<Vec<Coord<i32>>>,
}

/// Clip every polygon (exterior + holes) to a single `tile` with a fresh [`PolygonSlicerOne`],
/// returning the surviving per-tile features in the global frame.
fn clip_tile(polygons: &[FixturePolygon], buffer: u16, tile: TileId) -> Vec<TiledFeature> {
    let mut slicer = PolygonSlicerOne::<Coord<i32>>::new(EXTENT, buffer, tile).expect("valid config");
    for p in polygons {
        let holes: Vec<&[Coord<i32>]> = p.holes.iter().map(Vec::as_slice).collect();
        slicer.add_feature(&p.exterior, &holes).expect("clip");
    }
    let origin = tile.origin(EXTENT).expect("tile in range");
    let globalize = |ring: &[Coord<i32>]| ring.iter().map(|&c| c + origin).collect::<Vec<_>>();
    slicer
        .iter_features()
        .map(|f| {
            let mut exterior = Vec::new();
            let mut holes = Vec::new();
            for r in f.iter_rings() {
                if r.is_hole() {
                    holes.push(globalize(r.vertices()));
                } else {
                    exterior = globalize(r.vertices());
                }
            }
            TiledFeature { exterior, holes }
        })
        .collect()
}

/// Clip `polygons` one tile at a time across the padded tile span, collecting every non-empty per-tile
/// result keyed by tile (global coordinates).
fn clip_all_tiles(polygons: &[FixturePolygon], buffer: u16) -> BTreeMap<TileId, Vec<TiledFeature>> {
    let rings: Vec<Vec<Coord<i32>>> = polygons.iter().map(|p| p.exterior.clone()).collect();
    let (lo, hi) = support::padded_tile_span(&rings);
    let mut out = BTreeMap::new();
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            let piece = clip_tile(polygons, buffer, tile);
            if !piece.is_empty() {
                out.insert(tile, piece);
            }
        }
    }
    out
}

/// A copy of `polygons` with every vertex (exterior and holes) repeated once — consecutive duplicates
/// the slicer must transparently drop, so clipping the copy yields the same result as the original.
fn duplicate_vertices(polygons: &[FixturePolygon]) -> Vec<FixturePolygon> {
    let dup = |ring: &[Coord<i32>]| ring.iter().flat_map(|&c| [c, c]).collect::<Vec<_>>();
    polygons
        .iter()
        .map(|p| FixturePolygon {
            exterior: dup(&p.exterior),
            holes: p.holes.iter().map(|h| dup(h)).collect(),
        })
        .collect()
}

fn snapshot_polygon_fixture([path]: [&Path; 1]) {
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    let polygons = support::load_polygon_fixture(path);
    for (buffer, dir) in buffers() {
        snapshot_at_buffer(stem, &polygons, buffer, dir);
    }
}

fn snapshot_at_buffer(stem: &str, polygons: &[FixturePolygon], buffer: u16, dir: &str) {
    let per_tile = clip_all_tiles(polygons, buffer);

    // Duplicating every vertex must not change the clip (consecutive dups are dropped).
    let duped = clip_all_tiles(&duplicate_vertices(polygons), buffer);
    assert_eq!(
        duped, per_tile,
        "duplicating every vertex changed the clip for {stem} (buffer {buffer})"
    );

    // Build the snapshot: input polygons (yellow), then one filled piece per tile feature (holes
    // punched out).
    let mut features: Vec<_> = polygons
        .iter()
        .map(|p| support::input_polygon(&p.exterior, &p.holes))
        .collect();
    for (&tile, tile_features) in &per_tile {
        for f in tile_features {
            features.push(support::tile_polygon(&f.exterior, &f.holes, tile));
        }
    }
    let bytes = support::feature_collection_bytes(features);

    insta::with_settings!({
        snapshot_path => dir,
        prepend_module_to_snapshot => false,
    }, {
        let name = if buffer > 0 {
            format!("{stem}-{buffer}.geojson")
        } else {
            format!("{stem}.geojson")
        };
        assert_binary_snapshot!(&name, bytes);
    });
}
