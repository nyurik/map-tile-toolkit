//! Minimal per-tile `slice` driver for `cargo flamegraph` — no criterion, no measurement
//! machinery, so the flamegraph shows only the slicing cost. Loads every `tests/fixtures/*.geojson`
//! once and precomputes each geometry's tile ids up front (not part of the hot loop), then runs
//! [`Slicer::slice`] per tile in a tight loop.
//!
//! Iteration count is the first CLI arg (default 4,000,000). Run via `just flamegraph-slice`.

#![allow(clippy::pedantic, reason = "profiling helper")]

use std::hint::black_box;

use geo_types::Geometry;
use map_tile_toolkit::{Slicer, TileId};

#[path = "../tests/support/mod.rs"]
mod support;

const SLICER: Slicer = Slicer {
    divider: 25,
    buffer: 0,
};

fn main() {
    let iterations = get_iterations();
    let cases = get_params();

    for _ in 0..iterations {
        for (geom, tiles) in &cases {
            for &tile in tiles {
                black_box(SLICER.slice(black_box(geom), tile));
            }
        }
    }
}

fn get_iterations() -> u64 {
    let iterations: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4_000_000);
    iterations
}

fn get_params() -> Vec<(Geometry<i32>, Vec<TileId>)> {
    support::load_all_fixtures()
        .into_iter()
        .map(|(_, geom)| {
            let tiles = SLICER
                .slice_all(&geom)
                .into_iter()
                .map(|(t, _)| t)
                .collect();
            (geom, tiles)
        })
        .collect()
}
