//! Slicing benchmarks, measured by **CPU instruction count** under Valgrind/Callgrind via
//! [`gungraun`](https://docs.rs/gungraun) — one-shot and deterministic, so thermal drift, frequency
//! scaling, and neighbor noise cannot move the numbers (unlike wall-clock timing).
//!
//! Running them needs Valgrind installed (`sudo apt-get install valgrind`) plus the matching runner
//! (`cargo install gungraun-runner`), and they do **not** run on arm64 / Apple Silicon. Compiling
//! (`cargo bench --no-run`) works anywhere.
//!
//! Each benchmark runs its body **once** under Callgrind; everything that must not be counted — fixture
//! loading, building the big polyline, and (for `one`) precomputing tile ids — happens in a `setup`
//! function, whose result is handed to the measured function. That function hands its input back, so
//! freeing it happens outside the count too. Two operations, each over the same input
//! scenarios:
//! * `all` — [`Slicer::line`](map_tile_toolkit::Slicer::line) on each polyline, reading every
//!   vertex of every slice (what a consumer does; the slices are lazy).
//! * `one` — [`Slicer::tile_line`](map_tile_toolkit::Slicer::tile_line) per touched tile, likewise.
//!
//! Scenarios: `small` (the `tests/polylines/fixtures/*.geojson` set on the extent-25 grid) and the
//! single large [`support::big_polyline`] sliced into many / a few / a single tile (`big_multi`,
//! `big_few`, `big_single`), plus — for `all` only — `diagonal_4k` / `diagonal_z14`: one
//! [`support::long_diagonal`] segment across 4000 × 4000 / 16384 × 16384 (a z14 world) tiles of
//! extent 4096, whose routing must cost the tiles it touches, not its bounding box.
//!
//! The `*_unclipped` benchmarks run `all`, `one`, `polygon_all` and `polygon_one` through an
//! unclipped slicer ([`ClipMode::Unclipped`]), which gives every tile the whole geometry: they read
//! each slice's tile and view but not its vertices, which are the same input in every tile, so they
//! count the slicer's work, not a consumer's. `huge_fill` and the polygon diagonals are left out:
//! unclipped, their 10⁷–10⁸ covered tiles each come as a piece.
//!
//! The point benchmarks (`points_all`, `points_one`) slice a 20 000-point feature, point by point, on a 4 × 4 grid of
//! extent 4096, scattered or packed into the buffer bands (every point in two to four tiles).
//!
//! The polygon benchmarks mirror them, through one reused slicer (a bulk tile generator's per-worker
//! pattern), reading every vertex: `polygon_all` runs
//! [`Slicer::polygon`](map_tile_toolkit::Slicer::polygon) per polygon, `polygon_one`
//! [`Slicer::tile_polygon`](map_tile_toolkit::Slicer::tile_polygon) per edge tile. Scenarios: `small` (every
//! `tests/polygons/fixtures` feature), `buildings` ([`support::buildings`], 2000 four-corner footprints
//! on a z14-like grid), `footprints` ([`support::footprints`], 2000 closed rings of 4 to 8 vertices in
//! OSM's proportions: the bulk of real polygon data) on the same grid and, as `footprints_edge`, on
//! tiles of extent 256 that many of them cross, [`support::big_polygon`] at the three `big_*` scales, and —
//! for `polygon_all` only — `huge_fill`, the z14-like [`support::huge_square`] whose 2^28 covered
//! tiles must cost nothing per tile, `many_holes`, [`support::many_holes`] whose 2000 inner rings
//! must cost their hits, not their count per edge tile, and `diagonal_4k` / `diagonal_z14`, the
//! [`support::diagonal_triangle`] whose hypotenuse is that same long diagonal.
//!
//! **Baselines.** Each operation has traditional counterparts over the same inputs (see
//! `benches/baseline/mod.rs`), suffixed so they sort next to it, that clip each geometry into exactly
//! the tiles the toolkit's all-tiles slicer produces for it (for polygons, its edge tiles plus every
//! tile of its fill runs; for `one`, the edge tiles only, as there):
//! * `*_geo` — `geo`'s `BooleanOps` (`intersection` / `clip`) against each tile's buffered box. A
//!   general sweep-line overlay: an upper bound, not a target. It has no shared work across tiles, so
//!   polylines get only `one_geo` (an `all_geo` would be the same loop).
//! * `*_stripe` — a geojson-vt-style axis-stripe clipper (Sutherland–Hodgman for rings, every ring on
//!   its own): one tile is an x then a y stripe; `all` cuts one stripe per column, then each into its
//!   rows, as geojson-vt splits a tile. The realistic "traditional" target.
//!
//! Both get their `f64` input and tile lists in `setup`, compute intersection points instead of
//! keeping original vertices, and drop each tile's output instead of storing it. `huge_fill` has no
//! baseline (2^28 tiles), and `many_holes` only the stripe one.
//!
//! Filter with e.g. `just bench big`, `just bench big_single`, `just bench all`, `just bench polygon`,
//! `just bench stripe`.

