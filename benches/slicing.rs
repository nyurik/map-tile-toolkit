//! Slicing benchmarks. Each is measured **all together** where it loops (one timed run covers the
//! whole input set, not one benchmark per file). Fixture loading/parsing and the slicer configs are
//! shared with the tests/example via `tests/support/mod.rs`.
//!
//! Small-fixture set (`tests/fixtures/*.geojson`, divider 25), each fixture flattened to its
//! component polylines:
//! * `all` — [`SlicerAll::add_feature`] on each polyline into one accumulator.
//! * `one` — a fresh [`SlicerOne`] per touched tile (tile ids computed up front, outside the timed
//!   loop, so only the clipping/accumulation is measured).
//!
//! Large in-memory polyline (`support::big_polyline`, ~3.6k vertices), benchmarked independently at
//! each [`support::big_configs`] slicer so the same polyline is sliced into many / a few / a single
//! tile — `big-{all,one}-{multi,few,single}`.
//!
//! Filter with e.g. `just bench big`, `just bench single`, `just bench big-all`.

#![allow(clippy::pedantic, reason = "benchmark harness")]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use geo_types::Coord;
use map_tile_toolkit::TileId;

#[path = "../tests/support/mod.rs"]
mod support;

use support::Cfg;

/// The tiles a polyline touches (precomputed via a throwaway [`SlicerAll`], not part of a hot loop).
fn touched_tiles(cfg: &Cfg, poly: &[Coord<i32>]) -> Vec<TileId> {
    let mut acc = cfg.all();
    acc.add_feature(poly).expect("polyline");
    acc.iter_tiles().map(|t| t.id()).collect()
}

/// Time slicing every polyline into all touched tiles, accumulated into one `SlicerAll`.
fn bench_all(c: &mut Criterion, id: &str, cfg: &Cfg, polylines: &[Vec<Coord<i32>>]) {
    c.bench_function(id, |b| {
        b.iter(|| {
            let mut acc = cfg.all();
            for poly in polylines {
                acc.add_feature(black_box(poly)).expect("polyline");
            }
            black_box(&acc);
        });
    });
}

/// Time slicing each polyline into each touched tile with a fresh `SlicerOne`, with the tile ids
/// precomputed outside the timed loop.
fn bench_one(c: &mut Criterion, id: &str, cfg: &Cfg, polylines: &[Vec<Coord<i32>>]) {
    let cases: Vec<(&Vec<Coord<i32>>, Vec<TileId>)> = polylines
        .iter()
        .map(|poly| (poly, touched_tiles(cfg, poly)))
        .collect();
    c.bench_function(id, |b| {
        b.iter(|| {
            for (poly, tiles) in &cases {
                for &tile in tiles {
                    let mut acc = cfg.one(tile);
                    acc.add_feature(black_box(poly)).expect("polyline");
                    black_box(&acc);
                }
            }
        });
    });
}

/// The component polylines of a geometry, owned.
fn polylines_of(geom: &geo_types::Geometry<i32>) -> Vec<Vec<Coord<i32>>> {
    support::lines_of(geom)
        .into_iter()
        .map(<[_]>::to_vec)
        .collect()
}

fn benches(c: &mut Criterion) {
    // Small fixtures, flattened to polylines, sliced on the test grid.
    let small: Vec<Vec<Coord<i32>>> = support::load_all_fixtures()
        .into_iter()
        .flat_map(|(_, g)| polylines_of(&g))
        .collect();
    bench_all(c, "all", &support::grid(), &small);
    bench_one(c, "one", &support::grid(), &small);

    // One large polyline, sliced into many / a few / a single tile.
    let big = polylines_of(&support::big_polyline());
    for (cfg, slicer) in support::big_configs() {
        bench_all(c, &format!("big-all-{cfg}"), &slicer, &big);
        bench_one(c, &format!("big-one-{cfg}"), &slicer, &big);
    }
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
