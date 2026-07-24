//! The public API never panics: invalid input yields a typed [`SliceError`] instead.

use geo_types::Coord;
use map_tile_toolkit::{SliceError, SlicerAll, SlicerOne, TileId};

/// A polyline as a `Vec<Coord<i32>>`.
fn line(coords: Vec<(i32, i32)>) -> Vec<Coord<i32>> {
    coords.into_iter().map(|(x, y)| Coord { x, y }).collect()
}

/// A fresh all-tiles slicer over `Coord` (the config validation is what these tests probe).
fn all(extent: u32, buffer: u16) -> Result<SlicerAll<Coord<i32>>, SliceError> {
    SlicerAll::new(extent, buffer)
}

/// A fresh single-tile slicer over `Coord`, bound to `tile`.
fn one(extent: u32, buffer: u16, tile: TileId) -> SlicerOne<Coord<i32>> {
    SlicerOne::new(extent, buffer, tile).expect("valid config")
}

/// The observable state of an all-tiles slicer: every tile's runs, ordered by tile then feature.
fn snapshot(s: &SlicerAll<Coord<i32>>) -> Vec<(TileId, Vec<Vec<Coord<i32>>>)> {
    s.iter_tiles()
        .map(|t| {
            let runs = t
                .iter_features()
                .flat_map(|f| f.iter_polylines().map(<[_]>::to_vec))
                .collect();
            (t.id(), runs)
        })
        .collect()
}

/// A polyline that starts cleanly in a tile near `i32::MIN` (which the slicer emits into) and then
/// runs to a vertex whose tile's buffered box underflows `i32` — so `add_feature` errors *mid-walk*,
/// after it has already written pieces. `extent = 4096`, `buffer = 8`: the low vertex is `i32::MIN+8`
/// (itself within buffer of the edge, so the up-front bounds check passes) but its tile's base minus
/// the buffer underflows.
fn errors_after_emitting() -> Vec<Coord<i32>> {
    line(vec![
        (i32::MIN + 8200, 0), // tile −524286 (base i32::MIN+8192): clean, gets emitted
        (i32::MIN + 8250, 0),
        (i32::MIN + 8, 0), // tile −524288 (base i32::MIN): base − buffer underflows → Overflow
    ])
}

#[test]
fn invalid_extent() {
    assert_eq!(all(0, 0).err(), Some(SliceError::InvalidExtent));
    assert_eq!(all(u32::MAX, 0).err(), Some(SliceError::InvalidExtent));
    assert!(all(1, 0).is_ok());
    assert!(all(i32::MAX as u32, u16::MAX).is_ok());
    // Both slicers validate the extent the same way.
    assert_eq!(
        SlicerOne::<Coord<i32>>::new(0, 0, TileId::new(0, 0)).err(),
        Some(SliceError::InvalidExtent)
    );
}

#[test]
fn buffer_too_large() {
    // `buffer` must be strictly less than half the `extent` (i.e. `2*buffer < extent`).
    assert_eq!(all(10, 5).err(), Some(SliceError::BufferTooLarge)); // 2*5 == 10, not < 10
    assert_eq!(all(10, 6).err(), Some(SliceError::BufferTooLarge));
    assert!(all(10, 4).is_ok()); // 2*4 == 8 < 10
    // With extent 1 only a zero buffer is allowed.
    assert!(all(1, 0).is_ok());
    assert_eq!(all(2, 1).err(), Some(SliceError::BufferTooLarge));
    // Both slicers validate the buffer the same way.
    assert_eq!(
        SlicerOne::<Coord<i32>>::new(10, 5, TileId::new(0, 0)).err(),
        Some(SliceError::BufferTooLarge)
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
        Some(SliceError::UnsupportedGeometry("Point"))
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
        Some(SliceError::Overflow)
    );
    assert_eq!(
        one(4096, 0, TileId::new(i32::MIN, 0)).add_feature(&l).err(),
        Some(SliceError::Overflow)
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
        Some(SliceError::TooManyTiles)
    );
}

