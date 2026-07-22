//! Minimal `slice_all` driver for `cargo flamegraph` — no criterion, no measurement machinery, so
//! the flamegraph shows only the slicing cost. Loads every `tests/fixtures/*.geojson` once, then
//! runs [`Slicer::slice_all`] over the whole set in a tight loop.
//!
//! Iteration count is the first CLI arg (default 2,000,000). Run via `just flamegraph-slice-all`.

#![allow(clippy::pedantic, reason = "profiling helper")]

use std::hint::black_box;

use geo_types::Geometry;
use map_tile_toolkit::Slicer;

#[path = "../tests/support/mod.rs"]
mod support;

const SLICER: Slicer = Slicer::new(25, 0).unwrap();

fn main() {
    let (iterations, geoms) = get_params();

    for _ in 0..iterations {
        for geom in &geoms {
            black_box(SLICER.slice_all(black_box(geom)));
        }
    }
}

fn get_params() -> (u64, Vec<Geometry<i32>>) {
    let iterations: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    let geoms: Vec<Geometry<i32>> = support::load_all_fixtures()
        .into_iter()
        .map(|(_, g)| g)
        .collect();
    (iterations, geoms)
}