#![allow(clippy::pedantic, reason = "benchmark harness")]
#![allow(
    unused_qualifications,
    reason = "gungraun's macro expansion re-emits the qualified paths from the #[bench(...)] args"
)]

use std::hint::black_box;

use geo::BooleanOps;
use geo_types::{Coord, LineString, MultiLineString, MultiPolygon, Polygon};
use gungraun::{library_benchmark, library_benchmark_group, main};
use map_tile_toolkit::{ClipMode, Slicer, TileId};

mod baseline;
#[path = "../tests/support/mod.rs"]
mod support;

use baseline::Pt;
use support::{Cfg, FixturePolygon};

/// Per-polyline input for the `one` benchmark: a polyline paired with its precomputed touched tiles.
type OneCases = Vec<(Vec<Coord<i32>>, Vec<TileId>)>;

/// Which fixture set a benchmark case runs over (loaded in `setup`, never in the measured region).
#[derive(Clone, Copy)]
enum Input {
    /// The small `tests/polylines/fixtures` set, flattened to component polylines.
    Small,
    /// The single large [`support::big_polyline`] (~3.6k vertices).
    Big,
    /// One [`support::long_diagonal`] segment across this many tiles per axis.
    Diagonal(i32),
}

/// The polylines for an [`Input`] — setup-time work, excluded from the instruction count.
fn load(input: Input) -> support::Polylines {
    match input {
        Input::Small => support::load_all_fixtures()
            .into_iter()
            .flat_map(|(_, polys)| polys)
            .collect(),
        Input::Big => support::lines_of(&support::big_polyline())
            .into_iter()
            .map(<[_]>::to_vec)
            .collect(),
        Input::Diagonal(tiles) => vec![support::long_diagonal(tiles)],
    }
}

/// The tiles a polyline touches, sorted (setup-time work).
fn touched_tiles(cfg: &Cfg, poly: &[Coord<i32>]) -> Vec<TileId> {
    support::slice_all_runs(cfg, &[poly.to_vec()])
        .into_iter()
        .map(|(tile, _)| tile)
        .collect()
}

/// Read every vertex of every slice, as a consumer encoding them would.
fn consume<'a>(slices: impl Iterator<Item = map_tile_toolkit::LineSlice<'a, Coord<i32>>>) {
    for s in slices {
        black_box(s.tile());
        for v in s.run() {
            black_box(v);
        }
    }
}

// ---- `all`: slice every polyline into all touched tiles. ----

/// Prepare the `all` inputs: the slicer config and the polyline set (loading is not measured).
fn setup_all(cfg: Cfg, input: Input) -> (Cfg, support::Polylines) {
    (cfg, load(input))
}

#[library_benchmark(setup = setup_all)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
#[bench::diagonal_4k(support::slicer(4096, 64), Input::Diagonal(4000))]
#[bench::diagonal_z14(support::slicer(4096, 64), Input::Diagonal(1 << 14))]
fn all((cfg, polylines): (Cfg, support::Polylines)) -> support::Polylines {
    let slicer = cfg.slicer();
    for poly in &polylines {
        consume(slicer.line(black_box(poly)).expect("polyline"));
    }
    polylines
}

