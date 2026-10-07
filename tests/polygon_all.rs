//! [`PolygonSlicerAll`] against its oracle, [`PolygonSlicerOne`]: for every tile of the reachable
//! span, an edge tile's pieces must equal what a single-tile slicer produces there for each of the
//! feature's polygons, a fill-run tile must be one the single-tile slicer fills with whole-tile boxes
//! only, and every other tile must be one it leaves empty.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use geo_types::Coord;
use map_tile_toolkit::{FillRun, PolygonSlicerAll, PolygonSlicerOne, TileId};

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

/// One feature's output: edge tiles' pieces and the tiles its fill runs cover.
#[derive(Debug, PartialEq, Eq)]
struct Sliced {
    tiles: BTreeMap<TileId, Pieces>,
    fills: BTreeSet<TileId>,
}

/// `polygons` sliced as one multipolygon feature.
fn all_tiles(polygons: &[FixturePolygon], extent: u32, buffer: u16) -> Sliced {
    let mut all = PolygonSlicerAll::<Coord<i32>>::new(extent, buffer).expect("config");
    all.add_feature(rings_of(polygons)).expect("slice");
    assert!(all.len() <= 1);
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

/// Gather every feature's output, checking the structural promises along the way: edge tiles and
/// runs in row-major order, runs non-empty and maximal, and no tile both an edge tile and filled.
fn collect(all: &PolygonSlicerAll<Coord<i32>>) -> Sliced {
    let mut tiles = BTreeMap::new();
    let mut fills = BTreeSet::new();
    for f in all.iter_features() {
        let ids: Vec<TileId> = f.iter_tiles().map(|t| t.tile_id()).collect();
        assert!(
            ids.is_sorted_by_key(|t| (t.y, t.x)),
            "edge tiles in row-major order"
        );
        for t in f.iter_tiles() {
            let pieces: Pieces = t
                .iter_polygons()
                .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
                .collect();
            assert!(!pieces.is_empty(), "an edge tile always carries geometry");
            assert!(
                tiles.insert(t.tile_id(), pieces).is_none(),
                "tile listed twice"
            );
        }
        let runs: Vec<FillRun> = f.iter_fill_runs().collect();
        for w in runs.windows(2) {
            assert!(
                (w[0].y, w[0].x.end) < (w[1].y, w[1].x.start),
                "runs are row-major, disjoint and maximal: {w:?}"
            );
        }
        for run in &runs {
            assert!(!run.x.is_empty(), "empty run {run:?}");
            for x in run.x.clone() {
                let tile = TileId::new(x, run.y);
                assert!(!tiles.contains_key(&tile), "{tile:?} is both edge and fill");
                fills.insert(tile);
            }
        }
    }
    Sliced { tiles, fills }
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
    let in_span = |t: &TileId| (lo.x..=hi.x).contains(&t.x) && (lo.y..=hi.y).contains(&t.y);
    assert!(
        all.fills.iter().all(in_span),
        "{label}: a fill run escapes the span"
    );
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            let one = one_tile(polygons, extent, buffer, tile);
            let at = format!("{label}: extent {extent} buffer {buffer} tile {tile:?}");
            if let Some(pieces) = all.tiles.get(&tile) {
                assert_eq!(pieces, &one, "{at}");
            } else if all.fills.contains(&tile) {
                assert!(
                    only_fill_boxes(&one, extent),
                    "{at}: filled, but one = {one:?}"
                );
            } else {
                assert!(one.is_empty(), "{at}: missing {one:?}");
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
/// A star-shaped (hence simple) ring around `center`: angles increasing, radii in `r/2..r`. Wide
/// interiors, so many tiles are fully covered; `reverse` flips the winding.
fn star_ring(rng: &mut Rng, center: (i32, i32), r: i32, reverse: bool) -> Vec<Coord<i32>> {
    let n = rng.range(5, 40);
    let mut ring: Vec<Coord<i32>> = (0..n)
        .map(|i| {
            let angle = std::f64::consts::TAU * f64::from(i) / f64::from(n);
            let radius = f64::from(rng.range(r / 2, r));
            Coord {
                x: center.0 + (radius * angle.cos()).round() as i32,
                y: center.1 + (radius * angle.sin()).round() as i32,
            }
        })
        .collect();
    if reverse {
        ring.reverse();
    }
    ring
}

#[test]
fn random_star_multipolygons_match_one() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut fills = 0;
    for case in 0..300 {
        let polygons: Vec<FixturePolygon> = (0..rng.range(1, 4))
            .map(|_| {
                let center = (rng.range(-60, 60), rng.range(-60, 60));
                let r = rng.range(20, 80);
                FixturePolygon {
                    exterior: star_ring(&mut rng, center, r, case % 3 == 0),
                    // Holes inside the exterior's inner radius, so the polygon stays valid-ish.
                    holes: (0..rng.range(0, 3))
                        .map(|_| {
                            let c = (
                                center.0 + rng.range(-r / 4, r / 4 + 1),
                                center.1 + rng.range(-r / 4, r / 4 + 1),
                            );
                            star_ring(&mut rng, c, (r / 5).max(4), case % 2 == 0)
                        })
                        .collect(),
                }
            })
            .collect();
        let extent = [4, 7, 10, 25][case % 4];
        let buffer = [0, 1, 3][(case / 4) % 3] % (extent as u16).div_ceil(2);
        check(&polygons, extent, buffer, &format!("star case {case}"));
        fills += all_tiles(&polygons, extent, buffer).fills.len();
    }
    assert!(
        fills > 10_000,
        "the star cases exercise interior fill ({fills} tiles)"
    );
}

/// A z14-like ocean: a square spanning 16384 × 16384 tiles of extent 4096 (2^28 tiles) with a
/// 21 × 21-tile hole. Interior tiles must come back as one or two runs per row — bounded memory and
/// time proportional to the perimeter and the rows — and the routing budget (2^25 candidate tiles)
/// must not apply to them.
#[test]
fn huge_polygon_fills_by_runs() {
    const N: i32 = 1 << 14;
    const E: i32 = 4096;
    let (lo, hi) = (100, N * E - 100);
    let exterior = [(lo, lo), (hi, lo), (hi, hi), (lo, hi)].map(|(x, y)| Coord { x, y });
    // Hole edges sit mid-tile in columns/rows N/2 ± 10, so it has 19 × 19 tiles strictly inside.
    let (h0, h1) = ((N / 2 - 10) * E + 2000, (N / 2 + 10) * E + 2000);
    let hole = [(h0, h0), (h0, h1), (h1, h1), (h1, h0)].map(|(x, y)| Coord { x, y });

    let mut all = PolygonSlicerAll::<Coord<i32>>::new(E as u32, 64).expect("config");
    all.add_feature([[&exterior[..], &hole[..]]])
        .expect("huge polygon slices");
    let feature = all.iter_features().next().expect("one feature");

    let edge_tiles = feature.iter_tiles().count();
    assert_eq!(edge_tiles, 4 * N as usize - 4 + (21 * 21 - 19 * 19));
    let runs: Vec<FillRun> = feature.iter_fill_runs().collect();
    assert_eq!(
        runs.len(),
        (N - 2) as usize + 21,
        "one run per row, two beside the hole"
    );
    let filled: u64 = runs.iter().map(|r| r.x.len() as u64).sum();
    assert_eq!(filled, u64::from((N - 2) as u32).pow(2) - 21 * 21);

    // Spot-check against the single-tile oracle: an edge tile of each ring, a filled tile, and a
    // tile inside the hole.
    let polygon = FixturePolygon {
        exterior: exterior.to_vec(),
        holes: vec![hole.to_vec()],
    };
    let one = |tile| one_tile(std::slice::from_ref(&polygon), E as u32, 64, tile);
    for tile in [
        TileId::new(0, 0),
        TileId::new(N - 1, 77),
        TileId::new(N / 2 - 10, N / 2),
    ] {
        let t = feature
            .iter_tiles()
            .find(|t| t.tile_id() == tile)
            .expect("edge tile");
        let pieces: Pieces = t
            .iter_polygons()
            .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
            .collect();
        assert_eq!(pieces, one(tile), "{tile:?}");
    }
    let fill = TileId::new(5, 9);
    assert!(runs.iter().any(|r| r.y == fill.y && r.x.contains(&fill.x)));
    assert!(only_fill_boxes(&one(fill), E as u32));
    assert!(one(TileId::new(N / 2, N / 2)).is_empty(), "inside the hole");
}
