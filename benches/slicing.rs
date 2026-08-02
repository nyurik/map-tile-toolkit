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
//! Filter with e.g. `just bench big`, `just bench big_single`, `just bench all`.

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

use support::Cfg;

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

library_benchmark_group!(name = slicing, benchmarks = [all, one]);
main!(library_benchmark_groups = slicing);
