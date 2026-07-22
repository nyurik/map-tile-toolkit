//! Slicing benchmarks over every `tests/fixtures/*.geojson` input, measured **all together** (one
//! timed run covers the whole fixture set, not one benchmark per file):
//!
//! * `slice-all` — [`Slicer::slice_all`] on each fixture geometry.
//! * `slice` — [`Slicer::slice`] once per tile each geometry touches. The tile ids are computed up
//!   front (outside the timed loop) so only the clipping is measured.
//!
//! Fixture loading/parsing is shared with the tests via `tests/support/mod.rs`.

#![allow(clippy::pedantic, reason = "benchmark harness")]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use geo_types::Geometry;
use map_tile_toolkit::{Slicer, TileId};

#[path = "../tests/support/mod.rs"]
mod support;

const SLICER: Slicer = Slicer::new(25, 0).unwrap();

fn bench_slice_all(c: &mut Criterion) {
    let geoms: Vec<Geometry<i32>> = support::load_all_fixtures()
        .into_iter()
        .map(|(_, g)| g)
        .collect();
    c.bench_function("slice-all", |b| {
        b.iter(|| {
            for geom in &geoms {
                black_box(SLICER.slice_all(black_box(geom)));
            }
        });
    });
}

fn bench_slice(c: &mut Criterion) {
    // Precompute each geometry's tile ids here, outside the timed loop, so `slice` alone is timed.
    let cases: Vec<(Geometry<i32>, Vec<TileId>)> = support::load_all_fixtures()
        .into_iter()
        .map(|(_, geom)| {
            let tiles = SLICER
                .slice_all(&geom)
                .into_iter()
                .map(|(t, _)| t)
                .collect();
            (geom, tiles)
        })
        .collect();
    c.bench_function("slice", |b| {
        b.iter(|| {
            for (geom, tiles) in &cases {
                for &tile in tiles {
                    black_box(SLICER.slice(black_box(geom), tile));
                }
            }
        });
    });
}

criterion_group!(benches, bench_slice_all, bench_slice);
criterion_main!(benches);
