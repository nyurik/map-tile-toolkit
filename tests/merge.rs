//! `Slicer::merge` reconstructs a geometry from its per-tile pieces.
//!
//! The headline test slices every (non-large) fixture into tiles, then — for **every possible
//! order** of those tiles — fold-merges them one after another, and checks that the incrementally
//! merged result reconstructs the original geometry. This exercises `merge`'s order-independence
//! (any permutation reaches the same reconstruction) and that non-adjacent tiles merge cleanly
//! (they stay disconnected until a connecting tile arrives).

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::HashSet;

use geo_types::{Coord, Geometry, LineString, MultiLineString};
use map_tile_toolkit::{Slicer, TileId};

mod support;

fn slicer(divider: u32, buffer: u16) -> Slicer {
    Slicer::new(divider, buffer).expect("valid config")
}

fn line(coords: Vec<(i32, i32)>) -> Geometry<i32> {
    Geometry::LineString(LineString::from(coords))
}

/// The runs (vertex sequences) of a polyline geometry.
fn runs_of(g: &Geometry<i32>) -> Vec<Vec<Coord<i32>>> {
    match g {
        Geometry::LineString(ls) => vec![ls.0.clone()],
        Geometry::MultiLineString(mls) => mls.0.iter().map(|ls| ls.0.clone()).collect(),
        other => panic!("unexpected geometry {other:?}"),
    }
}

/// Add `tile`'s origin (`tile · divider`) back to a tile-local (or shared-frame) piece, recovering
/// global coordinates.
fn globalize(tile: TileId, g: &Geometry<i32>, divider: i32) -> Geometry<i32> {
    let (ox, oy) = (tile.x * divider, tile.y * divider);
    let shift = |ls: &LineString<i32>| {
        LineString(
            ls.0.iter()
                .map(|c| Coord {
                    x: c.x + ox,
                    y: c.y + oy,
                })
                .collect(),
        )
    };
    match g {
        Geometry::LineString(l) => Geometry::LineString(shift(l)),
        Geometry::MultiLineString(m) => {
            Geometry::MultiLineString(MultiLineString(m.0.iter().map(shift).collect()))
        }
        other => panic!("unexpected geometry {other:?}"),
    }
}

/// The set of directed edges of `g`, skipping zero-length edges (consecutive equal vertices), which
/// slicing drops. Two geometries with the same edge set describe the same polyline connectivity —
/// the invariant `merge` reconstruction must preserve, independent of run order or how a
/// self-touching path is retraced.
fn edge_set(g: &Geometry<i32>) -> HashSet<(Coord<i32>, Coord<i32>)> {
    let mut set = HashSet::new();
    for run in runs_of(g) {
        let mut prev: Option<Coord<i32>> = None;
        for c in run {
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

/// Fold-merge the tiles in `order`, returning the running lower-left anchor tile and the merged
/// piece in that tile's local frame.
fn fold_merge(
    s: Slicer,
    tiles: &[(TileId, Geometry<i32>)],
    order: &[usize],
) -> (TileId, Geometry<i32>) {
    let (anchor0, first) = &tiles[order[0]];
    let mut anchor = *anchor0;
    let mut acc = first.clone();
    for &i in &order[1..] {
        let (tile, piece) = &tiles[i];
        acc = s
            .merge((anchor, &acc), (*tile, piece))
            .expect("merge succeeds")
            .expect("merged piece is non-empty");
        anchor = TileId::new(anchor.x.min(tile.x), anchor.y.min(tile.y));
    }
    (anchor, acc)
}

#[test]
fn every_permutation_reconstructs_each_fixture() {
    let s = support::SLICER;
    let divider = s.divider() as i32;
    for (name, geom) in support::load_all_fixtures() {
        // Slice into tiles; slice_all already combines all of a tile's runs into one piece.
        let tiles: Vec<(TileId, Geometry<i32>)> =
            s.slice_all(&geom).expect("slice").into_iter().collect();
        assert!(!tiles.is_empty(), "{name}: fixture produced no tiles");
        let want = edge_set(&geom);

        for order in permutations(tiles.len()) {
            let (anchor, merged) = fold_merge(s, &tiles, &order);
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
    let geom = line(vec![(5, 5), (5, 15), (5, 25)]); // one vertex each in tiles (0,0),(0,1),(0,2)
    let tiles: std::collections::BTreeMap<TileId, Geometry<i32>> =
        s.slice_all(&geom).expect("slice").into_iter().collect();

    let merged = s
        .merge(
            (TileId::new(0, 1), &tiles[&TileId::new(0, 1)]),
            (TileId::new(0, 2), &tiles[&TileId::new(0, 2)]),
        )
        .expect("merge")
        .expect("non-empty");
    // Anchored at (0,1) (origin (0,10)); the (5,5) global vertex is (5,-5) there.
    assert_eq!(merged, line(vec![(5, -5), (5, 5), (5, 15)]));
    assert_eq!(
        globalize(TileId::new(0, 1), &merged, 10),
        line(vec![(5, 5), (5, 15), (5, 25)]),
    );
}

/// Merge accepts any two tiles now (no adjacency requirement): non-adjacent pieces simply stay
/// disconnected in the shared frame.
#[test]
fn merge_non_adjacent_stays_disconnected() {
    let s = slicer(10, 0);
    // Two separate short lines, far apart: tile (0,0) and tile (5,0).
    let a = s
        .slice(&line(vec![(2, 2), (7, 7)]), TileId::new(0, 0))
        .expect("slice")
        .expect("piece");
    let b = s
        .slice(&line(vec![(52, 2), (57, 7)]), TileId::new(5, 0))
        .expect("slice")
        .expect("piece");
    let merged = s
        .merge((TileId::new(0, 0), &a), (TileId::new(5, 0), &b))
        .expect("merge")
        .expect("non-empty");
    // Two disjoint segments → a MultiLineString of two runs (order-independent set of two edges).
    let edges = edge_set(&globalize(TileId::new(0, 0), &merged, 10));
    assert_eq!(
        edges.len(),
        2,
        "expected two disconnected segments, got {edges:?}"
    );
}
