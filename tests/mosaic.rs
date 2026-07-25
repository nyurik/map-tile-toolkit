//! [`Mosaic`] reassembles sliced tiles back into whole features.
//!
//! One data-driven test loads **every** fixture (sorted), slices them all into one set of tiles, then
//! for **every permutation** of the tile-insertion order builds a fresh mosaic, checks every `add`
//! succeeds (a slicer's own tiles are self-consistent, so none ever conflict), and checks the
//! reassembled geometry equals the combined input's — proving order-independence over all fixtures at
//! once. The extent is chosen coarse enough that the whole fixture set lands in a handful of tiles, so
//! all permutations stay enumerable while the geometry itself is unchanged; an assertion trips loudly
//! if that count grows past a safe bound.
//!
//! Reassembly is compared by **directed-edge set**: the mosaic re-chains the geometry by connectivity,
//! so a self-touching path or a shared junction may come back split/joined differently than the input
//! features, but the set of edges — the geometry — is identical.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use geo_types::Coord;
use map_tile_toolkit::{Mosaic, TileId};

mod support;

/// Every fixture's polylines, files sorted for a stable order.
fn all_fixture_polylines() -> Vec<Vec<Coord<i32>>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures dir exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("geojson"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found");
    paths
        .iter()
        .flat_map(|p| support::load_fixture(p))
        .collect()
}

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

/// Every permutation of `0..n`.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    assert!(n <= 10, "permutations of more than {n} tiles is too many to enumerate");

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

#[test]
fn every_permutation_reassembles_all_fixtures() {
    let cfg = support::grid();
    let polylines = all_fixture_polylines();

    // Slice the whole fixture set into per-tile (local-frame) runs — exactly what a caller feeds back.
    let tiles = support::slice_all_runs(&cfg, &polylines);
    assert!(!tiles.is_empty(), "fixtures produced no tiles");

    let want = edge_set(&polylines);
    for order in permutations(tiles.len()) {
        let mut mosaic = Mosaic::new(cfg.extent).expect("valid config");
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
            "insertion order {order:?} did not reconstruct the combined geometry"
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
