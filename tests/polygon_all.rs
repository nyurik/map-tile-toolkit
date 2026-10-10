//! [`PolygonSlicerAll`] against its oracle, [`PolygonSlicerOne`]: for every tile of the reachable
//! span, an edge tile's pieces must equal what a single-tile slicer produces there for each of the
//! feature's polygons, a fill-run tile's fill rings must equal it too, and every other tile must be
//! one it leaves empty. (The fixtures' snapshots, shared by both slicers, are in `clip_polygon.rs`.)
//!
//! Also here: fill coverage against an independent `geo` point-in-polygon of each tile center;
//! reassembly of the edge tiles with [`PolygonMosaic`]; all of it again under every rotation and
//! mirror image of the fixtures; planetiler's tricky hole cases; and the large-input bounds.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use geo::Contains;
use geo_types::{Coord, LineString, Point, Polygon};
use map_tile_toolkit::{
    FillRun, Measured, PolygonMosaic, PolygonSlicerAll, PolygonSlicerOne, TileId,
};

mod support;

use crate::support::{EXTENT, FixturePolygon};

/// One polygon's rings (exterior first), tile-local.
type Rings = Vec<Vec<Coord<i32>>>;

/// A tile's pieces: per polygon, its rings.
type Pieces = Vec<Rings>;

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

/// One feature's output: edge tiles' pieces, and each filled tile's fill rings (in polygon order).
#[derive(Debug, PartialEq, Eq)]
struct Sliced {
    tiles: BTreeMap<TileId, Pieces>,
    fills: BTreeMap<TileId, Pieces>,
}

/// `polygons` sliced as one multipolygon feature.
fn all_tiles(polygons: &[FixturePolygon], extent: u32, buffer: u16) -> Sliced {
    let mut all = PolygonSlicerAll::<Coord<i32>>::new(extent, buffer).expect("config");
    all.add_feature(rings_of(polygons)).expect("slice");
    assert!(all.len() <= 1);
    collect(&all)
}

fn rings_of(polygons: &[FixturePolygon]) -> Vec<Vec<&[Coord<i32>]>> {
    polygons.iter().map(support::rings).collect()
}

/// Gather every feature's output, checking the structural promises along the way: edge tiles in
/// row-major order, runs in row-major then polygon order, non-empty and maximal per polygon, and no
/// tile both an edge tile and filled.
fn collect(all: &PolygonSlicerAll<Coord<i32>>) -> Sliced {
    let mut tiles = BTreeMap::new();
    let mut fills: BTreeMap<TileId, Vec<(u32, Rings)>> = BTreeMap::new();
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
                (w[0].y, w[0].x.start, w[0].polygon) < (w[1].y, w[1].x.start, w[1].polygon),
                "runs are row-major, then by polygon: {w:?}"
            );
        }
        for (i, run) in runs.iter().enumerate() {
            assert!(!run.x.is_empty(), "empty run {run:?}");
            assert!(
                runs[i + 1..]
                    .iter()
                    .filter(|r| r.y == run.y && r.polygon == run.polygon)
                    .all(|r| r.x.start > run.x.end),
                "a polygon's runs in a row are disjoint and maximal: {run:?}"
            );
            let ring = f.fill_ring(run).to_vec();
            for x in run.x.clone() {
                let tile = TileId::new(x, run.y);
                assert!(!tiles.contains_key(&tile), "{tile:?} is both edge and fill");
                fills
                    .entry(tile)
                    .or_default()
                    .push((run.polygon, vec![ring.clone()]));
            }
        }
    }
    let fills = fills
        .into_iter()
        .map(|(tile, mut polys)| {
            polys.sort_by_key(|(polygon, _)| *polygon);
            (tile, polys.into_iter().map(|(_, rings)| rings).collect())
        })
        .collect();
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

