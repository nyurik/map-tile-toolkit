//! Benchmarks for the geometry-clipping paths.
//!
//! Each clipper is run over synthetic geometry of increasing complexity, reporting
//! throughput, to compare the two clipping strategies:
//!
//! * **per-tile** (`slice_tile` / `slice_all_tiles`): clip to each tile with `geo`'s overlay
//!   engine, `O(tiles × geometry)`.
//! * **stripe** (`stripe::TiledGeometry`): the eager slicer, roughly `O(geometry)` for a whole
//!   zoom level, with interior fill detection.
//!
//! Run with `cargo bench`. All input is deterministic (no RNG) so runs are comparable.

#![allow(clippy::pedantic, reason = "benchmark harness code")]

use std::f64::consts::PI;
use std::hint::black_box;
use std::num::NonZeroU32;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use geo_types::{Coord, Geometry, LineString, Polygon};
use map_tile_toolkit::extents::ForZoom;
use map_tile_toolkit::stripe::TiledGeometry;
use map_tile_toolkit::{SliceOptions, TileId, slice_all_tiles, slice_tile};

/// Earth circumference in meters (Web Mercator plane side length).
const CIRC: f64 = 40_075_016.685_578_5;
const ORIGIN: f64 = CIRC / 2.0;

const VERTEX_COUNTS: [usize; 3] = [16, 64, 256];
/// Zoom for the head-to-head per-tile-vs-stripe comparison (256 tiles — keeps the per-tile path tractable).
const ZOOM: u8 = 4;
const BUFFER_PX: u32 = 64;
const EXTENT: u32 = 4096;

fn opts() -> SliceOptions {
    SliceOptions::new(NonZeroU32::new(EXTENT).expect("nonzero"), BUFFER_PX)
}

fn buffer_fraction() -> f64 {
    f64::from(BUFFER_PX) / f64::from(EXTENT)
}

fn for_zoom(z: u8) -> ForZoom {
    let n = 1i32 << z;
    ForZoom::new(z, 0, 0, n, n, None)
}

/// A closed regular `n`-gon ring in world-fraction coordinates (`0..1` = whole world),
/// centered at `(0.5, 0.5)` with the given radius.
fn ngon_world(n: usize, radius: f64) -> Vec<(f64, f64)> {
    let mut ring: Vec<(f64, f64)> = (0..n)
        .map(|k| {
            let theta = 2.0 * PI * (k as f64) / (n as f64);
            (0.5 + radius * theta.cos(), 0.5 + radius * theta.sin())
        })
        .collect();
    ring.push(ring[0]);
    ring
}

/// World-fraction ring → a Web Mercator polygon (input to the per-tile path).
fn mercator_polygon(ring: &[(f64, f64)]) -> Geometry<f64> {
    let coords = ring
        .iter()
        .map(|&(wx, wy)| Coord { x: -ORIGIN + wx * CIRC, y: ORIGIN - wy * CIRC })
        .collect::<Vec<_>>();
    Geometry::Polygon(Polygon::new(LineString(coords), vec![]))
}

/// World-fraction ring → a polygon in `2^zoom` tile units (input to the stripe slicer).
fn tile_polygon(ring: &[(f64, f64)], zoom: u8) -> Geometry<f64> {
    let scale = f64::from(1u32 << zoom);
    let coords = ring
        .iter()
        .map(|&(wx, wy)| Coord { x: wx * scale, y: wy * scale })
        .collect::<Vec<_>>();
    Geometry::Polygon(Polygon::new(LineString(coords), vec![]))
}

/// Per-tile vs stripe, clipping the same N-gon into every tile it touches at [`ZOOM`].
fn bench_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_clip_all_tiles_z4");
    // The per-tile path runs ~1-2 ms/iter, too slow for criterion's default 100 samples in 5s.
    group.sample_size(50);
    let opts = opts();
    let buffer = buffer_fraction();
    let extents = for_zoom(ZOOM);
    for &n in &VERTEX_COUNTS {
        let ring = ngon_world(n, 0.45);
        let merc = mercator_polygon(&ring);
        let tile = tile_polygon(&ring, ZOOM);

        group.bench_with_input(BenchmarkId::new("per_tile", n), &merc, |b, g| {
            b.iter(|| black_box(slice_all_tiles(g, ZOOM, opts).count()));
        });
        group.bench_with_input(BenchmarkId::new("stripe", n), &tile, |b, g| {
            b.iter(|| {
                let sliced = TiledGeometry::slice_geometry(g, 0.0, buffer, ZOOM, &extents)
                    .expect("slice");
                black_box(sliced.tile_data().len())
            });
        });
    }
    group.finish();
}

/// Single-tile clip (the tile-server path): clip an N-gon to one tile it overlaps.
fn bench_single_tile(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_tile_clip_z4");
    let opts = opts();
    let tile = TileId::new(8, 8, ZOOM); // near the polygon center at z4
    for &n in &VERTEX_COUNTS {
        let merc = mercator_polygon(&ngon_world(n, 0.45));
        group.bench_with_input(BenchmarkId::new("slice_tile", n), &merc, |b, g| {
            b.iter(|| black_box(slice_tile(g, tile, opts)));
        });
    }
    group.finish();
}

/// Stripe fill detection: a near-world square at z8 fills the whole 256×256 grid. (The
/// per-tile path is omitted here — 65 536 per-tile overlays would dominate the whole run.)
fn bench_stripe_fill(c: &mut Criterion) {
    let square = vec![(0.02, 0.02), (0.98, 0.02), (0.98, 0.98), (0.02, 0.98), (0.02, 0.02)];
    let geom = tile_polygon(&square, 8);
    let extents = for_zoom(8);
    let buffer = buffer_fraction();
    c.bench_function("stripe_fill_z8", |b| {
        b.iter(|| {
            let sliced =
                TiledGeometry::slice_geometry(&geom, 0.0, buffer, 8, &extents).expect("slice");
            black_box(sliced.filled_tiles().count() + sliced.tile_data().len())
        });
    });
}

criterion_group!(benches, bench_batch, bench_single_tile, bench_stripe_fill);
criterion_main!(benches);
