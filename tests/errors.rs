//! The public API never panics: invalid input yields a typed [`TileError`] instead.

use geo_types::Coord;
use map_tile_toolkit::{
    PolygonSlicerAll, PolygonSlicerOne, SlicerAll, SlicerOne, TileError, TileId,
};

/// A polyline as a `Vec<Coord<i32>>`.
fn line(coords: Vec<(i32, i32)>) -> Vec<Coord<i32>> {
    coords.into_iter().map(|(x, y)| Coord { x, y }).collect()
}

/// A fresh all-tiles slicer over `Coord` (the config validation is what these tests probe).
fn all(extent: u32, buffer: u16) -> Result<SlicerAll<Coord<i32>>, TileError> {
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
            (t.tile_id(), runs)
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
    assert_eq!(all(0, 0).err(), Some(TileError::InvalidExtent));
    assert_eq!(all(u32::MAX, 0).err(), Some(TileError::InvalidExtent));
    assert!(all(1, 0).is_ok());
    assert!(all(i32::MAX as u32, u16::MAX).is_ok());
    // Both slicers validate the extent the same way.
    assert_eq!(
        SlicerOne::<Coord<i32>>::new(0, 0, TileId::new(0, 0)).err(),
        Some(TileError::InvalidExtent)
    );
}

#[test]
fn buffer_too_large() {
    // `buffer` must be strictly less than half the `extent` (i.e. `2*buffer < extent`).
    assert_eq!(all(10, 5).err(), Some(TileError::BufferTooLarge)); // 2*5 == 10, not < 10
    assert_eq!(all(10, 6).err(), Some(TileError::BufferTooLarge));
    assert!(all(10, 4).is_ok()); // 2*4 == 8 < 10
    // With extent 1 only a zero buffer is allowed.
    assert!(all(1, 0).is_ok());
    assert_eq!(all(2, 1).err(), Some(TileError::BufferTooLarge));
    // Both slicers validate the buffer the same way.
    assert_eq!(
        SlicerOne::<Coord<i32>>::new(10, 5, TileId::new(0, 0)).err(),
        Some(TileError::BufferTooLarge)
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
        Some(TileError::Overflow)
    );
    assert_eq!(
        one(4096, 0, TileId::new(i32::MIN, 0)).add_feature(&l).err(),
        Some(TileError::Overflow)
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
        Some(TileError::TooManyTiles)
    );
}