/// An unclipped slicer for `cfg`.
fn unclipped(cfg: Cfg) -> Slicer {
    cfg.slicer().with_clip_mode(ClipMode::Unclipped)
}

/// Take each unclipped slice's tile and view (its vertices are the whole input, in every tile).
fn consume_whole<'a>(slices: impl Iterator<Item = map_tile_toolkit::LineSlice<'a, Coord<i32>>>) {
    for s in slices {
        black_box((s.tile(), s.run()));
    }
}

#[library_benchmark(setup = setup_all)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
#[bench::diagonal_4k(support::slicer(4096, 64), Input::Diagonal(4000))]
#[bench::diagonal_z14(support::slicer(4096, 64), Input::Diagonal(1 << 14))]
fn all_unclipped((cfg, polylines): (Cfg, support::Polylines)) -> support::Polylines {
    let slicer = unclipped(cfg);
    for poly in &polylines {
        consume_whole(slicer.line(black_box(poly)).expect("polyline"));
    }
    polylines
}

// ---- `one`: slice each polyline into each touched tile. ----

/// Prepare the `one` inputs: the config and, per polyline, its precomputed touched tiles (the
/// tile-id computation is excluded from the measured region, matching the old criterion setup).
fn setup_one(cfg: Cfg, input: Input) -> (Cfg, OneCases) {
    let cases = load(input)
        .into_iter()
        .map(|poly| {
            let tiles = touched_tiles(&cfg, &poly);
            (poly, tiles)
        })
        .collect();
    (cfg, cases)
}

#[library_benchmark(setup = setup_one)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
fn one((cfg, cases): (Cfg, OneCases)) -> OneCases {
    for (poly, tiles) in &cases {
        let poly = black_box(poly);
        for &tile in tiles {
            consume(cfg.slicer().tile_line(tile, poly).expect("polyline"));
        }
    }
    cases
}

#[library_benchmark(setup = setup_one)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
fn one_unclipped((cfg, cases): (Cfg, OneCases)) -> OneCases {
    for (poly, tiles) in &cases {
        let poly = black_box(poly);
        for &tile in tiles {
            consume_whole(unclipped(cfg).tile_line(tile, poly).expect("polyline"));
        }
    }
    cases
}

// ---- Polyline baselines: the same inputs and tiles through traditional clippers. ----

/// A ring or line as `f64` points (setup-time conversion).
fn pts(ring: &[Coord<i32>]) -> Vec<Pt> {
    ring.iter()
        .map(|c| [f64::from(c.x), f64::from(c.y)])
        .collect()
}

/// A ring or line as an `f64` `geo` line string (setup-time conversion).
fn line_string(ring: &[Coord<i32>]) -> LineString<f64> {
    pts(ring).into_iter().map(Coord::from).collect()
}

/// Per geometry: its rings or lines as `f64` points, and the tiles to clip them into (column-major).
type StripeCases = Vec<(Vec<Vec<Pt>>, Vec<TileId>)>;

fn setup_stripe(cfg: Cfg, input: Input) -> (Cfg, StripeCases) {
    let (cfg, cases) = setup_one(cfg, input);
    (
        cfg,
        cases
            .iter()
            .map(|(p, t)| (vec![pts(p)], sorted(t)))
            .collect(),
    )
}

/// `tiles`, sorted column-major as the stripe clipper's `clip_all` takes them.
fn sorted(tiles: &[TileId]) -> Vec<TileId> {
    let mut tiles = tiles.to_vec();
    tiles.sort_unstable();
    tiles
}

/// The stripe clipper's `all`: each geometry into its tiles, one stripe per column.
fn stripe_all(cfg: Cfg, cases: &StripeCases, closed: bool) {
    for (parts, tiles) in cases {
        baseline::clip_all(
            black_box(parts),
            tiles,
            cfg.extent,
            cfg.buffer,
            closed,
            |t, p| {
                black_box((t, p));
            },
        );
    }
}

