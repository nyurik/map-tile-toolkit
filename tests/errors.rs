//! The public API never panics: invalid input yields a typed [`Error`] instead.

use geo_types::Coord;
use map_tile_toolkit::{Error, SlicerAll, SlicerOne, TileId};

/// A polyline as a `Vec<Coord<i32>>`.
fn line(coords: Vec<(i32, i32)>) -> Vec<Coord<i32>> {
    coords.into_iter().map(|(x, y)| Coord { x, y }).collect()
}

/// A fresh all-tiles slicer over `Coord` (the config validation is what these tests probe).
fn all(divider: u32, buffer: u16) -> Result<SlicerAll<Coord<i32>>, Error> {
    SlicerAll::new(divider, buffer)
}

/// A fresh single-tile slicer over `Coord`, bound to `tile`.
fn one(divider: u32, buffer: u16, tile: TileId) -> SlicerOne<Coord<i32>> {
    SlicerOne::new(divider, buffer, tile).expect("valid config")
}

#[test]
fn invalid_divider() {
    assert_eq!(all(0, 0).err(), Some(Error::InvalidDivider));
    assert_eq!(all(u32::MAX, 0).err(), Some(Error::InvalidDivider));
    assert!(all(1, 0).is_ok());
    assert!(all(i32::MAX as u32, u16::MAX).is_ok());
    // Both slicers validate the divider the same way.
    assert_eq!(
        SlicerOne::<Coord<i32>>::new(0, 0, TileId::new(0, 0)).err(),
        Some(Error::InvalidDivider)
    );
}

#[cfg(feature = "geo")]
#[test]
fn non_polyline_geometry_errors() {
    use geo_types::{Geometry, Point};
    let mut s = all(25, 0).expect("valid config");
    let point = Geometry::Point(Point::new(1, 2));
    assert_eq!(
        s.add_geometry(&point).err(),
        Some(Error::UnsupportedGeometry("Point"))
    );
}

#[test]
fn extreme_tile_errors_instead_of_panicking() {
    let l = line(vec![(0, 0), (10, 10)]);
    // A tile whose (buffered) box coordinates overflow i32 → Overflow, not a panic.
    assert_eq!(
        one(4096, 0, TileId::new(i32::MAX, i32::MAX))
            .add_feature(&l)
            .err(),
        Some(Error::Overflow)
    );
    assert_eq!(
        one(4096, 0, TileId::new(i32::MIN, 0)).add_feature(&l).err(),
        Some(Error::Overflow)
    );
    // A far-but-representable tile touches nothing → no features accumulated.
    let mut far = one(4096, 0, TileId::new(1000, 1000));
    far.add_feature(&l).expect("no error for a far tile");
    assert!(far.is_empty());
}

#[test]
fn spanning_too_many_tiles_errors() {
    let mut s = all(1, 0).expect("valid config"); // 1 unit per tile
    // Spans 40 000 tiles on x, past i16::MAX (32 767).
    assert_eq!(
        s.add_feature(line(vec![(0, 0), (40_000, 0)])).err(),
        Some(Error::TooManyTiles)
    );
}

#[test]
fn coordinate_overflow_errors() {
    let mut s = all(4096, 8).expect("valid config");
    // A vertex within `buffer` of i32::MAX: the buffered bound overflows i32 → Overflow.
    assert_eq!(
        s.add_feature(line(vec![(i32::MAX, 0), (i32::MAX, 10)]))
            .err(),
        Some(Error::Overflow)
    );
}

#[test]
fn too_many_vertices_errors() {
    // Huge divider → everything in one tile, so only the vertex-count limit can trip.
    let mut s = all(1_000_000, 0).expect("valid config");
    let coords: Vec<Coord<i32>> = (0..=(i32::from(u16::MAX) + 1))
        .map(|i| Coord { x: i % 8, y: 0 })
        .collect();
    assert_eq!(s.add_feature(&coords).err(), Some(Error::PolylineTooLarge));
}

#[test]
fn empty_and_degenerate_inputs_are_ok() {
    let mut s = all(25, 0).expect("valid config");
    // Empty polyline → no tiles, no error.
    s.add_feature(Vec::<Coord<i32>>::new())
        .expect("empty polyline is ok");
    assert!(s.is_empty());
    // A single-point polyline touches its own tile but yields no ≥2-vertex run.
    let dot = line(vec![(5, 5)]);
    s.add_feature(&dot).expect("single-point polyline is ok");
    assert!(s.is_empty());

    let mut one = one(25, 0, TileId::new(0, 0));
    one.add_feature(&dot).expect("single-point polyline is ok");
    assert!(one.is_empty());
}