#[test]
fn coordinate_overflow_errors() {
    let mut s = all(4096, 8).expect("valid config");
    // A vertex within `buffer` of i32::MAX: the buffered bound overflows i32 → Overflow.
    assert_eq!(
        s.add_feature(line(vec![(i32::MAX, 0), (i32::MAX, 10)]))
            .err(),
        Some(TileError::Overflow)
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
        Some(TileError::PolylineTooLarge)
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
        Some(TileError::Overflow)
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
fn rollback_leaves_untouched_tiles_alone() {
    // A clean feature near the origin, then a failing one near i32::MIN — their tiles are disjoint.
    // The failing feature never touches the clean tile, so its rollback must scan past that tile on
    // the "not touched by this feature" branch (leaving it intact) while still dropping the far tiles
    // it did create before erroring. The atomicity test above only exercises the *touched* branch.
    let mut s = all(4096, 8).expect("valid config");
    s.add_feature(line(vec![(0, 0), (100, 0)]))
        .expect("clean feature near the origin");
    let before = snapshot(&s);

    assert_eq!(
        s.add_feature(errors_after_emitting()).err(),
        Some(TileError::Overflow),
        "the far feature errors mid-walk"
    );
    assert_eq!(
        snapshot(&s),
        before,
        "rolling back a feature that touches none of the pre-existing tiles leaves them intact"
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

// ---- PolygonSlicerAll ----

/// One polygon (exterior only) from integer corner pairs.
fn poly(coords: Vec<(i32, i32)>) -> Vec<Vec<Coord<i32>>> {
    vec![line(coords)]
}

/// Every edge tile's pieces and every fill run, per feature — the slicer's whole observable state.
type PolyState = Vec<(
    Vec<(TileId, Vec<Vec<Vec<Coord<i32>>>>)>,
    Vec<map_tile_toolkit::FillRun>,
)>;

fn poly_state(s: &PolygonSlicerAll<Coord<i32>>) -> PolyState {
    s.iter_features()
        .map(|f| {
            let tiles = f
                .iter_tiles()
                .map(|t| {
                    let polys = t
                        .iter_polygons()
                        .map(|p| p.iter_rings().map(|r| r.vertices().to_vec()).collect())
                        .collect();
                    (t.tile_id(), polys)
                })
                .collect();
            (tiles, f.iter_fill_runs().collect())
        })
        .collect()
}

#[test]
fn polygon_all_validates_config() {
    assert_eq!(
        PolygonSlicerAll::<Coord<i32>>::new(0, 0).err(),
        Some(TileError::InvalidExtent)
    );
    assert_eq!(
        PolygonSlicerAll::<Coord<i32>>::new(10, 5).err(),
        Some(TileError::BufferTooLarge)
    );
}

#[test]
fn polygon_all_too_many_tiles() {
    let mut s = PolygonSlicerAll::<Coord<i32>>::new(1, 0).expect("valid config");
    // A ring spanning 40 000 tiles on x, past i16::MAX.
    assert_eq!(
        s.add_feature([poly(vec![(0, 0), (40_000, 0), (40_000, 5)])])
            .err(),
        Some(TileError::TooManyTiles)
    );
    assert!(s.is_empty());

    // The candidate-tile budget (2^25 = 33 554 432) is shared by all rings of a feature: the small
    // triangle charges ~362 000 candidates (its 601² diagonal box plus two edges) and the large one's
    // first edge 5781² = 33 419 961 — each fine alone, too many together (the large one is rejected
    // as soon as its first box is charged). Covered tiles are never charged: the small triangle alone
    // fills ~180 000 tiles.
    let small = poly(vec![(0, 0), (600, 600), (0, 600)]);
    let large = poly(vec![(0, 0), (5780, 5780), (0, 5780)]);
    s.add_feature([small.clone()])
        .expect("one ring is within budget");
    assert_eq!(s.len(), 1);
    let filled: usize = s
        .iter_features()
        .flat_map(|f| f.iter_fill_runs())
        .map(|r| r.x.len())
        .sum();
    assert!(filled > 170_000, "{filled}");
    let before = poly_state(&s);
    assert_eq!(
        s.add_feature([small, large]).err(),
        Some(TileError::TooManyTiles)
    );
    assert_eq!(poly_state(&s), before, "a rejected feature leaves no trace");
}

#[test]
fn polygon_all_is_atomic_on_error() {
    // A square whose left edge runs through the column of tiles based at x = i32::MIN (extent 4096,
    // buffer 0). Routing succeeds, but closing that edge's ring in tile row 1 needs a `B⁺` corner one
    // unit left of i32::MIN → Overflow, after row 0's tiles were already written.
    let (x0, x1) = (i32::MIN + 10, i32::MIN + 6000);
    let bad = poly(vec![(x0, 0), (x1, 0), (x1, 10_000), (x0, 10_000)]);
    let mut one =
        PolygonSlicerOne::<Coord<i32>>::new(4096, 0, TileId::new(-524_288, 1)).expect("config");
    assert_eq!(
        one.add_feature(&bad[0], &[]).err(),
        Some(TileError::Overflow),
        "the single-tile clip agrees"
    );

    let good = poly(vec![(5, 5), (9000, 5), (9000, 9000), (5, 9000)]);
    let mut s = PolygonSlicerAll::<Coord<i32>>::new(4096, 0).expect("valid config");
    s.add_feature([good.clone()]).expect("clean feature");
    let before = poly_state(&s);
    assert_eq!(s.add_feature([bad]).err(), Some(TileError::Overflow));
    assert_eq!(
        poly_state(&s),
        before,
        "errored feature must roll back fully"
    );

    // Still usable, and identical to a slicer that never saw the bad input.
    s.add_feature([good.clone()]).expect("still usable");
    let mut clean = PolygonSlicerAll::<Coord<i32>>::new(4096, 0).expect("valid config");
    clean.add_feature([good.clone()]).expect("clean");
    clean.add_feature([good]).expect("clean");
    assert_eq!(poly_state(&s), poly_state(&clean));
}

#[test]
fn polygon_all_degenerate_inputs_and_clear() {
    let mut s = PolygonSlicerAll::<Coord<i32>>::new(25, 0).expect("valid config");
    // No polygons, a polygon with no rings, and a degenerate exterior (whose holes are then ignored):
    // nothing is recorded.
    s.add_feature(Vec::<Vec<Vec<Coord<i32>>>>::new())
        .expect("empty");
    s.add_feature([Vec::<Vec<Coord<i32>>>::new()])
        .expect("no rings");
    s.add_feature([vec![
        line(vec![(1, 1), (9, 9), (1, 1)]),
        line(vec![(2, 2), (8, 2), (8, 8)]),
    ]])
    .expect("degenerate exterior");
    assert!(s.is_empty());
    // A degenerate hole is ignored.
    let square = line(vec![(2, 2), (60, 2), (60, 60), (2, 60)]);
    s.add_feature([vec![square.clone(), line(vec![(30, 30), (31, 31)])]])
        .expect("degenerate hole");
    let mut plain = PolygonSlicerAll::<Coord<i32>>::new(25, 0).expect("valid config");
    plain.add_feature([vec![square.clone()]]).expect("plain");
    assert_eq!(poly_state(&s), poly_state(&plain));

    // After clear the slicer behaves exactly like a fresh one.
    s.clear();
    assert!(s.is_empty());
    assert_eq!(s.len(), 0);
    s.add_feature([vec![square]]).expect("reuse");
    assert_eq!(poly_state(&s), poly_state(&plain));
}
