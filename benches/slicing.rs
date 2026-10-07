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
//! function, whose result is handed to the measured function. Two operations, each over the same input
//! scenarios:
//! * `all` — [`SlicerAll::add_feature`] on each polyline into one accumulator.
//! * `one` — a fresh [`SlicerOne`] per touched tile.
//!
//! Scenarios: `small` (the `tests/polylines/fixtures/*.geojson` set on the extent-25 grid) and the
//! single large [`support::big_polyline`] sliced into many / a few / a single tile (`big_multi`,
//! `big_few`, `big_single`).
//!
//! The polygon slicers mirror them: `polygon_all` adds each feature to one
//! [`PolygonSlicerAll`](map_tile_toolkit::PolygonSlicerAll) cleared between features (a bulk tile
//! generator's per-worker pattern), `polygon_one` uses a fresh
//! [`PolygonSlicerOne`](map_tile_toolkit::PolygonSlicerOne) per edge tile. Scenarios: `small` (every
//! `tests/polygons/fixtures` feature), [`support::big_polygon`] at the three `big_*` scales, and —
//! for `polygon_all` only — `huge_fill`, the z14-like [`support::huge_square`] whose 2^28 covered
//! tiles must cost nothing per tile.
//!
//! Filter with e.g. `just bench big`, `just bench big_single`, `just bench all`, `just bench polygon`.

#![allow(clippy::pedantic, reason = "benchmark harness")]
#![allow(
    unused_qualifications,
    reason = "gungraun's macro expansion re-emits the qualified paths from the #[bench(...)] args"
)]

use std::hint::black_box;

use geo_types::Coord;
use gungraun::{library_benchmark, library_benchmark_group, main};
use map_tile_toolkit::TileId;

#[path = "../tests/support/mod.rs"]
mod support;

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
    }
}

/// The tiles a polyline touches (precomputed via a throwaway [`SlicerAll`]).
fn touched_tiles(cfg: &Cfg, poly: &[Coord<i32>]) -> Vec<TileId> {
    let mut acc = cfg.all();
    acc.add_feature(poly).expect("polyline");
    acc.iter_tiles().map(|t| t.tile_id()).collect()
}

// ---- `all`: slice every polyline into all touched tiles, accumulated into one `SlicerAll`. ----

/// Prepare the `all` inputs: the slicer config and the polyline set (loading is not measured).
fn setup_all(cfg: Cfg, input: Input) -> (Cfg, support::Polylines) {
    (cfg, load(input))
}

#[library_benchmark(setup = setup_all)]
#[bench::small(support::grid(), Input::Small)]
#[bench::big_multi(support::slicer(25, 0), Input::Big)]
#[bench::big_few(support::slicer(300, 0), Input::Big)]
#[bench::big_single(support::slicer(1024, 0), Input::Big)]
fn all((cfg, polylines): (Cfg, support::Polylines)) {
    let mut acc = cfg.all();
    for poly in &polylines {
        acc.add_feature(black_box(poly)).expect("polyline");
    }
    black_box(acc);
}

// ---- `one`: slice each polyline into each touched tile with a fresh `SlicerOne`. ----

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
fn one((cfg, cases): (Cfg, OneCases)) {
    for (poly, tiles) in &cases {
        for &tile in tiles {
            let mut acc = cfg.one(tile);
            acc.add_feature(black_box(poly)).expect("polyline");
            black_box(&acc);
        }
    }
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
    }
}

fn setup_polygon_all(cfg: Cfg, input: PolyInput) -> (Cfg, Vec<Vec<FixturePolygon>>) {
    (cfg, load_polygons(input))
}

#[library_benchmark(setup = setup_polygon_all)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
#[bench::huge_fill(support::slicer(4096, 64), PolyInput::Huge)]
fn polygon_all((cfg, features): (Cfg, Vec<Vec<FixturePolygon>>)) {
    let mut acc = cfg.poly_all();
    for feature in &features {
        acc.clear();
        acc.add_feature(black_box(feature.iter().map(support::rings)))
            .expect("polygon");
        black_box(&acc);
    }
}

/// Per feature: its polygons and the edge tiles the all-tiles slicer finds (precomputed in setup).
type PolyOneCases = Vec<(Vec<FixturePolygon>, Vec<TileId>)>;

fn setup_polygon_one(cfg: Cfg, input: PolyInput) -> (Cfg, PolyOneCases) {
    let cases = load_polygons(input)
        .into_iter()
        .map(|feature| {
            let mut acc = cfg.poly_all();
            acc.add_feature(feature.iter().map(support::rings))
                .expect("polygon");
            let tiles = acc
                .iter_features()
                .flat_map(|f| f.iter_tiles().map(|t| t.tile_id()).collect::<Vec<_>>())
                .collect();
            (feature, tiles)
        })
        .collect();
    (cfg, cases)
}

#[library_benchmark(setup = setup_polygon_one)]
#[bench::small(support::grid(), PolyInput::Small)]
#[bench::big_multi(support::slicer(25, 0), PolyInput::Big)]
#[bench::big_few(support::slicer(300, 0), PolyInput::Big)]
#[bench::big_single(support::slicer(1024, 0), PolyInput::Big)]
fn polygon_one((cfg, cases): (Cfg, PolyOneCases)) {
    for (feature, tiles) in &cases {
        for &tile in tiles {
            let mut acc = cfg.poly_one(tile);
            for p in feature {
                let holes: Vec<&[Coord<i32>]> = p.holes.iter().map(Vec::as_slice).collect();
                acc.add_feature(black_box(&p.exterior), &holes)
                    .expect("polygon");
            }
            black_box(&acc);
        }
    }
}

library_benchmark_group!(
    name = slicing,
    benchmarks = [all, one, polygon_all, polygon_one]
);
main!(library_benchmark_groups = slicing);
