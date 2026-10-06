//! [`PolygonMosaic`] reassembles sliced polygon tiles back into whole rings — the accumulating inverse
//! of the polygon slicer.
//!
//! One test per polygon fixture (`tests/polygons/fixtures/*.geojson`): slice the fixture's polygons
//! into per-tile rings with [`PolygonSlicerOne`] across the padded tile span, then reassemble them with
//! [`PolygonMosaic`] under many tile-insertion orders. Every order must succeed (a slicer's own tiles
//! are self-consistent, so none conflict) and reassemble the **same** rings, whose directed-edge set
//! must equal the input polygons' edge set — proving the mosaic dropped the synthetic clip-boundary
//! corners and re-chained the original edges back into the original rings. It runs at buffer 0 and
//! buffer 5; both recover the exact same global geometry.
//!
//! Correctness is asserted directly — `reassembled == input` by **directed-edge set** — rather than
//! against a golden file: the mosaic re-chains by connectivity, so rings that touch at a shared vertex
//! may come back split/joined differently (an implementation detail the edge-set check is invariant to),
//! but the geometry — the set of edges — is identical to the input.
//!
//! Exhaustive order-independence of the underlying edge index is proven for polylines in `mosaic.rs`;
//! here a small tile set still gets every permutation, while the few larger ones get a deterministic
//! spread of orders (identity, reverse, rotations).

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::HashSet;
use std::path::Path;

use geo_types::Coord;
use map_tile_toolkit::{PolygonMosaic, PolygonSlicerOne, TileError, TileId};

mod support;

use crate::support::{EXTENT, FixturePolygon, permutations};

mod files {
    use test_each_file::test_each_path;

    // One test per input fixture.
    test_each_path! { for ["geojson"] in "./tests/polygons/fixtures" => super::reassemble_fixture }
}

/// All rings (exterior + every hole) of a fixture, closed, in the global frame — the reassembly target.
fn input_rings(polygons: &[FixturePolygon]) -> Vec<Vec<Coord<i32>>> {
    polygons
        .iter()
        .flat_map(|p| std::iter::once(p.exterior.clone()).chain(p.holes.iter().cloned()))
        .collect()
}

/// Directed-edge set of closed rings, skipping zero-length edges (consecutive dups).
fn edge_set(rings: &[Vec<Coord<i32>>]) -> HashSet<(Coord<i32>, Coord<i32>)> {
    let mut set = HashSet::new();
    for r in rings {
        for w in r.windows(2) {
            if w[0] != w[1] {
                set.insert((w[0], w[1]));
            }
        }
    }
    set
}

/// The rings a single-tile slicer produced, flattened across its features (exterior + holes) in the
/// tile-local frame — exactly what a caller collects from one tile's output to feed the mosaic.
fn tile_rings(slicer: &PolygonSlicerOne<Coord<i32>>) -> Vec<Vec<Coord<i32>>> {
    let mut rings = Vec::new();
    for f in slicer.iter_features() {
        for r in f.iter_rings() {
            rings.push(r.vertices().to_vec());
        }
    }
    rings
}

/// Slice all `polygons` into per-tile ring lists (local frame) across the padded span, keeping only
/// tiles that produced geometry.
fn slice_tiles(polygons: &[FixturePolygon], buffer: u16) -> Vec<(TileId, Vec<Vec<Coord<i32>>>)> {
    let exts: Vec<Vec<Coord<i32>>> = polygons.iter().map(|p| p.exterior.clone()).collect();
    let (lo, hi) = support::padded_tile_span(&exts);
    let mut out = Vec::new();
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            let mut slicer =
                PolygonSlicerOne::<Coord<i32>>::new(EXTENT, buffer, tile).expect("valid config");
            for p in polygons {
                let holes: Vec<&[Coord<i32>]> = p.holes.iter().map(Vec::as_slice).collect();
                slicer.add_feature(&p.exterior, &holes).expect("clip");
            }
            let rings = tile_rings(&slicer);
            if !rings.is_empty() {
                out.push((tile, rings));
            }
        }
    }
    out
}

/// Tile-insertion orders to try: every permutation for a small set (exhaustive), else a deterministic
/// spread — identity, reverse, and every rotation — for larger sets where `n!` is impractical.
fn orders(n: usize) -> Vec<Vec<usize>> {
    if n <= 6 {
        return permutations(n);
    }
    let base: Vec<usize> = (0..n).collect();
    let mut out = vec![base.clone()];
    let mut rev = base;
    rev.reverse();
    out.push(rev);
    for r in 1..n {
        out.push((0..n).map(|i| (i + r) % n).collect());
    }
    out
}

