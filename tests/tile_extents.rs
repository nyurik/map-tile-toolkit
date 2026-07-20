//! Ported from planetiler `geo/TileExtentsTest.java`.
//!
//! Which tiles a world bounding box (and optional clip shape) covers, per zoom. `TileExtents`
//! is stubbed, so these are **red** until implemented.

#![allow(clippy::pedantic, reason = "ported test coordinates and literals")]

mod support;

use geo_types::{Coord, Rect};
use map_tile_toolkit::extents::{TileExtents, world_bounds};
use support::new_polygon;

const EPS: f64 = 1.0 / (1u64 << 30) as f64; // 2^-30

fn envelope(min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> Rect<f64> {
    Rect::new(Coord { x: min_x, y: min_y }, Coord { x: max_x, y: max_y })
}

#[test]
fn full_world() {
    let extents = TileExtents::compute_from_world_bounds(14, world_bounds());
    for z in 0..=14u8 {
        let max = 1i32 << z;
        let fz = extents.for_zoom(z);
        assert_eq!(fz.min_x, 0, "z{z} minX");
        assert_eq!(fz.max_x, max, "z{z} maxX");
        assert_eq!(fz.min_y, 0, "z{z} minY");
        assert_eq!(fz.max_y, max, "z{z} maxY");
    }
}

#[test]
fn top_left() {
    let extents = TileExtents::compute_from_world_bounds(14, envelope(0.0, EPS, 0.0, EPS));
    for z in 0..=14u8 {
        let fz = extents.for_zoom(z);
        assert_eq!(
            (fz.min_x, fz.max_x, fz.min_y, fz.max_y),
            (0, 1, 0, 1),
            "z{z}"
        );
    }
}

#[test]
fn top_right() {
    let extents = TileExtents::compute_from_world_bounds(14, envelope(1.0 - EPS, 1.0, 0.0, EPS));
    for z in 0..=14u8 {
        let max = 1i32 << z;
        let fz = extents.for_zoom(z);
        assert_eq!(
            (fz.min_x, fz.max_x, fz.min_y, fz.max_y),
            (max - 1, max, 0, 1),
            "z{z}"
        );
    }
}

#[test]
fn bottom_left() {
    let extents = TileExtents::compute_from_world_bounds(14, envelope(0.0, EPS, 1.0 - EPS, 1.0));
    for z in 0..=14u8 {
        let max = 1i32 << z;
        let fz = extents.for_zoom(z);
        assert_eq!(
            (fz.min_x, fz.max_x, fz.min_y, fz.max_y),
            (0, 1, max - 1, max),
            "z{z}"
        );
    }
}

#[test]
fn shape() {
    let s = 1.0 / f64::from(1i32 << 14); // 2^-14
    // planetiler feeds `worldToLatLonCoords(shape)`; the world-coord polygon is used directly
    // here since the shape-clip path is what's under test (and stubbed).
    let shape = new_polygon(&[
        0.5,
        0.5 - s * 5.0,
        0.5 + s * 5.0,
        0.5,
        0.5,
        0.5 + s * 5.0,
        0.5 - s * 5.0,
        0.5,
        0.5,
        0.5 - s * 5.0,
    ]);
    let extents = TileExtents::compute_from_world_bounds_with_shape(
        14,
        envelope(0.5 - s * 4.0, 0.5 + s * 4.0, 0.5 - s * 4.0, 0.5 + s * 4.0),
        &shape,
    );
    for z in 0..=14u8 {
        let middle = (1u32 << z) / 2;
        assert!(extents.test(middle, middle, z), "z{z}");
    }
    let half = 1u32 << 13;
    assert!(extents.test(half + 3, half, 14), "inside shape and bounds");
    assert!(
        !extents.test(half + 4, half, 14),
        "inside shape, outside bounds"
    );
    assert!(
        !extents.test(half + 3, half + 3, 14),
        "inside bounds, outside shape"
    );
}
