//! [`PolygonSlicerAll`] against its oracle, [`PolygonSlicerOne`]: for every tile of the reachable
//! span, an edge tile's pieces must equal what a single-tile slicer produces there for each of the
//! feature's polygons, and every other tile must be one the single-tile slicer leaves empty.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::Coord;
use map_tile_toolkit::{PolygonSlicerAll, PolygonSlicerOne, TileId};

mod support;

use crate::support::{EXTENT, FixturePolygon};

/// A tile's pieces: per polygon, its rings (exterior first), tile-local.
type Pieces = Vec<Vec<Vec<Coord<i32>>>>;

/// What [`PolygonSlicerOne`] yields for `tile`, adding each polygon as its own feature.
fn one_tile(polygons: &[FixturePolygon], extent: u32, buffer: u16, tile: TileId) -> Pieces {
    let mut one = PolygonSlicerOne::<Coord<i32>>::new(extent, buffer, tile).expect("config");
    for p in polygons {
        let holes: Vec<&[Coord<i32>]> = p.holes.iter().map(Vec::as_slice).collect();
        one.add_feature(&p.exterior, &holes).expect("clip");
    }
    one.iter_features()
        .map(|f| f.iter_rings().map(|r| r.vertices().to_vec()).collect())
        .collect()
}

/// Every edge tile of `polygons` sliced as one multipolygon feature.
fn all_tiles(polygons: &[FixturePolygon], extent: u32, buffer: u16) -> BTreeMap<TileId, Pieces> {
    let mut all = PolygonSlicerAll::<Coord<i32>>::new(extent, buffer).expect("config");
    all.add_feature(rings_of(polygons)).expect("slice");
    collect(&all)
}

fn rings_of(polygons: &[FixturePolygon]) -> Vec<Vec<&[Coord<i32>]>> {
    polygons
        .iter()
        .map(|p| {
            std::iter::once(p.exterior.as_slice())
                .chain(p.holes.iter().map(Vec::as_slice))
                .collect()
        })
        .collect()
}

fn collect(all: &PolygonSlicerAll<Coord<i32>>) -> BTreeMap<TileId, Pieces> {
    let mut out = BTreeMap::new();
    for f in all.iter_features() {
        for t in f.iter_tiles() {
            let pieces: Pieces = t
                .iter_polygons()
                .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
                .collect();
            assert!(
                out.insert(t.tile_id(), pieces).is_none(),
                "tile listed twice"
            );
        }
    }
    out
}

/// Inclusive tile span every piece can reach (one tile of padding covers any buffer < extent / 2).
fn span(polygons: &[FixturePolygon], extent: u32) -> (TileId, TileId) {
    let e = extent as i32;
    let (mut lo, mut hi) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
    for c in polygons
        .iter()
        .flat_map(|p| p.exterior.iter().chain(p.holes.iter().flatten()))
    {
        let (x, y) = (c.x.div_euclid(e), c.y.div_euclid(e));
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    }
    (
        TileId::new(lo.0 - 1, lo.1 - 1),
        TileId::new(hi.0 + 1, hi.1 + 1),
    )
}

/// Whether every piece is a single synthetic ring wholly outside the tile's core — a fill box.
fn only_fill_boxes(pieces: &Pieces, extent: u32) -> bool {
    let e = extent as i32;
    !pieces.is_empty()
        && pieces.iter().all(|p| {
            p.len() == 1
                && p[0].len() == 5
                && p[0]
                    .iter()
                    .all(|c| !(0..e).contains(&c.x) && !(0..e).contains(&c.y))
        })
}

/// Assert the all-tiles slicer agrees with the single-tile oracle on every tile of the span.
fn check(polygons: &[FixturePolygon], extent: u32, buffer: u16, label: &str) {
    let all = all_tiles(polygons, extent, buffer);
    let (lo, hi) = span(polygons, extent);
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            let one = one_tile(polygons, extent, buffer, tile);
            match all.get(&tile) {
                Some(pieces) => assert_eq!(
                    pieces, &one,
                    "{label}: extent {extent} buffer {buffer} tile {tile:?}"
                ),
                None => assert!(
                    one.is_empty() || only_fill_boxes(&one, extent),
                    "{label}: extent {extent} buffer {buffer} tile {tile:?} missing: {one:?}"
                ),
            }
        }
    }
}

mod files {
    use test_each_file::test_each_path;

    test_each_path! { for ["geojson"] in "./tests/polygons/fixtures" => super::fixture_matches_one }
}

fn fixture_matches_one([path]: [&Path; 1]) {
    let polygons = support::load_polygon_fixture(path);
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    for buffer in [0, 5] {
        check(&polygons, EXTENT, buffer, stem);
    }
    // Finer grids make the same geometry wrap many more tiles.
    for (extent, buffer) in [(7, 0), (7, 3), (4, 1), (1, 0)] {
        check(&polygons, extent, buffer, stem);
    }
}

/// A tiny deterministic PRNG (xorshift64*), so the random cases need no extra dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo) as u64) as i32
    }
}

fn random_ring(rng: &mut Rng, lo: i32, hi: i32) -> Vec<Coord<i32>> {
    let n = rng.range(3, 12);
    (0..n)
        .map(|_| Coord {
            x: rng.range(lo, hi),
            y: rng.range(lo, hi),
        })
        .collect()
}

#[test]
fn random_multipolygons_match_one() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..400 {
        let polygons: Vec<FixturePolygon> = (0..rng.range(1, 4))
            .map(|_| FixturePolygon {
                exterior: random_ring(&mut rng, -40, 40),
                holes: (0..rng.range(0, 3))
                    .map(|_| random_ring(&mut rng, -40, 40))
                    .collect(),
            })
            .collect();
        let extent = [5, 9, 16, 25][case % 4];
        let buffer = [0, 1, 2, 4][(case / 4) % 4] % (extent as u16).div_ceil(2);
        check(&polygons, extent, buffer, &format!("random case {case}"));
    }
}