/// Assert the all-tiles slicer agrees with the single-tile oracle on every tile of the span.
fn check(polygons: &[FixturePolygon], extent: u32, buffer: u16, label: &str) {
    let all = all_tiles(polygons, extent, buffer);
    // Consecutive duplicate vertices are dropped, so repeating every vertex changes nothing.
    let dup = |ring: &[Coord<i32>]| ring.iter().flat_map(|&c| [c, c]).collect::<Vec<_>>();
    let duped: Vec<FixturePolygon> = polygons
        .iter()
        .map(|p| FixturePolygon {
            exterior: dup(&p.exterior),
            holes: p.holes.iter().map(|h| dup(h)).collect(),
        })
        .collect();
    assert_eq!(
        all,
        all_tiles(&duped, extent, buffer),
        "{label}: duplicated vertices"
    );
    let (lo, hi) = span(polygons, extent);
    let in_span = |t: &TileId| (lo.x..=hi.x).contains(&t.x) && (lo.y..=hi.y).contains(&t.y);
    assert!(
        all.fills.keys().all(in_span),
        "{label}: a fill run escapes the span"
    );
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            let tile = TileId::new(x, y);
            let one = one_tile(polygons, extent, buffer, tile);
            let at = format!("{label}: extent {extent} buffer {buffer} tile {tile:?}");
            if let Some(pieces) = all.tiles.get(&tile) {
                assert_eq!(pieces, &one, "{at}");
            } else if let Some(pieces) = all.fills.get(&tile) {
                assert_eq!(pieces, &one, "{at}: fill");
            } else {
                assert!(one.is_empty(), "{at}: missing {one:?}");
            }
        }
    }
}

mod files {
    use test_each_file::test_each_path;

    test_each_path! { for ["geojson"] in "./tests/polygons/fixtures" => super::fixture }
}

fn fixture([path]: [&Path; 1]) {
    fixture_matches_one(path);
    fixture_fill_matches_geo(path);
    fixture_reassembles(path);
    fixture_symmetries(path);
}

fn fixture_matches_one(path: &Path) {
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

/// The tile center (a point of its core cell) in the global frame.
fn center(tile: TileId, extent: u32) -> Point<f64> {
    let e = extent as i32;
    Point::new(f64::from(tile.x * e + e / 2), f64::from(tile.y * e + e / 2))
}

fn geo_polygon(p: &FixturePolygon) -> Polygon<f64> {
    let ring = |r: &[Coord<i32>]| {
        LineString::from(
            r.iter()
                .map(|c| (f64::from(c.x), f64::from(c.y)))
                .collect::<Vec<_>>(),
        )
    };
    Polygon::new(ring(&p.exterior), p.holes.iter().map(|h| ring(h)).collect())
}

/// Every tile no edge touches is filled iff its center lies in some polygon, by `geo`'s independent
/// point-in-polygon.
fn fixture_fill_matches_geo(path: &Path) {
    let polygons = support::load_polygon_fixture(path);
    let geo: Vec<Polygon<f64>> = polygons.iter().map(geo_polygon).collect();
    for (extent, buffer) in [(EXTENT, 0), (EXTENT, 5), (7, 1), (3, 0)] {
        let all = all_tiles(&polygons, extent, buffer);
        let (lo, hi) = span(&polygons, extent);
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let tile = TileId::new(x, y);
                if all.tiles.contains_key(&tile) {
                    continue;
                }
                let inside = geo.iter().any(|p| p.contains(&center(tile, extent)));
                assert_eq!(
                    all.fills.contains_key(&tile),
                    inside,
                    "{}: extent {extent} buffer {buffer} tile {tile:?}",
                    path.display()
                );
            }
        }
    }
}

/// Directed-edge set of closed rings, skipping zero-length edges.
fn edge_set<'a>(
    rings: impl IntoIterator<Item = &'a [Coord<i32>]>,
) -> HashSet<(Coord<i32>, Coord<i32>)> {
    rings
        .into_iter()
        .flat_map(|r| r.windows(2))
        .filter(|w| w[0] != w[1])
        .map(|w| (w[0], w[1]))
        .collect()
}

/// The edge tiles of every feature reassemble with [`PolygonMosaic`] into the input rings' edges
/// (fill runs carry no original edge, so they are not needed).
fn fixture_reassembles(path: &Path) {
    let features = support::load_polygon_features(path);
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    for buffer in [0, 5] {
        reassembles(&features, EXTENT, buffer, stem);
    }
}

