//! Geometry-clipping benchmarks, organized by geometry type.
//!
//! For each geometry type — polygon, polygon-with-holes, and polyline — two operations are
//! measured:
//!
//! * **one tile** ([`slice_tile`]): extract a single tile's clipped slice from the geometry
//!   (the tile-server operation).
//! * **all tiles**: slice the geometry into every tile it occupies, via both clipping paths —
//!   the per-tile [`slice_all_tiles`] and the eager [`stripe::TiledGeometry`] slicer.
//!
//! Run with `cargo bench`. All input is deterministic (no RNG) so runs are comparable.

#![allow(clippy::pedantic, reason = "benchmark harness code")]

use std::f64::consts::PI;
use std::hint::black_box;
use std::num::NonZeroU32;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use geo::MapCoords as _;
use geo_types::{Coord, Geometry, LineString, Polygon};
use map_tile_toolkit::extents::ForZoom;
use map_tile_toolkit::stripe::TiledGeometry;
use map_tile_toolkit::{SliceOptions, TileId, slice_all_tiles, slice_tile};

/// Earth circumference in meters (Web Mercator plane side length).
const CIRC: f64 = 40_075_016.685_578_5;
const ORIGIN: f64 = CIRC / 2.0;

/// Zoom used for the "all tiles" benchmarks (1024 tiles).
const ZOOM: u8 = 5;
const BUFFER_PX: u32 = 64;
const EXTENT: u32 = 4096;

fn opts() -> SliceOptions {
    SliceOptions::new(NonZeroU32::new(EXTENT).expect("nonzero"), BUFFER_PX)
}

fn buffer_fraction() -> f64 {
    f64::from(BUFFER_PX) / f64::from(EXTENT)
}

fn full_extents(z: u8) -> ForZoom {
    let n = 1i32 << z;
    ForZoom::new(z, 0, 0, n, n, None)
}

// --- synthetic geometries, in world-fraction coordinates (0..1 = whole world) -------------

/// A closed regular `n`-gon ring centered at `(cx, cy)` with radius `r`.
fn ring(cx: f64, cy: f64, r: f64, n: usize) -> LineString<f64> {
    let mut pts: Vec<Coord<f64>> = (0..n)
        .map(|k| {
            let theta = 2.0 * PI * (k as f64) / (n as f64);
            Coord {
                x: cx + r * theta.cos(),
                y: cy + r * theta.sin(),
            }
        })
        .collect();
    pts.push(pts[0]);
    LineString(pts)
}

/// A 64-gon covering much of the world.
fn polygon_world() -> Geometry<f64> {
    Geometry::Polygon(Polygon::new(ring(0.5, 0.5, 0.4, 64), vec![]))
}

/// The same outer ring with two interior holes.
fn polygon_with_holes_world() -> Geometry<f64> {
    Geometry::Polygon(Polygon::new(
        ring(0.5, 0.5, 0.4, 64),
        vec![ring(0.35, 0.5, 0.08, 32), ring(0.65, 0.5, 0.08, 32)],
    ))
}

/// A sinusoidal polyline that passes through the world center and spans a band of tiles.
fn polyline_world() -> Geometry<f64> {
    let n = 65; // odd, so the midpoint vertex lands exactly on (0.5, 0.5)
    let pts: Vec<Coord<f64>> = (0..n)
        .map(|k| {
            let t = (k as f64) / ((n - 1) as f64);
            Coord {
                x: 0.1 + 0.8 * t,
                y: 0.5 + 0.35 * (2.0 * PI * 2.0 * t).sin(),
            }
        })
        .collect();
    Geometry::LineString(LineString(pts))
}

/// World-fraction → Web Mercator (input to `slice_tile` / `slice_all_tiles`).
fn to_mercator(geom: &Geometry<f64>) -> Geometry<f64> {
    geom.map_coords(|c| Coord {
        x: -ORIGIN + c.x * CIRC,
        y: ORIGIN - c.y * CIRC,
    })
}

/// World-fraction → `2^zoom` tile units (input to the stripe slicer).
fn to_tile_units(geom: &Geometry<f64>, zoom: u8) -> Geometry<f64> {
    let scale = f64::from(1u32 << zoom);
    geom.map_coords(|c| Coord {
        x: c.x * scale,
        y: c.y * scale,
    })
}

fn geometries() -> [(&'static str, Geometry<f64>); 3] {
    [
        ("polygon", polygon_world()),
        ("polygon_with_holes", polygon_with_holes_world()),
        ("polyline", polyline_world()),
    ]
}

// --- benchmarks -----------------------------------------------------------

/// Extract a single tile's slice from each geometry type.
fn bench_clip_one_tile(c: &mut Criterion) {
    let mut group = c.benchmark_group("clip_one_tile");
    let opts = opts();
    let tile = TileId::new(1 << (ZOOM - 1), 1 << (ZOOM - 1), ZOOM); // center tile (16, 16, 5)
    for (name, geom) in geometries() {
        let merc = to_mercator(&geom);
        group.bench_function(name, |b| {
            b.iter(|| black_box(slice_tile(&merc, tile, opts)))
        });
    }
    group.finish();
}

/// Slice each geometry type into every tile it occupies, via both clipping paths.
fn bench_slice_all_tiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("slice_all_tiles");
    // The per-tile path runs several ms/iter, too slow for criterion's default 100 samples.
    group.sample_size(30);
    let opts = opts();
    let buffer = buffer_fraction();
    let extents = full_extents(ZOOM);
    for (name, geom) in geometries() {
        let merc = to_mercator(&geom);
        let tile_units = to_tile_units(&geom, ZOOM);

        group.bench_with_input(BenchmarkId::new(name, "per_tile"), &merc, |b, g| {
            b.iter(|| black_box(slice_all_tiles(g, ZOOM, opts).count()));
        });
        group.bench_with_input(BenchmarkId::new(name, "stripe"), &tile_units, |b, g| {
            b.iter(|| {
                let sliced =
                    TiledGeometry::slice_geometry(g, 0.0, buffer, ZOOM, &extents).expect("slice");
                black_box(sliced.tile_data().len() + sliced.filled_tiles().count())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_clip_one_tile, bench_slice_all_tiles);
criterion_main!(benches);
