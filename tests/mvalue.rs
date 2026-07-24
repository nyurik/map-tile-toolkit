//! Slicing and merging over a custom [`Vertex`] that carries an M value (a per-vertex payload
//! `geo-types` cannot represent). The payload must ride through slicing and merging untouched, and
//! the positions must match the plain `Coord` path.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::BTreeMap;

use geo_types::Coord;
use map_tile_toolkit::{CombineError, Measured, Mosaic, SlicerAll, TileId, Vertex};

/// Slice one polyline into every tile it touches (as a single feature), each tile's runs flattened.
/// Generic over the vertex type, so the same helper drives both the `Measured` and `Coord` paths.
fn slice_all_runs<V: Vertex>(poly: &[V]) -> BTreeMap<TileId, Vec<Vec<V>>> {
    let mut acc = SlicerAll::<V>::new(10, 0).expect("valid config");
    acc.add_feature(poly).expect("slice");
    acc.iter_tiles()
        .map(|t| {
            let runs = t
                .iter_features()
                .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
                .collect();
            (t.id(), runs)
        })
        .collect()
}

/// A vertical polyline crossing two tile borders, each vertex tagged with a distinct M value.
fn measured_line() -> Vec<Measured<u32>> {
    vec![
        Measured::new(5, 5, 10),  // tile (0,0)
        Measured::new(5, 15, 20), // tile (0,1)
        Measured::new(5, 25, 30), // tile (0,2)
    ]
}

#[test]
fn slice_all_preserves_m_and_localizes_position() {
    let tiles = slice_all_runs(&measured_line());

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
    // The same polyline without the payload, as plain `Coord`s.
    let plain = vec![
        Coord { x: 5, y: 5 },
        Coord { x: 5, y: 15 },
        Coord { x: 5, y: 25 },
    ];

    let m_positions: BTreeMap<TileId, Vec<Vec<Coord<i32>>>> = slice_all_runs(&measured_line())
        .into_iter()
        .map(|(t, runs)| {
            let runs = runs
                .iter()
                .map(|r| r.iter().map(|v| v.position).collect())
                .collect();
            (t, runs)
        })
        .collect();

    let coord_tiles = slice_all_runs(&plain);

    assert_eq!(
        m_positions, coord_tiles,
        "M-vertex positions must match the Coord path"
    );
}

#[test]
fn mosaic_reconstructs_with_m_values() {
    // Feed every sliced tile into a mosaic; it reassembles the original line in the global frame with
    // every M value carried through unchanged (the duplicated border segments collapse).
    let mut mosaic = Mosaic::new(10).expect("valid config");
    for (tile, runs) in slice_all_runs(&measured_line()) {
        mosaic
            .add(tile, runs.as_slice())
            .expect("self-consistent tiles never conflict");
    }
    let feats: Vec<Vec<Measured<u32>>> = mosaic.iter_features().collect();
    assert_eq!(feats, vec![measured_line()]);
}

#[test]
fn mosaic_rejects_a_payload_mismatch() {
    // Two tiles share the border segment (5,5)→(5,15), but tile (0,1) carries a different M on the
    // shared vertex — the mosaic rejects it, naming tile (0,0), and stays unchanged.
    let mut mosaic = Mosaic::new(10).expect("valid config");
    mosaic
        .add(
            TileId::new(0, 0),
            &[vec![Measured::new(5, 5, 10), Measured::new(5, 15, 20)]],
        )
        .expect("first tile");
    let bad = mosaic.add(
        TileId::new(0, 1),
        &[vec![Measured::new(5, -5, 10), Measured::new(5, 5, 999)]],
    );
    assert_eq!(bad, Err(CombineError::Conflict(vec![TileId::new(0, 0)])));
    assert_eq!(mosaic.len(), 1);
    assert!(mosaic.contains(TileId::new(0, 0)));
}