/// Fixtures with a hole outside its shell (an invalid polygon): a tile the shell does not reach
/// drops that hole, so only the shell's edges are sure to come back.
const HOLE_OUTSIDE_SHELL: &[&str] = &["only_hole_touches_other_tile"];

/// Slice `features` with [`PolygonSlicerAll`], feed every edge tile to a [`PolygonMosaic`], and
/// assert the result has exactly the input rings' edges (for a fixture in [`HOLE_OUTSIDE_SHELL`]:
/// every shell edge, and nothing that is not an input edge).
fn reassembles(features: &[Vec<FixturePolygon>], extent: u32, buffer: u16, label: &str) {
    let input: Vec<&[Coord<i32>]> = features.iter().flatten().flat_map(support::rings).collect();
    let mut all = PolygonSlicerAll::<Coord<i32>>::new(extent, buffer).expect("config");
    for f in features {
        all.add_feature(rings_of(f)).expect("slice");
    }
    let mut per_tile: BTreeMap<TileId, Vec<Vec<Coord<i32>>>> = BTreeMap::new();
    for f in all.iter_features() {
        for t in f.iter_tiles() {
            let rings = t
                .iter_polygons()
                .flat_map(|p| p.iter_rings().map(|r| r.vertices().to_vec()));
            per_tile.entry(t.tile_id()).or_default().extend(rings);
        }
    }
    let mut mosaic = PolygonMosaic::<Coord<i32>>::new(extent, buffer).expect("config");
    for (tile, rings) in &per_tile {
        mosaic
            .add(*tile, rings)
            .expect("the slicer's own tiles never conflict");
    }
    let rebuilt: Vec<Vec<Coord<i32>>> = mosaic.iter_features().collect();
    let rebuilt = edge_set(rebuilt.iter().map(Vec::as_slice));
    let input = edge_set(input.iter().copied());
    let at = format!("{label}: reassembly at extent {extent} buffer {buffer}");
    if HOLE_OUTSIDE_SHELL
        .iter()
        .any(|stem| label.starts_with(stem))
    {
        let shells = edge_set(features.iter().flatten().map(|p| p.exterior.as_slice()));
        assert!(rebuilt.is_subset(&input), "{at}: invented edges");
        assert!(shells.is_subset(&rebuilt), "{at}: lost shell edges");
    } else {
        assert_eq!(rebuilt, input, "{at}");
    }
}

/// The eight symmetries of the tile grid: `k % 4` quarter turns about the origin, after a mirror
/// across the y axis when `k >= 4`. All are exact in integers and map every tile's buffered box onto
/// another's, so slicing a transformed polygon must give the transformed tiles. A single mirror flips
/// the winding, so its rings are reversed to keep exteriors and holes wound as they were.
fn dihedral(polygons: &[FixturePolygon], k: u8) -> Vec<FixturePolygon> {
    let mirror = k >= 4;
    let map = |c: &Coord<i32>| {
        let mut c = if mirror {
            Coord { x: -c.x, y: c.y }
        } else {
            *c
        };
        for _ in 0..k % 4 {
            c = Coord { x: -c.y, y: c.x };
        }
        c
    };
    let ring = |r: &[Coord<i32>]| {
        let mut r: Vec<Coord<i32>> = r.iter().map(map).collect();
        if mirror {
            r.reverse();
        }
        r
    };
    let out: Vec<FixturePolygon> = polygons
        .iter()
        .map(|p| FixturePolygon {
            exterior: ring(&p.exterior),
            holes: p.holes.iter().map(|h| ring(h)).collect(),
        })
        .collect();
    let windings = |ps: &[FixturePolygon]| -> Vec<bool> {
        ps.iter()
            .flat_map(support::rings)
            .map(|r| twice_area(r) > 0)
            .collect()
    };
    assert_eq!(
        windings(&out),
        windings(polygons),
        "symmetry {k} keeps winding"
    );
    out
}

