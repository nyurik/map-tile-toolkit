//! [`Mosaic`] reassembles sliced tiles back into whole features.
//!
//! One data-driven test loads **every** fixture (sorted), slices them all into one set of tiles, then
//! for **every permutation** of the tile-insertion order builds a fresh mosaic, checks every `add`
//! succeeds (a slicer's own tiles are self-consistent, so none ever conflict), and checks the
//! reassembled geometry equals the combined input's — proving order-independence over all fixtures at
//! once. It runs at **both** buffer sizes: flush tiles (buffer 0) and overlapping tiles (buffer 5).
//! A buffer only duplicates near-edge segments into more tiles, so rebasing (`local + tile·extent`)
//! must collapse those duplicates back onto the same global edges — either way the mosaic recovers
//! the exact same original geometry. The extent is chosen coarse enough that the whole fixture set
//! lands in a handful of tiles, so all permutations stay enumerable; an assertion trips loudly if
//! that count grows past a safe bound.
//!
//! Reassembly is compared by **directed-edge set**: the mosaic re-chains the geometry by connectivity,
//! so a self-touching path or a shared junction may come back split/joined differently than the input
//! features, but the set of edges — the geometry — is identical.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use geo_types::Coord;
use map_tile_toolkit::{Mosaic, TileError, TileId};

use crate::support::Cfg;

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

/// A polyline of plain `Coord`s.
fn line(coords: &[(i32, i32)]) -> Vec<Coord<i32>> {
    coords.iter().map(|&(x, y)| Coord { x, y }).collect()
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
    assert!(n <= 10, "permutations of more than {n} tiles is too many");

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
    let polylines = all_fixture_polylines();

    // Reassembly is a pure function of the edge set, so every insertion order — and both buffer sizes —
    // yields byte-identical GeoJSON. `allow_duplicates!` lets all those runs assert against one shared
    // `reassembled.geojson` snapshot; the single block spans both `validate_mosaic` calls.
    insta::allow_duplicates! {
        for (label, cfg) in [
            ("buffer 0", support::grid()),
            ("buffer 5", support::grid_buffered()),
        ] {
            validate_mosaic(label, &cfg, &polylines);
        }
    }
}

/// Slice all fixtures into per-tile (local-frame) runs — exactly what a caller feeds back — then check
/// that **every** tile-insertion order reassembles the exact same features (order-independence), that
/// those features match the input (edge set), and that the result matches the shared
/// `reassembled.geojson` snapshot.
///
/// Reassembly is a pure function of the edge set, so all orders (and both buffers) produce identical
/// features; the per-order check is a cheap equality against the first, and the snapshot is asserted
/// once here. Must be called inside an `insta::allow_duplicates!` block, so both buffer sizes can
/// assert that one shared snapshot.
fn validate_mosaic(label: &str, cfg: &Cfg, polylines: &[Vec<Coord<i32>>]) {
    let tiles = support::slice_all_runs(cfg, polylines);
    assert!(!tiles.is_empty(), "{label}: fixtures produced no tiles");

    let mut canonical: Option<Vec<Vec<Coord<i32>>>> = None;
    for order in permutations(tiles.len()) {
        let mut mosaic = Mosaic::new(cfg.extent).expect("valid config");
        for &i in &order {
            let (tile, runs) = &tiles[i];
            mosaic
                .add(*tile, runs.as_slice())
                .expect("a slicer's own tiles are self-consistent and never conflict");
        }
        assert_eq!(
            mosaic.len(),
            tiles.len(),
            "{label}: every tile must register"
        );

        // The mosaic reassembles in the global frame — the input's own space at any buffer.
        let features: Vec<Vec<Coord<i32>>> = mosaic.iter_features().collect();
        match &canonical {
            None => canonical = Some(features),
            Some(first) => assert_eq!(
                &features, first,
                "{label}: insertion order {order:?} changed the reassembly"
            ),
        }
    }

    // Correctness against the input, then the golden GeoJSON. Both buffers reach here with identical
    // features, so both assert the one snapshot (the `allow_duplicates!` block permits the repeat).
    let features = canonical.expect("at least one permutation");
    assert_eq!(
        edge_set(&features),
        edge_set(polylines),
        "{label}: reassembly did not reconstruct the combined input geometry"
    );
    insta::with_settings!(
        { snapshot_path => "snapshots-mosaic", prepend_module_to_snapshot => false },
        { insta::assert_binary_snapshot!("reassembled.geojson", reassembly_geojson(polylines, &features)); }
    );
}

