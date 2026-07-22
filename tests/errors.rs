//! The public API never panics: invalid input yields a typed [`Error`] instead.

use geo_types::{Coord, Geometry, LineString, Point};
use map_tile_toolkit::{Error, Slicer, TileId};

fn slicer(divider: u32, buffer: u16) -> Slicer {
    Slicer::new(divider, buffer).expect("valid config")
}

fn line(coords: Vec<(i32, i32)>) -> Geometry<i32> {
    Geometry::LineString(LineString::from(coords))
}

#[test]
fn invalid_divider() {
    assert_eq!(Slicer::new(0, 0), Err(Error::InvalidDivider));
    assert_eq!(Slicer::new(u32::MAX, 0), Err(Error::InvalidDivider));
    assert!(Slicer::new(1, 0).is_ok());
    assert!(Slicer::new(i32::MAX as u32, u16::MAX).is_ok());
}

#[test]
fn non_polyline_geometry_errors() {
    let s = slicer(25, 0);
    let point = Geometry::Point(Point::new(1, 2));
    assert_eq!(
        s.slice(&point, TileId::new(0, 0)),
        Err(Error::UnsupportedGeometry("Point"))
    );
    assert_eq!(
        s.slice_all(&point),
        Err(Error::UnsupportedGeometry("Point"))
    );
}

#[test]
fn extreme_tile_errors_instead_of_panicking() {
    let s = slicer(4096, 0);
    let l = line(vec![(0, 0), (10, 10)]);
    // A tile whose (buffered) box coordinates overflow i32 → Overflow, not a panic.
    assert_eq!(
        s.slice(&l, TileId::new(i32::MAX, i32::MAX)),
        Err(Error::Overflow)
    );
    assert_eq!(s.slice(&l, TileId::new(i32::MIN, 0)), Err(Error::Overflow));
    // A far-but-representable tile touches nothing → Ok(None).
    assert_eq!(s.slice(&l, TileId::new(1000, 1000)), Ok(None));
}

#[test]
fn spanning_too_many_tiles_errors() {
    let s = slicer(1, 0); // 1 unit per tile
    // Spans 40 000 tiles on x, past i16::MAX (32 767).
    assert_eq!(
        s.slice_all(&line(vec![(0, 0), (40_000, 0)])),
        Err(Error::TooManyTiles)
    );
}

#[test]
fn coordinate_overflow_errors() {
    let s = slicer(4096, 8);
    // A vertex within `buffer` of i32::MAX: the buffered bound overflows i32 → Overflow.
    assert_eq!(
        s.slice_all(&line(vec![(i32::MAX, 0), (i32::MAX, 10)])),
        Err(Error::Overflow)
    );
}

#[test]
fn too_many_vertices_errors() {
    // Huge divider → everything in one tile, so only the vertex-count limit can trip.
    let s = slicer(1_000_000, 0);
    let coords: Vec<Coord<i32>> = (0..=(i32::from(u16::MAX) + 1))
        .map(|i| Coord { x: i % 8, y: 0 })
        .collect();
    let l = Geometry::LineString(LineString(coords));
    assert_eq!(s.slice_all(&l), Err(Error::PolylineTooLarge));
}

#[test]
fn empty_and_degenerate_inputs_are_ok() {
    let s = slicer(25, 0);
    // No lines / empty line → no tiles, no error.
    assert_eq!(
        s.slice_all(&Geometry::LineString(LineString(vec![]))),
        Ok(vec![])
    );
    // A single-point line touches its own tile but yields no ≥2-vertex piece.
    let dot = line(vec![(5, 5)]);
    assert_eq!(s.slice_all(&dot), Ok(vec![]));
    assert_eq!(s.slice(&dot, TileId::new(0, 0)), Ok(None));
}