/// Twice the signed (shoelace) area of a closed or open ring.
fn twice_area(ring: &[Coord<i32>]) -> i64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            i64::from(a.x) * i64::from(b.y) - i64::from(b.x) * i64::from(a.y)
        })
        .sum()
}

/// The tile that `tile` becomes under [`dihedral`] symmetry `k` (the one its center maps into).
fn dihedral_tile(tile: TileId, extent: u32, k: u8) -> TileId {
    // Doubled coordinates keep the center on the integer grid.
    let e = extent as i32;
    let center = |t: i32| (2 * t + 1) * e;
    let poly = FixturePolygon {
        exterior: vec![Coord {
            x: center(tile.x),
            y: center(tile.y),
        }],
        holes: Vec::new(),
    };
    let c = dihedral(&[poly], k)[0].exterior[0];
    TileId::new(c.x.div_euclid(2 * e), c.y.div_euclid(2 * e))
}

/// Every rotation and mirror image of a fixture still matches the single-tile oracle (fill rings
/// included) and still reassembles, at both snapshot buffers and on two finer grids — tiles of 10
/// put boundaries through many fixture vertices, tiles of 7 through few. Each symmetry walks rows
/// and crossings in a different order, so this catches tie-breaks that hold only one way round.
fn fixture_symmetries(path: &Path) {
    let features = support::load_polygon_features(path);
    let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
    for k in 0..8 {
        let features: Vec<Vec<FixturePolygon>> = features.iter().map(|f| dihedral(f, k)).collect();
        let polygons: Vec<FixturePolygon> = features.iter().flatten().cloned().collect();
        let label = format!("{stem} (symmetry {k})");
        for (extent, buffer) in [(EXTENT, 0), (EXTENT, 5), (10, 2), (7, 3)] {
            check(&polygons, extent, buffer, &label);
            reassembles(&features, extent, buffer, &label);
        }
    }
}

/// What one tile of a [`Sliced`] feature holds.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Holds {
    Nothing,
    Edge,
    Fill,
}

fn holds(sliced: &Sliced, tile: TileId) -> Holds {
    if sliced.tiles.contains_key(&tile) {
        Holds::Edge
    } else if sliced.fills.contains_key(&tile) {
        Holds::Fill
    } else {
        Holds::Nothing
    }
}

/// A named polygon, and what some of its tiles (x, y) must hold.
type Case<'a> = (&'a str, FixturePolygon, &'a [(i32, i32, Holds)]);

