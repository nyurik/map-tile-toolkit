//! Slicing benchmarks. Each is measured **all together** where it loops (one timed run covers the
//! whole input set, not one benchmark per file). Fixture loading/parsing and the slicer configs are
//! shared with the tests/example via `tests/support/mod.rs`.
//!
//! Small-fixture set (`tests/fixtures/*.geojson`, divider 25):
//! * `all` — [`Slicer::slice_all`] on each fixture geometry.
//! * `one` — [`Slicer::slice`] once per touched tile (tile ids computed up front, outside the
//!   timed loop, so only the clipping is measured).
//!
//! Large in-memory polyline (`support::big_polyline`, ~3.6k vertices), benchmarked independently at
//! each [`support::BIG_CONFIGS`] slicer so the same geometry is sliced into many / a few / a single
//! tile — `big-{all,one}-{multi,few,single}`. `all` uses [`Slicer::slice_all`]; `one` uses
//! [`Slicer::slice`] per touched tile (ids precomputed outside the timed loop).
//!
//! Filter with e.g. `just bench big`, `just bench single`, `just bench big-all`.

#![allow(clippy::pedantic, reason = "benchmark harness")]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use geo_types::Geometry;
use map_tile_toolkit::{Slicer, TileId};

#[path = "../tests/support/mod.rs"]
mod support;

/// Time `slicer.slice_all` over every geometry in one run.
fn bench_all(c: &mut Criterion, id: &str, slicer: Slicer, geoms: &[Geometry<i32>]) {
    c.bench_function(id, |b| {
        b.iter(|| {
            for geom in geoms {
                black_box(slicer.slice_all(black_box(geom)));
            }
        });
    });
}

/// Time `slicer.slice` per touched tile, with the tile ids precomputed outside the timed loop.
fn bench_one(c: &mut Criterion, id: &str, slicer: Slicer, geoms: &[Geometry<i32>]) {
    let cases: Vec<(&Geometry<i32>, Vec<TileId>)> = geoms
        .iter()
        .map(|geom| {
            let tiles = slicer.slice_all(geom).into_iter().map(|(t, _)| t).collect();
            (geom, tiles)
        })
        .collect();
    c.bench_function(id, |b| {
        b.iter(|| {
            for (geom, tiles) in &cases {
                for &tile in tiles {
                    black_box(slicer.slice(black_box(geom), tile));
                }
            }
        });
    });
}

fn benches(c: &mut Criterion) {
    // Small fixtures, sliced on the test grid.
    let small: Vec<Geometry<i32>> = support::load_all_fixtures()
        .into_iter()
        .map(|(_, g)| g)
        .collect();
    bench_all(c, "all", support::SLICER, &small);
    bench_one(c, "one", support::SLICER, &small);

    // One large polyline, sliced into many / a few / a single tile.
    let big = [support::big_polyline()];
    for (cfg, slicer) in support::BIG_CONFIGS {
        bench_all(c, &format!("big-all-{cfg}"), slicer, &big);
        bench_one(c, &format!("big-one-{cfg}"), slicer, &big);
    }
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