/// Serialize a reassembly as a GeoJSON `FeatureCollection`, mirroring the clip snapshots: the original
/// input polylines in gray, then every reassembled feature colored by parity. Reassembled features are
/// sorted by their coordinates so the file is stable and easy to eyeball — each colored line must lie
/// exactly on the gray input, since reassembly is in the input's own global space.
fn reassembly_geojson(input: &[Vec<Coord<i32>>], features: &[Vec<Coord<i32>>]) -> Vec<u8> {
    let mut fc: Vec<_> = input
        .iter()
        .map(|line| support::input_feature(line))
        .collect();

    let mut sorted = features.to_vec();
    sorted.sort_by_key(|run| run.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>());
    for (i, run) in sorted.iter().enumerate() {
        let color = if i % 2 == 0 { "#1f77b4" } else { "#ff7f0e" };
        fc.push(support::styled_line(run, &format!("feature {i}"), color, 3));
    }
    support::feature_collection_bytes(fc)
}

// --- Behaviors the Coord fixtures structurally can't reach (payload conflicts live in mvalue.rs). ---

#[test]
fn invalid_extent_is_rejected() {
    assert_eq!(
        Mosaic::<Coord<i32>>::new(0).err(),
        Some(TileError::InvalidExtent)
    );
    assert_eq!(
        Mosaic::<Coord<i32>>::new(u32::MAX).err(),
        Some(TileError::InvalidExtent)
    );
}

#[test]
fn far_tile_overflows_instead_of_panicking() {
    let mut mosaic = Mosaic::new(4096).expect("valid config");
    let run = vec![Coord { x: 0, y: 0 }, Coord { x: 1, y: 0 }];
    let bad = mosaic.add(TileId::new(i32::MAX, 0), &[run]);
    assert_eq!(bad, Err(TileError::Overflow));
    assert!(mosaic.is_empty(), "an overflowing add changes nothing");
}

#[test]
fn purge_and_clear_manage_tiles() {
    let mut mosaic = Mosaic::new(25).expect("valid config");
    assert!(mosaic.is_empty());

    // Two disjoint segments in different tiles.
    mosaic
        .add(TileId::new(0, 0), &[line(&[(1, 1), (10, 10)])])
        .expect("tile 0");
    mosaic
        .add(TileId::new(5, 5), &[line(&[(3, 3), (12, 12)])])
        .expect("tile 5");
    assert_eq!(mosaic.len(), 2);
    assert!(mosaic.contains(TileId::new(0, 0)));
    assert!(!mosaic.contains(TileId::new(9, 9)));

    // Purge one tile: it and only it disappears.
    assert!(mosaic.purge(TileId::new(0, 0)));
    assert!(!mosaic.contains(TileId::new(0, 0)));
    assert!(mosaic.contains(TileId::new(5, 5)));
    assert_eq!(mosaic.len(), 1);
    assert!(
        !mosaic.purge(TileId::new(0, 0)),
        "purging an absent tile is a no-op"
    );

    // Clear drops everything.
    mosaic.clear();
    assert!(mosaic.is_empty());
    assert_eq!(mosaic.len(), 0);
}

#[test]
fn purge_keeps_edges_other_tiles_still_hold() {
    // Both tiles carry the same global border segment (20,5)→(30,5) — tile (1,0)'s local run rebases
    // onto it. Purging one tile must leave the shared edge, since the other still holds it.
    let mut mosaic = Mosaic::new(25).expect("valid config");
    mosaic
        .add(TileId::new(0, 0), &[line(&[(20, 5), (30, 5)])])
        .expect("tile 0");
    mosaic
        .add(TileId::new(1, 0), &[line(&[(-5, 5), (5, 5)])])
        .expect("tile 1");

    assert!(mosaic.purge(TileId::new(1, 0)));
    let feats: Vec<Vec<Coord<i32>>> = mosaic.iter_features().collect();
    assert_eq!(
        feats,
        vec![line(&[(20, 5), (30, 5)])],
        "the shared edge survives while tile (0,0) still holds it"
    );

    // Purging the last holder drops the edge and empties the mosaic.
    assert!(mosaic.purge(TileId::new(0, 0)));
    assert!(mosaic.is_empty());
}