/// planetiler's `TiledGeometryTest` hole cases in their own frame: its coordinates are in tiles, so
/// at 10 units per tile they land on integers with the tile boundaries where planetiler has them.
/// Each case runs under all eight [`dihedral`] symmetries (planetiler checks seven of them) against
/// the oracle and the mosaic, and makes planetiler's own assertions about particular tiles (plus the
/// tiles they imply for each hole alone).
#[test]
fn planetiler_hole_cases() {
    // Closed, as the fixtures' rings are.
    let ring = |pts: &[(i32, i32)]| {
        pts.iter()
            .chain(&pts[..1])
            .map(|&(x, y)| Coord { x, y })
            .collect::<Vec<_>>()
    };
    let polygon = |rings: &[&[(i32, i32)]]| FixturePolygon {
        exterior: ring(rings[0]),
        holes: rings[1..].iter().map(|r| ring(r)).collect(),
    };
    let outer: &[(i32, i32)] = &[(10, 10), (100, 10), (100, 100), (10, 100)];
    // testOverlappingHoles: a chevron hole, and a triangle hole in its notch.
    let chevron: &[(i32, i32)] = &[(20, 20), (20, 90), (90, 90), (30, 50), (90, 20)];
    let in_notch: &[(i32, i32)] = &[(90, 30), (90, 80), (40, 50)];
    let overlapping = [
        (3, 3, Holds::Nothing),
        (7, 4, Holds::Nothing),
        (1, 1, Holds::Edge),
        (9, 9, Holds::Edge),
    ];
    // testInsideComplexHole: a hole wrapped round an island of polygon, with a speck hole on it.
    let complex: &[(i32, i32)] = &[
        (65, 15),
        (20, 20),
        (20, 90),
        (90, 90),
        (90, 20),
        (46, 20),
        (80, 80),
        (30, 80),
        (40, 20),
    ];
    let speck: &[(i32, i32)] = &[(55, 65), (55, 66), (56, 66)];
    let island = [
        (5, 5, Holds::Fill),
        (4, 6, Holds::Fill),
        (5, 6, Holds::Edge),
    ];
    // testSideOfHoleIntercepted: the chevron's arm sends a finger back into tile (7, 4).
    let finger: &[(i32, i32)] = &[
        (20, 20),
        (20, 90),
        (90, 90),
        (30, 50),
        (90, 20),
        (90, 42),
        (75, 42),
        (75, 48),
        (95, 48),
        (95, 18),
    ];
    // testOnlyHoleTouchesOtherCellBottom: a hole outside its speck of a shell touches tile (1, 2).
    let speck_shell: &[(i32, i32)] = &[(15, 15), (16, 15), (15, 16)];
    let touching: &[(i32, i32)] = &[(14, 18), (16, 18), (15, 20)];

    let cases: [Case<'_>; 7] = [
        (
            "overlapping 1",
            polygon(&[outer, chevron]),
            &[(3, 3, Holds::Nothing), (7, 4, Holds::Fill)],
        ),
        (
            "overlapping 2",
            polygon(&[outer, in_notch]),
            &[(3, 3, Holds::Fill), (7, 4, Holds::Nothing)],
        ),
        (
            "overlapping 1 2",
            polygon(&[outer, chevron, in_notch]),
            &overlapping,
        ),
        (
            "overlapping 2 1",
            polygon(&[outer, in_notch, chevron]),
            &overlapping,
        ),
        ("complex", polygon(&[outer, complex, speck]), &island),
        ("finger", polygon(&[outer, finger]), &[(7, 4, Holds::Edge)]),
        (
            // Named so `reassembles` expects only the shell back.
            HOLE_OUTSIDE_SHELL[0],
            polygon(&[speck_shell, touching]),
            &[(1, 1, Holds::Edge), (1, 2, Holds::Nothing)],
        ),
    ];
    for (name, polygon, expect) in &cases {
        for k in 0..8 {
            let polygons = dihedral(std::slice::from_ref(polygon), k);
            let label = format!("{name} (symmetry {k})");
            for buffer in [0, 1, 3] {
                check(&polygons, 10, buffer, &label);
                reassembles(std::slice::from_ref(&polygons), 10, buffer, &label);
            }
            // planetiler's assertions, made without a buffer.
            let sliced = all_tiles(&polygons, 10, 0);
            for &(x, y, want) in *expect {
                let tile = dihedral_tile(TileId::new(x, y), 10, k);
                assert_eq!(
                    holds(&sliced, tile),
                    want,
                    "{label}: tile ({x}, {y}) -> {tile:?}"
                );
            }
        }
    }
}

/// Overlapping polygons (an invalid multipolygon) fill the shared tiles once each, in polygon order,
/// exactly as the single-tile slicer does — even where the later polygon's run starts first, and one
/// is wound the other way.
#[test]
fn overlapping_polygons_match_one() {
    let square = |x0: i32, y0: i32, side: i32, ccw: bool| {
        let mut ring = vec![
            Coord { x: x0, y: y0 },
            Coord {
                x: x0 + side,
                y: y0,
            },
            Coord {
                x: x0 + side,
                y: y0 + side,
            },
            Coord {
                x: x0,
                y: y0 + side,
            },
        ];
        if !ccw {
            ring.reverse();
        }
        FixturePolygon {
            exterior: ring,
            holes: Vec::new(),
        }
    };
    let polygons = [
        square(50, 3, 100, true),
        square(2, 0, 100, false),
        square(20, 20, 50, true),
    ];
    for (extent, buffer) in [(7, 0), (7, 3), (10, 1), (4, 0)] {
        check(&polygons, extent, buffer, "overlapping");
    }
    let all = all_tiles(&polygons, 7, 0);
    assert!(
        all.fills.values().any(|polys| polys.len() == 3),
        "some tile is filled by all three polygons"
    );
}