#[test]
fn coordinate_overflow_errors() {
    let mut s = all(4096, 8).expect("valid config");
    // A vertex within `buffer` of i32::MAX: the buffered bound overflows i32 → Overflow.
    assert_eq!(
        s.add_feature(line(vec![(i32::MAX, 0), (i32::MAX, 10)]))
            .err(),
        Some(SliceError::Overflow)
    );
}

#[test]
fn too_many_vertices_errors() {
    // Huge extent → everything in one tile, so only the vertex-count limit can trip.
    let mut s = all(1_000_000, 0).expect("valid config");
    let coords: Vec<Coord<i32>> = (0..=(i32::from(u16::MAX) + 1))
        .map(|i| Coord { x: i % 8, y: 0 })
        .collect();
    assert_eq!(
        s.add_feature(&coords).err(),
        Some(SliceError::PolylineTooLarge)
    );
}

#[test]
fn add_feature_is_atomic_on_error() {
    // The crafted polyline really does fail after emitting (otherwise this proves nothing).
    let mut probe = all(4096, 8).expect("valid config");
    probe
        .add_feature(errors_after_emitting())
        .expect_err("must error mid-walk");
    assert!(
        probe.is_empty(),
        "a feature that errors before any other data must leave the slicer empty"
    );

    // A clean feature that shares a tile with the failing one, so rollback must *truncate* an existing
    // tile (not just drop freshly-created ones).
    let mut s = all(4096, 8).expect("valid config");
    s.add_feature(line(vec![(i32::MIN + 8200, 0), (i32::MIN + 8300, 0)]))
        .expect("clean feature");
    let before = snapshot(&s);

    // The failing feature must leave no trace — the accumulator is byte-for-byte what it was.
    assert_eq!(
        s.add_feature(errors_after_emitting()).err(),
        Some(SliceError::Overflow)
    );
    assert_eq!(snapshot(&s), before, "errored feature must roll back fully");

    // And the slicer stays usable: skipping the bad input and adding more works, and matches a run
    // that never saw the bad input at all.
    s.add_feature(line(vec![(i32::MIN + 8210, 0), (i32::MIN + 8260, 0)]))
        .expect("still usable after a rolled-back error");
    let mut clean = all(4096, 8).expect("valid config");
    clean
        .add_feature(line(vec![(i32::MIN + 8200, 0), (i32::MIN + 8300, 0)]))
        .expect("clean feature");
    clean
        .add_feature(line(vec![(i32::MIN + 8210, 0), (i32::MIN + 8260, 0)]))
        .expect("clean feature");
    assert_eq!(
        snapshot(&s),
        snapshot(&clean),
        "skipping a failed feature matches never adding it"
    );
}

#[test]
fn clear_resets_for_reuse() {
    let mut s = all(25, 0).expect("valid config");
    s.add_feature(line(vec![(5, 5), (60, 40)])).expect("slice");
    assert!(!s.is_empty());
    s.clear();
    assert!(s.is_empty());
    assert_eq!(s.len(), 0);
    // After clear the slicer behaves exactly like a fresh one.
    let mut fresh = all(25, 0).expect("valid config");
    s.add_feature(line(vec![(1, 1), (30, 30)])).expect("slice");
    fresh
        .add_feature(line(vec![(1, 1), (30, 30)]))
        .expect("slice");
    assert_eq!(snapshot(&s), snapshot(&fresh));

    // SlicerOne clears too.
    let mut o = one(25, 0, TileId::new(0, 0));
    o.add_feature(line(vec![(5, 5), (20, 20)])).expect("slice");
    assert!(!o.is_empty());
    o.clear();
    assert!(o.is_empty());
    assert_eq!(o.tile(), TileId::new(0, 0), "clear keeps the bound tile");
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