/// The stripe clipper's `one`: each geometry into each of its tiles separately.
fn stripe_one(cfg: Cfg, cases: &StripeCases, closed: bool) {
    for (parts, tiles) in cases {
        for &tile in tiles {
            let mut out = Vec::new();
            baseline::clip_tile(
                black_box(parts),
                tile,
                cfg.extent,
                cfg.buffer,
                closed,
                &mut out,
            );
            black_box(out);
        }
    }
}

#[library_benchmark(setup = setup_stripe)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
fn all_stripe((cfg, cases): (Cfg, StripeCases)) -> StripeCases {
    stripe_all(cfg, &cases, false);
    cases
}

#[library_benchmark(setup = setup_stripe)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
fn one_stripe((cfg, cases): (Cfg, StripeCases)) -> StripeCases {
    stripe_one(cfg, &cases, false);
    cases
}

/// Per polyline: its `geo` form and its tiles.
type GeoLines = Vec<(MultiLineString<f64>, Vec<TileId>)>;

fn setup_one_geo(cfg: Cfg, input: Input) -> (Cfg, GeoLines) {
    let (cfg, cases) = setup_one(cfg, input);
    let cases = cases
        .iter()
        .map(|(p, t)| (MultiLineString(vec![line_string(p)]), t.clone()))
        .collect();
    (cfg, cases)
}

#[library_benchmark(setup = setup_one_geo)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
fn one_geo((cfg, cases): (Cfg, GeoLines)) -> GeoLines {
    for (line, tiles) in &cases {
        for &tile in tiles {
            let rect = baseline::tile_rect(tile, cfg.extent, cfg.buffer);
            black_box(rect.to_polygon().clip(black_box(line), false));
        }
    }
    cases
}

// ---- Polygons ----

/// Which polygon set a polygon benchmark runs over (loaded in `setup`).
#[derive(Clone, Copy)]
enum PolyInput {
    /// Every `tests/polygons/fixtures` feature (one polygon or multipolygon each).
    Small,
    /// The single [`support::big_polygon`].
    Big,
    /// The single [`support::huge_square`].
    Huge,
    /// The single [`support::many_holes`].
    Holes,
    /// [`support::buildings`], each building its own feature.
    Buildings,
    /// [`support::footprints`], each footprint its own feature.
    Footprints,
    /// The single [`support::diagonal_triangle`] across this many tiles per axis.
    Diagonal(i32),
}

/// Multipolygon features (each a list of polygons) for a [`PolyInput`].
fn load_polygons(input: PolyInput) -> Vec<Vec<FixturePolygon>> {
    match input {
        PolyInput::Small => {
            let dir =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/polygons/fixtures");
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .expect("polygon fixtures dir")
                .map(|e| e.expect("dir entry").path())
                .filter(|p| p.extension().is_some_and(|e| e == "geojson"))
                .collect();
            paths.sort();
            paths
                .iter()
                .flat_map(|p| support::load_polygon_features(p))
                .collect()
        }
        PolyInput::Big => vec![vec![support::big_polygon()]],
        PolyInput::Huge => vec![vec![support::huge_square()]],
        PolyInput::Holes => vec![vec![support::many_holes()]],
        PolyInput::Buildings => support::buildings().into_iter().map(|b| vec![b]).collect(),
        PolyInput::Footprints => support::footprints().into_iter().map(|b| vec![b]).collect(),
        PolyInput::Diagonal(tiles) => vec![vec![support::diagonal_triangle(tiles)]],
    }
}

/// A polygon's rings, exterior first, without collecting them. A named function rather than a closure
/// in the measured function: gungraun counts only inside functions named `*::__gungraun_wrapper_mod::*`
/// and toggles counting at each one, and a closure's type would put that path into the slicer's own
/// (generic) function name, turning counting off inside it.
fn polygon_rings(p: &FixturePolygon) -> impl Iterator<Item = &Vec<Coord<i32>>> {
    std::iter::once(&p.exterior).chain(&p.holes)
}

/// A multipolygon feature as the benchmarks slice it, polygon by polygon: each polygon its rings
/// (exterior first).
type PolyFeature = Vec<Vec<Vec<Coord<i32>>>>;

