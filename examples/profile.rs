//! Minimal slicing driver for `cargo flamegraph` — no criterion, no measurement machinery, so the
//! flamegraph shows only the slicing cost. Selects one case by name and runs it in a tight loop.
//!
//! Usage: `cargo run --release --example profile -- <name> [iterations]`, or `just flamegraph <name>
//! [iterations]`.
//!
//! Cases (`<name>`):
//! * `all` / `one` — [`Slicer::slice_all`] / per-tile [`Slicer::slice`] over the small fixture set.
//! * `big-all-<cfg>` / `big-one-<cfg>` — the large in-memory `big_polyline` sliced with the
//!   [`support::BIG_CONFIGS`] `<cfg>` (`multi`, `few`, or `single` — many / a few / one output tile).
//!
//! The second parameter is the iteration count (defaults chosen per scale). Tile ids for the `one`
//! cases are precomputed up front, so the loop measures only clipping.

#![allow(clippy::pedantic, reason = "profiling helper")]

use std::hint::black_box;

use geo_types::Geometry;
use map_tile_toolkit::{Slicer, TileId};

#[path = "../tests/support/mod.rs"]
mod support;

/// Which operation to profile.
enum Op {
    All,
    One,
}

/// A geometry paired with the tile ids it touches (precomputed, not part of the hot loop).
type Case = (Geometry<i32>, Vec<TileId>);

fn main() {
    let name = std::env::args()
        .nth(1)
        .expect("usage: profile <name> [iterations]");
    let (geoms, slicer, op, default_iters) = resolve(&name);
    let iterations: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_iters);

    // Precompute each geometry's tile ids once, outside the timed loop.
    let cases: Vec<Case> = geoms
        .into_iter()
        .map(|geom| {
            let tiles = slicer
                .slice_all(&geom)
                .into_iter()
                .map(|(t, _)| t)
                .collect();
            (geom, tiles)
        })
        .collect();

    for _ in 0..iterations {
        for (geom, tiles) in &cases {
            match op {
                Op::All => {
                    black_box(slicer.slice_all(black_box(geom)));
                }
                Op::One => {
                    for &tile in tiles {
                        black_box(slicer.slice(black_box(geom), tile));
                    }
                }
            }
        }
    }
}

/// Resolve a case name into `(geometries, slicer, operation, default iterations)`.
fn resolve(name: &str) -> (Vec<Geometry<i32>>, Slicer, Op, u64) {
    let small = || {
        support::load_all_fixtures()
            .into_iter()
            .map(|(_, g)| g)
            .collect()
    };
    match name {
        "all" => (small(), support::SLICER, Op::All, 2_000_000),
        "one" => (small(), support::SLICER, Op::One, 2_000_000),
        _ => {
            let rest = name
                .strip_prefix("big-")
                .unwrap_or_else(|| panic!("unknown case name: {name}"));
            let (op_str, cfg) = rest
                .split_once('-')
                .unwrap_or_else(|| panic!("unknown case name: {name}"));
            let op = match op_str {
                "all" => Op::All,
                "one" => Op::One,
                _ => panic!("unknown case name: {name}"),
            };
            let slicer = support::BIG_CONFIGS
                .iter()
                .find(|(c, _)| *c == cfg)
                .map(|(_, s)| *s)
                .unwrap_or_else(|| panic!("unknown big config `{cfg}` in: {name}"));
            (vec![support::big_polyline()], slicer, op, 3_000)
        }
    }
}
