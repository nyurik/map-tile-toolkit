//! [`Mosaic`] reassembles sliced tiles back into whole features.
//!
//! The headline test is data-driven: for every fixture it slices the input into tiles, then for
//! **every permutation** of the tile-insertion order builds a fresh mosaic, checks every `add`
//! succeeds (a slicer's own tiles are self-consistent, so none ever conflict), and checks the
//! reassembled geometry equals the original input's — proving order-independence. Buffer 0 keeps the
//! tile count small enough to enumerate all permutations (the largest fixture touches 6 tiles → 720
//! orders); an assertion trips loudly if a future fixture grows past a safe bound.
//!
//! Reassembly is compared by **directed-edge set**: the mosaic re-chains the geometry by connectivity,
//! so a self-touching path or a shared junction may come back split/joined differently than the input
//! features, but the set of edges — the geometry — is identical.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::HashSet;
use std::path::Path;

use geo_types::Coord;
use map_tile_toolkit::{Mosaic, TileId};

mod support;

/// Directed-edge set of a run list, skipping zero-length edges (slicing drops consecutive dups).
fn edge_set(runs: &[Vec<Coord<i32>>]) -> HashSet<(Coord<i32>, Coord<i32>)> {
    let mut set = HashSet::new();
    for run in runs {
        for w in run.windows(2) {
            if w[0] != w[1] {
                set.insert((w[0], w[1]));
            }
        }
    }
    set
}

/// Every permutation of `0..n` (n is small — a fixture touches at most a handful of tiles).
fn permutations(n: usize) -> Vec<Vec<usize>> {
    assert!(n < 10, "permutations of {n} is too many to enumerate");

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

mod files {
    use test_each_file::test_each_path;

    use super::reassemble_one_fixture;

    // Generate one test per input fixture.
    test_each_path! { for ["geojson"] in "./tests/fixtures" => reassemble_one_fixture }
}

fn reassemble_one_fixture([path]: [&Path; 1]) {
    let cfg = support::grid();
    let extent = cfg.extent();
    let polylines = support::load_fixture(path);

    // Slice into per-tile (local-frame) runs, exactly what a caller would feed back to a mosaic.
    let tiles = support::slice_all_runs(&cfg, &polylines);
    assert!(!tiles.is_empty(), "fixture produced no tiles");
    assert!(
        tiles.len() <= 8,
        "fixture touches {} tiles — too many to enumerate all permutations; add a sampling cap",
        tiles.len()
    );

    let want = edge_set(&polylines);
    for order in permutations(tiles.len()) {
        let mut mosaic = Mosaic::new(extent).expect("valid config");
        for &i in &order {
            let (tile, runs) = &tiles[i];
            mosaic
                .add(*tile, runs.as_slice())
                .expect("a slicer's own tiles are self-consistent and never conflict");
        }
        assert_eq!(mosaic.len(), tiles.len(), "every tile must register");

        // The mosaic reassembles in the global frame, which at buffer 0 is the input's own space.
        let features: Vec<Vec<Coord<i32>>> = mosaic.iter_features().collect();
        assert_eq!(
            edge_set(&features),
            want,
            "insertion order {order:?} did not reconstruct the original geometry"
        );
    }
}

// --- Behaviors the Coord fixtures structurally can't reach (payload conflicts live in mvalue.rs). ---

#[test]
fn invalid_extent_is_rejected() {
    use map_tile_toolkit::SliceError;
    assert_eq!(
        Mosaic::<Coord<i32>>::new(0).err(),
        Some(SliceError::InvalidExtent)
    );
    assert_eq!(
        Mosaic::<Coord<i32>>::new(u32::MAX).err(),
        Some(SliceError::InvalidExtent)
    );
}

#[test]
fn far_tile_overflows_instead_of_panicking() {
    use map_tile_toolkit::CombineError;
    let mut mosaic = Mosaic::new(4096).expect("valid config");
    let run = vec![Coord { x: 0, y: 0 }, Coord { x: 1, y: 0 }];
    let bad = mosaic.add(TileId::new(i32::MAX, 0), &[run]);
    assert_eq!(bad, Err(CombineError::Overflow));
    assert!(mosaic.is_empty(), "an overflowing add changes nothing");
}