fn as_rings(feature: &[FixturePolygon]) -> PolyFeature {
    feature
        .iter()
        .map(|p| polygon_rings(p).cloned().collect())
        .collect()
}

/// The slices of one polygon of a [`PolyFeature`].
type PolySlices<'a> = map_tile_toolkit::PolygonSlices<'a, Coord<i32>, Vec<Coord<i32>>>;

/// Read every vertex of every slice, as a consumer encoding them would.
fn consume_polygons(slices: &mut PolySlices<'_>) {
    while let Some(slice) = slices.next() {
        match slice.expect("polygon") {
            map_tile_toolkit::PolygonSlice::Piece(piece) => {
                black_box(piece.tile());
                for ring in piece.rings() {
                    for v in ring {
                        black_box(v);
                    }
                }
            }
            map_tile_toolkit::PolygonSlice::Run(run) => {
                black_box(run);
            }
        }
    }
}

fn setup_polygon_all(cfg: Cfg, input: PolyInput) -> (Cfg, Vec<PolyFeature>) {
    (
        cfg,
        load_polygons(input).iter().map(|f| as_rings(f)).collect(),
    )
}

#[library_benchmark(setup = setup_polygon_all)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::footprints(support::slicer(4096, 64), PolyInput::Footprints)]
#[bench::footprints_edge(support::slicer(256, 8), PolyInput::Footprints)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
#[bench::huge_fill(support::slicer(4096, 64), PolyInput::Huge)]
#[bench::many_holes(support::slicer(256, 8), PolyInput::Holes)]
#[bench::diagonal_4k(support::slicer(4096, 64), PolyInput::Diagonal(4000))]
#[bench::diagonal_z14(support::slicer(4096, 64), PolyInput::Diagonal(1 << 14))]
fn polygon_all((cfg, features): (Cfg, Vec<PolyFeature>)) -> Vec<PolyFeature> {
    let mut slicer = cfg.slicer();
    for feature in &features {
        for polygon in feature {
            consume_polygons(&mut slicer.polygon(black_box(polygon)).expect("polygon"));
        }
    }
    features
}

/// Take each unclipped piece's tile and view (its rings are the whole input, in every tile).
fn consume_whole_polygons(slices: &mut PolySlices<'_>) {
    while let Some(slice) = slices.next() {
        black_box(slice.expect("polygon"));
    }
}

#[library_benchmark(setup = setup_polygon_all)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
#[bench::many_holes(support::slicer(256, 8), PolyInput::Holes)]
fn polygon_all_unclipped((cfg, features): (Cfg, Vec<PolyFeature>)) -> Vec<PolyFeature> {
    let mut slicer = unclipped(cfg);
    for feature in &features {
        for polygon in feature {
            consume_whole_polygons(&mut slicer.polygon(black_box(polygon)).expect("polygon"));
        }
    }
    features
}

/// Per feature: its polygons and the edge tiles the all-tiles slicer finds (precomputed in setup).
type PolyOneCases = Vec<(PolyFeature, Vec<TileId>)>;

/// The tiles [`Slicer::polygon`](map_tile_toolkit::Slicer::polygon) produces for `feature`, sorted
/// and distinct: its edge tiles, plus every tile of its fill runs if `fills`.
fn polygon_tiles(cfg: Cfg, feature: &[FixturePolygon], fills: bool) -> Vec<TileId> {
    let mut slicer = cfg.slicer();
    let mut tiles = std::collections::BTreeSet::new();
    for rings in as_rings(feature) {
        let mut slices = slicer.polygon(&rings).expect("polygon");
        while let Some(slice) = slices.next() {
            match slice.expect("polygon") {
                map_tile_toolkit::PolygonSlice::Piece(piece) => {
                    tiles.insert(piece.tile());
                }
                map_tile_toolkit::PolygonSlice::Run(run) if fills => {
                    tiles.extend(run.x.map(|x| TileId::new(x, run.y)));
                }
                map_tile_toolkit::PolygonSlice::Run(_) => {}
            }
        }
    }
    tiles.into_iter().collect()
}

