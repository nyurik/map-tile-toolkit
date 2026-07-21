//! Profiling workload for `just flamegraph`.
//!
//! Runs one geometry type through one clipping operation in a tight loop, with no benchmark
//! harness, so a flamegraph of this binary shows only the clipping code (not criterion or
//! `cargo metadata` startup noise).
//!
//! Usage: `profile_clip <kind> <op> <secs>`
//! * `kind`: `polygon` | `polygon_with_holes` | `polyline`
//! * `op`: `stripe` / `per_tile` (both slice into all tiles) or `one_tile` (extract one tile)
//! * `secs`: how long to loop (default 10)

#![allow(clippy::pedantic, reason = "profiling harness")]

use std::env;
use std::f64::consts::PI;
use std::hint::black_box;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use geo::MapCoords as _;
use geo_types::{Coord, Geometry, LineString, Polygon, coord};
use map_tile_toolkit::extents::ForZoom;
use map_tile_toolkit::stripe::TiledGeometry;
use map_tile_toolkit::{SliceOptions, TileId, slice_all_tiles, slice_tile};

const CIRC: f64 = 40_075_016.685_578_5;
const ORIGIN: f64 = CIRC / 2.0;
const ZOOM: u8 = 5;
const EXTENT: u32 = 4096;
const BUFFER_PX: u32 = 64;

fn ring(cx: f64, cy: f64, r: f64, n: usize) -> LineString<f64> {
    let mut pts: Vec<Coord<f64>> = (0..n)
        .map(|k| {
            let theta = 2.0 * PI * (k as f64) / (n as f64);
            coord! { x: cx + r * theta.cos(), y: cy + r * theta.sin() }
        })
        .collect();
    pts.push(pts[0]);
    LineString(pts)
}

/// The synthetic geometry for `kind`, in world-fraction coordinates (`0..1` = whole world).
fn geometry(kind: &str) -> Geometry<f64> {
    match kind {
        "polygon" => Geometry::Polygon(Polygon::new(ring(0.5, 0.5, 0.4, 64), vec![])),
        "polygon_with_holes" => Geometry::Polygon(Polygon::new(
            ring(0.5, 0.5, 0.4, 64),
            vec![ring(0.35, 0.5, 0.08, 32), ring(0.65, 0.5, 0.08, 32)],
        )),
        "polyline" => {
            let n = 65;
            let pts = (0..n)
                .map(|k| {
                    let t = (k as f64) / f64::from(n - 1);
                    Coord {
                        x: 0.1 + 0.8 * t,
                        y: 0.5 + 0.35 * (2.0 * PI * 2.0 * t).sin(),
                    }
                })
                .collect();
            Geometry::LineString(LineString(pts))
        }
        other => panic!("unknown kind: {other}"),
    }
}

fn to_mercator(g: &Geometry<f64>) -> Geometry<f64> {
    g.map_coords(|c| coord! { x: -ORIGIN + c.x * CIRC, y: ORIGIN - c.y * CIRC })
}

fn to_tile_units(g: &Geometry<f64>) -> Geometry<f64> {
    let scale = f64::from(1u32 << ZOOM);
    g.map_coords(|c| coord! { x: c.x * scale, y: c.y * scale })
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let kind = args.get(1).map_or("polygon", String::as_str);
    let op = args.get(2).map_or("stripe", String::as_str);
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);

    let world = geometry(kind);
    let opts = SliceOptions::new(NonZeroU32::new(EXTENT).expect("nonzero"), BUFFER_PX);
    let extents = ForZoom::new(ZOOM, 0, 0, 1 << ZOOM, 1 << ZOOM, None);
    let buffer = f64::from(BUFFER_PX) / f64::from(EXTENT);
    let merc = to_mercator(&world);
    let tile_units = to_tile_units(&world);
    let center = TileId::new(1 << (ZOOM - 1), 1 << (ZOOM - 1), ZOOM);

    let deadline = Duration::from_secs(secs);
    let start = Instant::now();
    let mut acc = 0usize;
    let mut iterations = 0u64;
    while start.elapsed() < deadline {
        acc += match op {
            "stripe" => {
                let sliced = TiledGeometry::slice_geometry(
                    black_box(&tile_units),
                    0.0,
                    buffer,
                    ZOOM,
                    &extents,
                )
                .expect("slice");
                sliced.tile_data().len() + sliced.filled_tiles().count()
            }
            "per_tile" => slice_all_tiles(black_box(&merc), ZOOM, opts).count(),
            "one_tile" => usize::from(slice_tile(black_box(&merc), center, opts).is_some()),
            other => panic!("unknown op: {other}"),
        };
        iterations += 1;
    }
    // Keep the work observable so nothing is optimized away.
    black_box(acc);
    eprintln!("{kind}/{op}: {iterations} iterations in {secs}s");
}