fn reassemble_fixture([path]: [&Path; 1]) {
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    let polygons = support::load_polygon_fixture(path);
    let target = edge_set(&input_rings(&polygons));

    // Both buffers reassemble to the same global geometry, and reassembly is a pure function of the
    // edge set, so every order (and both buffers) yields byte-identical features — pinned to `canonical`.
    let mut canonical: Option<Vec<Vec<Coord<i32>>>> = None;
    for buffer in [0, 5] {
        let tiles = slice_tiles(&polygons, buffer);
        assert!(
            !tiles.is_empty(),
            "{stem} (buffer {buffer}) produced no tiles"
        );

        for order in orders(tiles.len()) {
            let mut mosaic =
                PolygonMosaic::<Coord<i32>>::new(EXTENT, buffer).expect("valid config");
            for &i in &order {
                let (tile, rings) = &tiles[i];
                mosaic
                    .add(*tile, rings.as_slice())
                    .expect("a slicer's own tiles are self-consistent and never conflict");
            }

            let features: Vec<Vec<Coord<i32>>> = mosaic.iter_features().collect();
            assert_eq!(
                edge_set(&features),
                target,
                "{stem} (buffer {buffer}, order {order:?}): reassembly did not reconstruct the input"
            );
            // Reassembly is a pure function of the edge set, so every order — and both buffers —
            // yields byte-identical features; pin them to the first.
            match &canonical {
                None => canonical = Some(features),
                Some(first) => assert_eq!(
                    &features, first,
                    "{stem} (buffer {buffer}, order {order:?}): reassembly changed with the order"
                ),
            }
        }
    }
}

// ---- Focused behavior tests ----

fn ring(pts: &[(i32, i32)]) -> Vec<Coord<i32>> {
    pts.iter().map(|&(x, y)| Coord { x, y }).collect()
}

#[test]
fn invalid_config_rejected() {
    assert_eq!(
        PolygonMosaic::<Coord<i32>>::new(0, 0).err(),
        Some(TileError::InvalidExtent)
    );
    assert_eq!(
        PolygonMosaic::<Coord<i32>>::new(10, 5).err(),
        Some(TileError::BufferTooLarge)
    );
}

#[test]
fn empty_mosaic_yields_no_features() {
    let m = PolygonMosaic::<Coord<i32>>::new(EXTENT, 0).expect("valid config");
    assert!(m.is_empty());
    assert_eq!(m.iter_features().count(), 0);
}

#[test]
fn interior_fill_tile_contributes_nothing() {
    // A polygon enclosing tile (1,1) with no edge near it clips to an all-synthetic fill there; the
    // mosaic drops every synthetic vertex, so the tile records no original edge (a reassembled
    // polygon's interior is implied by its winding, not by fill tiles).
    let ext = ring(&[(-10, -10), (80, -10), (80, 80), (-10, 80), (-10, -10)]);
    let mut s = PolygonSlicerOne::<Coord<i32>>::new(EXTENT, 0, TileId::new(1, 1)).expect("config");
    s.add_feature(&ext, &[]).expect("clip");
    let rings = tile_rings(&s);
    assert!(
        !rings.is_empty(),
        "the fill tile does carry (synthetic) geometry"
    );

    let mut m = PolygonMosaic::<Coord<i32>>::new(EXTENT, 0).expect("valid config");
    m.add(TileId::new(1, 1), &rings).expect("add");
    assert!(
        m.is_empty(),
        "an all-synthetic fill tile records no original edges"
    );
    assert!(!m.contains(TileId::new(1, 1)));
    assert_eq!(m.iter_features().count(), 0);
}

#[test]
fn inconsistent_tiles_conflict() {
    // Tile (0,0) carries a ring reaching into tile (1,0)'s core (its right edge sits at x = 35 ∈
    // [25,49]); tile (1,0) is then given a disjoint ring that does not corroborate that edge — a
    // membership conflict, leaving the mosaic unchanged.
    let mut s0 = PolygonSlicerOne::<Coord<i32>>::new(EXTENT, 0, TileId::new(0, 0)).expect("config");
    s0.add_feature(&ring(&[(15, 5), (35, 5), (35, 15), (15, 15), (15, 5)]), &[])
        .expect("clip");
    let rings0 = tile_rings(&s0);

    let mut m = PolygonMosaic::<Coord<i32>>::new(EXTENT, 0).expect("valid config");
    m.add(TileId::new(0, 0), &rings0).expect("add");

    // A ring fully inside tile (1,0)'s core (given in that tile's local frame), unrelated to the edge
    // tile (0,0) planted there.
    let disjoint = ring(&[(5, 5), (15, 5), (15, 15), (5, 15), (5, 5)]);
    let err = m.add(TileId::new(1, 0), &[disjoint]).unwrap_err();
    assert!(
        matches!(err, TileError::Conflict(_)),
        "expected a conflict, got {err:?}"
    );
    assert!(
        !m.contains(TileId::new(1, 0)),
        "a conflicting add leaves the mosaic unchanged"
    );
    assert_eq!(m.len(), 1);
}

#[test]
fn purge_and_clear_manage_tiles() {
    // Two disjoint squares, each fully inside its own tile.
    let sq = ring(&[(5, 5), (20, 5), (20, 20), (5, 20), (5, 5)]);
    let mut m = PolygonMosaic::<Coord<i32>>::new(EXTENT, 0).expect("valid config");
    assert!(m.is_empty());
    m.add(TileId::new(0, 0), std::slice::from_ref(&sq))
        .expect("tile 0");
    m.add(TileId::new(3, 3), &[sq]).expect("tile 3");
    assert_eq!(m.len(), 2);
    assert!(m.contains(TileId::new(0, 0)));

    assert!(m.purge(TileId::new(0, 0)));
    assert!(!m.contains(TileId::new(0, 0)));
    assert_eq!(m.len(), 1);
    assert!(
        !m.purge(TileId::new(0, 0)),
        "purging an absent tile is a no-op"
    );

    m.clear();
    assert!(m.is_empty());
    assert_eq!(m.iter_features().count(), 0);
}