fn setup_polygon_one(cfg: Cfg, input: PolyInput) -> (Cfg, PolyOneCases) {
    let cases = load_polygons(input)
        .into_iter()
        .map(|feature| {
            let tiles = polygon_tiles(cfg, &feature, false);
            (as_rings(&feature), tiles)
        })
        .collect();
    (cfg, cases)
}

#[library_benchmark(setup = setup_polygon_one)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::footprints(support::slicer(4096, 64), PolyInput::Footprints)]
#[bench::footprints_edge(support::slicer(256, 8), PolyInput::Footprints)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
fn polygon_one((cfg, cases): (Cfg, PolyOneCases)) -> PolyOneCases {
    let mut slicer = cfg.slicer();
    for (feature, tiles) in &cases {
        for &tile in tiles {
            for polygon in feature {
                let piece = slicer
                    .tile_polygon(tile, black_box(polygon))
                    .expect("polygon");
                for ring in piece.iter().flat_map(|p| p.rings()) {
                    for v in ring {
                        black_box(v);
                    }
                }
            }
        }
    }
    cases
}

#[library_benchmark(setup = setup_polygon_one)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
fn polygon_one_unclipped((cfg, cases): (Cfg, PolyOneCases)) -> PolyOneCases {
    let mut slicer = unclipped(cfg);
    for (feature, tiles) in &cases {
        for &tile in tiles {
            for polygon in feature {
                let piece = slicer
                    .tile_polygon(tile, black_box(polygon))
                    .expect("polygon");
                black_box(piece);
            }
        }
    }
    cases
}

// ---- Polygon baselines: the same features (and, for `one`, the same edge tiles). ----

/// Every ring of a feature's polygons, as `f64` points — the stripe clipper clips each on its own.
fn feature_rings(feature: &[FixturePolygon]) -> Vec<Vec<Pt>> {
    feature
        .iter()
        .flat_map(|p| std::iter::once(&p.exterior).chain(&p.holes))
        .map(|r| pts(r))
        .collect()
}

/// A feature as an `f64` `geo` multipolygon.
fn multi_polygon(feature: &[FixturePolygon]) -> MultiPolygon<f64> {
    let polygon = |p: &FixturePolygon| {
        Polygon::new(
            line_string(&p.exterior),
            p.holes.iter().map(|h| line_string(h)).collect(),
        )
    };
    MultiPolygon(feature.iter().map(polygon).collect())
}

/// Each feature's rings and its tiles: all of them (edge and fill) if `fills`, else the edge tiles.
fn polygon_stripe_cases(cfg: Cfg, input: PolyInput, fills: bool) -> (Cfg, StripeCases) {
    let cases = load_polygons(input)
        .iter()
        .map(|f| (feature_rings(f), sorted(&polygon_tiles(cfg, f, fills))))
        .collect();
    (cfg, cases)
}

fn setup_polygon_all_stripe(cfg: Cfg, input: PolyInput) -> (Cfg, StripeCases) {
    polygon_stripe_cases(cfg, input, true)
}

fn setup_polygon_one_stripe(cfg: Cfg, input: PolyInput) -> (Cfg, StripeCases) {
    polygon_stripe_cases(cfg, input, false)
}

#[library_benchmark(setup = setup_polygon_all_stripe)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
#[bench::many_holes(support::slicer(256, 8), PolyInput::Holes)]
fn polygon_all_stripe((cfg, cases): (Cfg, StripeCases)) -> StripeCases {
    stripe_all(cfg, &cases, true);
    cases
}

#[library_benchmark(setup = setup_polygon_one_stripe)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
fn polygon_one_stripe((cfg, cases): (Cfg, StripeCases)) -> StripeCases {
    stripe_one(cfg, &cases, true);
    cases
}

/// Per feature: its `geo` form and the tiles to intersect it with.
type GeoPolygons = Vec<(MultiPolygon<f64>, Vec<TileId>)>;

