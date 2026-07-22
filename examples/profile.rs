//! Minimal slicing driver for `cargo flamegraph` — no criterion, no measurement machinery, so the
//! flamegraph shows only the slicing cost. Selects one case by name and runs it in a tight loop.
//!
//! Usage: `cargo run --release --example profile -- <name> [iterations]`, or `just flamegraph <name>
//! [iterations]`.
//!
//! Cases (`<name>`):
//! * `all` / `one` — slice the small fixture set into all tiles (one [`SlicerAll`]) / per touched
//!   tile (a fresh [`SlicerOne`] each).
//! * `big-all-<cfg>` / `big-one-<cfg>` — the large in-memory `big_polyline` sliced with the
//!   [`support::big_configs`] `<cfg>` (`multi`, `few`, or `single` — many / a few / one output tile).
//!
//! The second parameter is the iteration count (defaults chosen per scale). Tile ids for the `one`
//! cases are precomputed up front, so the loop measures only clipping/accumulation.

#![allow(clippy::pedantic, reason = "profiling helper")]

use std::hint::black_box;

use geo_types::Coord;
use map_tile_toolkit::TileId;

#[path = "../tests/support/mod.rs"]
mod support;

use support::Cfg;

/// Which operation to profile.
enum Op {
    All,
    One,
}

/// A polyline paired with the tile ids it touches (precomputed, not part of the hot loop).
type Case = (Vec<Coord<i32>>, Vec<TileId>);

/// The tiles a polyline touches (precomputed via a throwaway [`SlicerAll`]).
fn touched_tiles(cfg: &Cfg, poly: &[Coord<i32>]) -> Vec<TileId> {
    let mut acc = cfg.all();
    acc.add_feature(poly).expect("polyline");
    acc.iter_tiles().map(|t| t.id()).collect()
}

fn main() {
    let name = std::env::args()
        .nth(1)
        .expect("usage: profile <name> [iterations]");
    let (polylines, cfg, op, default_iters) = resolve(&name);
    let iterations: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_iters);

    // Precompute each polyline's tile ids once, outside the timed loop.
    let cases: Vec<Case> = polylines
        .into_iter()
        .map(|poly| {
            let tiles = touched_tiles(&cfg, &poly);
            (poly, tiles)
        })
        .collect();

    for _ in 0..iterations {
        for (poly, tiles) in &cases {
            match op {
                Op::All => {
                    let mut acc = cfg.all();
                    acc.add_feature(black_box(poly)).expect("polyline");
                    black_box(&acc);
                }
                Op::One => {
                    for &tile in tiles {
                        let mut acc = cfg.one(tile);
                        acc.add_feature(black_box(poly)).expect("polyline");
                        black_box(&acc);
                    }
                }
            }
        }
    }
}

/// The component polylines of a geometry, owned.
fn polylines_of(geom: &geo_types::Geometry<i32>) -> Vec<Vec<Coord<i32>>> {
    support::lines_of(geom)
        .into_iter()
        .map(<[_]>::to_vec)
        .collect()
}

/// Resolve a case name into `(polylines, config, operation, default iterations)`.
fn resolve(name: &str) -> (Vec<Vec<Coord<i32>>>, Cfg, Op, u64) {
    let small = || {
        support::load_all_fixtures()
            .into_iter()
            .flat_map(|(_, g)| polylines_of(&g))
            .collect()
    };
    match name {
        "all" => (small(), support::grid(), Op::All, 2_000_000),
        "one" => (small(), support::grid(), Op::One, 2_000_000),
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
            let slicer = support::big_configs()
                .into_iter()
                .find(|(c, _)| *c == cfg)
                .map(|(_, s)| s)
                .unwrap_or_else(|| panic!("unknown big config `{cfg}` in: {name}"));
            (polylines_of(&support::big_polyline()), slicer, op, 3_000)
        }
    }
}
