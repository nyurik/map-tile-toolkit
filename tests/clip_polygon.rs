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

use crate::support::EXTENT;

/// Buffer sizes each fixture is snapshotted at, paired with the directory to write into.
fn buffers() -> [(u16, &'static str); 2] {
    [(0, "polygons/snapshots"), (5, "polygons/snapshots-5")]
}

mod files {
    use test_each_file::test_each_path;

    // One test per input fixture.
    test_each_path! { for ["geojson"] in "./tests/polygons/fixtures" => super::snapshot_polygon_fixture }
}

/// Clip every polygon in `rings` to a single `tile` with a fresh [`PolygonSlicerOne`], returning the
/// surviving rings in the **global** frame (tile-local `+ tile · extent`), ready to render.
fn clip_tile(rings: &[Vec<Coord<i32>>], buffer: u16, tile: TileId) -> Vec<Vec<Coord<i32>>> {
    let mut slicer = PolygonSlicerOne::<Coord<i32>>::new(EXTENT, buffer, tile).expect("valid config");
    for exterior in rings {
        slicer.add_feature(exterior, &[]).expect("clip");
    }
    let origin = tile.origin(EXTENT).expect("tile in range");
    slicer
        .iter_features()
        .flat_map(|f| {
            f.iter_rings()
                .map(|r| r.vertices().iter().map(|&c| c + origin).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Clip `rings` one tile at a time across the padded tile span, collecting every non-empty per-tile
/// result keyed by tile (global coordinates).
fn clip_all_tiles(rings: &[Vec<Coord<i32>>], buffer: u16) -> BTreeMap<TileId, Vec<Vec<Coord<i32>>>> {
    let (lo, hi) = support::padded_tile_span(rings);
    let mut out = BTreeMap::new();
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            let piece = clip_tile(rings, buffer, tile);
            if !piece.is_empty() {
                out.insert(tile, piece);
            }
        }
    }
    out
}

/// A copy of `rings` with every vertex repeated once — consecutive duplicates the slicer must
/// transparently drop, so clipping the copy yields the same result as the original.
fn duplicate_vertices(rings: &[Vec<Coord<i32>>]) -> Vec<Vec<Coord<i32>>> {
    rings
        .iter()
        .map(|ring| ring.iter().flat_map(|&c| [c, c]).collect())
        .collect()
}

fn snapshot_polygon_fixture([path]: [&Path; 1]) {
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    let rings = support::load_polygon_fixture(path);
    for (buffer, dir) in buffers() {
        snapshot_at_buffer(stem, &rings, buffer, dir);
    }
}

fn snapshot_at_buffer(stem: &str, rings: &[Vec<Coord<i32>>], buffer: u16, dir: &str) {
    let per_tile = clip_all_tiles(rings, buffer);

    // Duplicating every vertex must not change the clip (consecutive dups are dropped).
    let duped = clip_all_tiles(&duplicate_vertices(rings), buffer);
    assert_eq!(
        duped, per_tile,
        "duplicating every vertex changed the clip for {stem} (buffer {buffer})"
    );

    // Build the snapshot: input polygons (yellow), then one filled ring per tile piece.
    let mut features: Vec<_> = rings.iter().map(|r| support::input_polygon(r)).collect();
    for (&tile, tile_rings) in &per_tile {
        for ring in tile_rings {
            features.push(support::tile_polygon(ring, tile));
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