/// Each feature as a `geo` multipolygon and its tiles: edge and fill if `fills`, else edge only.
fn polygon_geo_cases(cfg: Cfg, input: PolyInput, fills: bool) -> (Cfg, GeoPolygons) {
    let cases = load_polygons(input)
        .iter()
        .map(|f| (multi_polygon(f), polygon_tiles(cfg, f, fills)))
        .collect();
    (cfg, cases)
}

fn setup_polygon_all_geo(cfg: Cfg, input: PolyInput) -> (Cfg, GeoPolygons) {
    polygon_geo_cases(cfg, input, true)
}

fn setup_polygon_one_geo(cfg: Cfg, input: PolyInput) -> (Cfg, GeoPolygons) {
    polygon_geo_cases(cfg, input, false)
}

/// `geo`'s overlay of each feature with each of its tiles' boxes.
fn geo_intersections(cfg: Cfg, cases: &GeoPolygons) {
    for (feature, tiles) in cases {
        for &tile in tiles {
            let rect = baseline::tile_rect(tile, cfg.extent, cfg.buffer);
            black_box(rect.to_polygon().intersection(black_box(feature)));
        }
    }
}

#[library_benchmark(setup = setup_polygon_all_geo)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
fn polygon_all_geo((cfg, cases): (Cfg, GeoPolygons)) -> GeoPolygons {
    geo_intersections(cfg, &cases);
    cases
}

#[library_benchmark(setup = setup_polygon_one_geo)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::buildings(support::slicer(4096, 64), PolyInput::Buildings)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
fn polygon_one_geo((cfg, cases): (Cfg, GeoPolygons)) -> GeoPolygons {
    geo_intersections(cfg, &cases);
    cases
}

// ---- Points: `point` (all tiles) and `tile_point` (every tile of the 4 × 4 grid), one point at a time. ----

/// Which point set a points benchmark slices (a multipoint feature, point by point; generated in `setup`).
#[derive(Clone, Copy)]
enum PointInput {
    /// 20 000 points scattered over the grid: a few percent near a tile edge.
    Scattered,
    /// 20 000 points all within the buffer bands along the tile edges: two to four tiles each.
    EdgeHeavy,
}

fn setup_points(cfg: Cfg, input: PointInput) -> (Cfg, Vec<Coord<i32>>) {
    let points = support::scattered_points(20_000, matches!(input, PointInput::EdgeHeavy));
    (cfg, points)
}

#[library_benchmark(setup = setup_points)]
#[bench::scattered(support::slicer(4096, 64), PointInput::Scattered)]
#[bench::edge_heavy(support::slicer(4096, 64), PointInput::EdgeHeavy)]
#[bench::scattered_unbuffered(support::slicer(4096, 0), PointInput::Scattered)]
fn points_all((cfg, points): (Cfg, Vec<Coord<i32>>)) -> Vec<Coord<i32>> {
    let slicer = cfg.slicer();
    for &point in black_box(&points) {
        for slice in slicer.point(point).expect("point") {
            black_box((slice.tile(), slice.point()));
        }
    }
    points
}

#[library_benchmark(setup = setup_points)]
#[bench::scattered(support::slicer(4096, 64), PointInput::Scattered)]
#[bench::edge_heavy(support::slicer(4096, 64), PointInput::EdgeHeavy)]
fn points_one((cfg, points): (Cfg, Vec<Coord<i32>>)) -> Vec<Coord<i32>> {
    let slicer = cfg.slicer();
    for ty in 0..4 {
        for tx in 0..4 {
            for &point in black_box(&points) {
                let local = slicer
                    .tile_point(TileId::new(tx, ty), point)
                    .expect("point");
                black_box(local);
            }
        }
    }
    points
}

library_benchmark_group!(
    name = slicing,
    benchmarks = [
        all,
        all_stripe,
        all_unclipped,
        one,
        one_stripe,
        one_geo,
        one_unclipped,
        polygon_all,
        polygon_all_stripe,
        polygon_all_geo,
        polygon_all_unclipped,
        polygon_one,
        polygon_one_stripe,
        polygon_one_geo,
        polygon_one_unclipped,
        points_all,
        points_one
    ]
);
main!(library_benchmark_groups = slicing);