/// Many holes, from specks inside one tile to holes containing whole tiles, some overlapping: every
/// tile still matches the oracle, now that untouched holes are settled by count rather than visited.
#[test]
fn many_holes_match_one() {
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut below = |n: i32| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        i32::try_from(state % u64::from(n.unsigned_abs())).expect("fits")
    };
    let square = |x: i32, y: i32, side: i32| {
        [(x, y), (x + side, y), (x + side, y + side), (x, y + side)]
            .map(|(x, y)| Coord { x, y })
            .to_vec()
    };
    let holes = (0..60)
        .map(|i| {
            let side = if i % 6 == 0 {
                30 + below(60)
            } else {
                1 + below(8)
            };
            square(10 + below(380 - side), 10 + below(380 - side), side)
        })
        .collect();
    let polygon = FixturePolygon {
        exterior: square(0, 0, 400),
        holes,
    };
    for buffer in [0, 1, 3] {
        check(std::slice::from_ref(&polygon), 16, buffer, "many holes");
    }
}

#[test]
fn payload_and_attribute_ride_through() {
    // Each vertex carries its index as an M value; synthetic corners carry the default (0).
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/polygons/fixtures/big_fill.geojson");
    let polygons = support::load_polygon_fixture(&path);
    let mut m = 0;
    let mut measured = |ring: &[Coord<i32>]| -> Vec<Measured<u32>> {
        ring.iter()
            .map(|c| {
                m += 1;
                Measured::new(c.x, c.y, m)
            })
            .collect()
    };
    let rings: Vec<Vec<Vec<Measured<u32>>>> = polygons
        .iter()
        .map(|p| {
            std::iter::once(measured(&p.exterior))
                .chain(p.holes.iter().map(|h| measured(h)))
                .collect()
        })
        .collect();

    let mut all = PolygonSlicerAll::<Measured<u32>, &str>::new(EXTENT, 3).expect("config");
    all.add_feature_with(&rings, "lake").expect("slice");
    assert_eq!(all.len(), 1);
    let feature = all.iter_features().next().expect("feature");
    assert_eq!(*feature.attr(), "lake");
    assert!(feature.iter_fill_runs().count() > 0);
    for t in feature.iter_tiles() {
        let mut one =
            PolygonSlicerOne::<Measured<u32>>::new(EXTENT, 3, t.tile_id()).expect("config");
        for p in &rings {
            let holes: Vec<&[Measured<u32>]> = p[1..].iter().map(Vec::as_slice).collect();
            one.add_feature(&p[0], &holes).expect("clip");
        }
        let expected: Vec<Vec<Vec<Measured<u32>>>> = one
            .iter_features()
            .map(|f| f.iter_rings().map(|r| r.vertices().to_vec()).collect())
            .collect();
        let got: Vec<Vec<Vec<Measured<u32>>>> = t
            .iter_polygons()
            .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
            .collect();
        assert_eq!(got, expected, "{:?}", t.tile_id());
        // Every polygon carries its feature's attribute, and the views are plain copies.
        let tile = t;
        assert!(tile.iter_polygons().all(|p| *p.attr() == "lake"));
        assert_eq!(t.tile_id(), tile.tile_id());
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
    // The exterior sits 100 units inside the outer tiles; the hole's edges sit mid-tile in
    // columns/rows N/2 ± 10, so it has 19 × 19 tiles strictly inside.
    let polygon = support::huge_square();
    let mut all = PolygonSlicerAll::<Coord<i32>>::new(E as u32, 64).expect("config");
    all.add_feature([support::rings(&polygon)])
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
    let run = runs
        .iter()
        .find(|r| r.y == fill.y && r.x.contains(&fill.x))
        .expect("a run covers the tile");
    assert_eq!(one(fill), vec![vec![feature.fill_ring(run).to_vec()]]);
    assert!(one(TileId::new(N / 2, N / 2)).is_empty(), "inside the hole");
}
