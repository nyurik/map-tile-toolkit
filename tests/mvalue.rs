//! Slicing and merging over a custom [`Vertex`] that carries an M value (a per-vertex payload
//! `geo-types` cannot represent). The payload must ride through slicing and merging untouched, and
//! the positions must match the plain `Coord` path.

#![allow(clippy::pedantic, reason = "test tool")]

use geo_types::{Coord, Geometry, LineString};
use map_tile_toolkit::{Measured, Slicer, TileId};

fn slicer() -> Slicer {
    Slicer::new(10, 0).expect("valid config")
}

/// A vertical polyline crossing two tile borders, each vertex tagged with a distinct M value.
fn measured_line() -> Vec<Vec<Measured<u32>>> {
    vec![vec![
        Measured::new(5, 5, 10),  // tile (0,0)
        Measured::new(5, 15, 20), // tile (0,1)
        Measured::new(5, 25, 30), // tile (0,2)
    ]]
}

#[test]
fn slice_all_preserves_m_and_localizes_position() {
    let s = slicer();
    let lines = measured_line();
    let tiles: std::collections::BTreeMap<TileId, Vec<Vec<Measured<u32>>>> = s
        .slice_all_lines(&lines)
        .expect("slice")
        .into_iter()
        .collect();

    // Tile (0,1) sees the whole polyline (both segments touch it). Positions are tile-local
    // (origin (0,10)); the M values are carried through unchanged.
    assert_eq!(
        tiles[&TileId::new(0, 1)],
        vec![vec![
            Measured::new(5, -5, 10),
            Measured::new(5, 5, 20),
            Measured::new(5, 15, 30),
        ]]
    );
}

#[test]
fn positions_match_the_coord_path() {
    let s = slicer();
    let lines = measured_line();

    // Same geometry without the payload, via the geo_types API.
    let plain = Geometry::LineString(LineString::from(vec![(5, 5), (5, 15), (5, 25)]));

    let m_tiles: std::collections::BTreeMap<TileId, Vec<Vec<Coord<i32>>>> = s
        .slice_all_lines(&lines)
        .expect("slice m")
        .into_iter()
        .map(|(t, runs)| {
            (
                t,
                runs.iter()
                    .map(|r| r.iter().map(|v| v.position).collect())
                    .collect(),
            )
        })
        .collect();

    let coord_tiles: std::collections::BTreeMap<TileId, Vec<Vec<Coord<i32>>>> = s
        .slice_all(&plain)
        .expect("slice coord")
        .into_iter()
        .map(|(t, g)| {
            let runs = match g {
                Geometry::LineString(ls) => vec![ls.0],
                Geometry::MultiLineString(mls) => mls.0.into_iter().map(|ls| ls.0).collect(),
                other => panic!("unexpected {other:?}"),
            };
            (t, runs)
        })
        .collect();

    assert_eq!(
        m_tiles, coord_tiles,
        "M-vertex positions must match the Coord path"
    );
}

#[test]
fn merge_reconstructs_with_m_values() {
    let s = slicer();
    let tiles: std::collections::BTreeMap<TileId, Vec<Vec<Measured<u32>>>> = s
        .slice_all_lines(&measured_line())
        .expect("slice")
        .into_iter()
        .collect();

    // Merge tiles (0,1) and (0,2); the shared frame is anchored at (0,1).
    let merged = s
        .merge_lines(
            (TileId::new(0, 1), &tiles[&TileId::new(0, 1)]),
            (TileId::new(0, 2), &tiles[&TileId::new(0, 2)]),
        )
        .expect("merge");

    // The duplicated border segment collapses; the reconstructed run keeps every M value.
    assert_eq!(
        merged,
        vec![vec![
            Measured::new(5, -5, 10),
            Measured::new(5, 5, 20),
            Measured::new(5, 15, 30),
        ]]
    );
}
