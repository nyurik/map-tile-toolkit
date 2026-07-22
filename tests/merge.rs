//! `merge` reconstructs a polyline from its per-tile runs.
//!
//! The headline test slices every (non-large) fixture into tiles, then — for **every possible
//! order** of those tiles — fold-merges them one after another, and checks that the incrementally
//! merged result reconstructs the original geometry. This exercises `merge`'s order-independence
//! (any permutation reaches the same reconstruction) and that non-adjacent tiles merge cleanly
//! (they stay disconnected until a connecting tile arrives).

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::{BTreeMap, HashSet};

use geo_types::{Coord, Geometry};
use map_tile_toolkit::{TileId, merge};

mod support;
use support::Cfg;

/// A config with the given divider/buffer.
fn slicer(divider: u32, buffer: u16) -> Cfg {
    support::slicer(divider, buffer)
}

/// A polyline as `Vec<Coord<i32>>`.
fn coords(v: Vec<(i32, i32)>) -> Vec<Coord<i32>> {
    v.into_iter().map(|(x, y)| Coord { x, y }).collect()
}

/// The runs of a polyline geometry (fixtures load as `geo-types` `Geometry`).
fn runs_of(g: &Geometry<i32>) -> Vec<Vec<Coord<i32>>> {
    support::lines_of(g)
        .into_iter()
        .map(<[_]>::to_vec)
        .collect()
}

/// Add `tile`'s origin (`tile · divider`) back to shared-frame runs, recovering global coordinates.
fn globalize(tile: TileId, runs: &[Vec<Coord<i32>>], divider: i32) -> Vec<Vec<Coord<i32>>> {
    let (ox, oy) = (tile.x * divider, tile.y * divider);
    runs.iter()
        .map(|r| {
            r.iter()
                .map(|c| Coord {
                    x: c.x + ox,
                    y: c.y + oy,
                })
                .collect()
        })
        .collect()
}

/// The set of directed edges of `runs`, skipping zero-length edges (consecutive equal vertices),
/// which slicing drops. Two polylines with the same edge set describe the same connectivity — the
/// invariant `merge` reconstruction must preserve, independent of run order or how a self-touching
/// path is retraced.
fn edge_set(runs: &[Vec<Coord<i32>>]) -> HashSet<(Coord<i32>, Coord<i32>)> {
    let mut set = HashSet::new();
    for run in runs {
        let mut prev: Option<Coord<i32>> = None;
        for &c in run {
            if let Some(p) = prev.filter(|&p| p != c) {
                set.insert((p, c));
            }
            prev = Some(c);
        }
    }
    set
}

/// Every permutation of `0..n` (n small: fixtures touch at most a handful of tiles).
fn permutations(n: usize) -> Vec<Vec<usize>> {
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

/// All tiles a fixture geometry touches, each tile's features flattened into combined runs (each line
/// is added as its own feature, then flattened — the merge inputs treat a tile's whole content as
/// one bag of runs).
fn tiles_of(cfg: &Cfg, geom: &Geometry<i32>) -> Vec<(TileId, Vec<Vec<Coord<i32>>>)> {
    let mut acc = cfg.all();
    for line in support::lines_of(geom) {
        acc.add_feature(line).expect("slice");
    }
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

/// Slice one polyline into all touched tiles, each tile's features flattened into combined runs.
fn all_tile_runs(cfg: &Cfg, poly: &[Coord<i32>]) -> BTreeMap<TileId, Vec<Vec<Coord<i32>>>> {
    let mut acc = cfg.all();
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

/// Clip one polyline to a single tile, its features flattened into runs.
fn one_tile_runs(cfg: &Cfg, poly: &[Coord<i32>], tile: TileId) -> Vec<Vec<Coord<i32>>> {
    let mut acc = cfg.one(tile);
    acc.add_feature(poly).expect("slice");
    acc.iter_features()
        .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
        .collect()
}

/// Fold-merge the tiles in `order`, returning the running lower-left anchor tile and the merged runs
/// in that tile's local frame.
fn fold_merge(
    cfg: &Cfg,
    tiles: &[(TileId, Vec<Vec<Coord<i32>>>)],
    order: &[usize],
) -> (TileId, Vec<Vec<Coord<i32>>>) {
    let (anchor0, first) = &tiles[order[0]];
    let mut anchor = *anchor0;
    let mut acc = first.clone();
    for &i in &order[1..] {
        let (tile, runs) = &tiles[i];
        acc = merge(
            cfg.divider(),
            (anchor, acc.as_slice()),
            (*tile, runs.as_slice()),
        )
        .expect("merge succeeds");
        anchor = TileId::new(anchor.x.min(tile.x), anchor.y.min(tile.y));
    }
    (anchor, acc)
}

#[test]
fn every_permutation_reconstructs_each_fixture() {
    let s = support::grid();
    let divider = s.divider() as i32;
    for (name, geom) in support::load_all_fixtures() {
        let tiles = tiles_of(&s, &geom);
        assert!(!tiles.is_empty(), "{name}: fixture produced no tiles");
        let want = edge_set(&runs_of(&geom));

        for order in permutations(tiles.len()) {
            let (anchor, merged) = fold_merge(&s, &tiles, &order);
            let got = edge_set(&globalize(anchor, &merged, divider));
            assert_eq!(
                got, want,
                "{name}: merging tiles in order {order:?} did not reconstruct the original"
            );
        }
    }
}

/// A concrete check of the shared-frame anchor: a vertical polyline crossing two tile borders,
/// merging the top two tiles. The result is anchored at the lower-left of the pair, so the bottom
/// vertex sits at a negative local coordinate.
#[test]
fn merge_shared_frame_anchor() {
    let s = slicer(10, 0);
    // One vertex each in tiles (0,0), (0,1), (0,2).
    let tiles = all_tile_runs(&s, &coords(vec![(5, 5), (5, 15), (5, 25)]));

    let merged = merge(
        s.divider(),
        (TileId::new(0, 1), tiles[&TileId::new(0, 1)].as_slice()),
        (TileId::new(0, 2), tiles[&TileId::new(0, 2)].as_slice()),
    )
    .expect("merge");
    // Anchored at (0,1) (origin (0,10)); the (5,5) global vertex is (5,-5) there.
    assert_eq!(merged, vec![coords(vec![(5, -5), (5, 5), (5, 15)])]);
    assert_eq!(
        globalize(TileId::new(0, 1), &merged, 10),
        vec![coords(vec![(5, 5), (5, 15), (5, 25)])],
    );
}

/// Merge accepts any two tiles (no adjacency requirement): non-adjacent pieces simply stay
/// disconnected in the shared frame.
#[test]
fn merge_non_adjacent_stays_disconnected() {
    let s = slicer(10, 0);
    // Two separate short lines, far apart: tile (0,0) and tile (5,0).
    let a = one_tile_runs(&s, &coords(vec![(2, 2), (7, 7)]), TileId::new(0, 0));
    let b = one_tile_runs(&s, &coords(vec![(52, 2), (57, 7)]), TileId::new(5, 0));
    let merged = merge(
        s.divider(),
        (TileId::new(0, 0), a.as_slice()),
        (TileId::new(5, 0), b.as_slice()),
    )
    .expect("merge");
    // Two disjoint segments → two runs (an order-independent set of two edges).
    let edges = edge_set(&globalize(TileId::new(0, 0), &merged, 10));
    assert_eq!(
        edges.len(),
        2,
        "expected two disconnected segments, got {edges:?}"
    );
}
