//! End-to-end tests for the slicing API, including a round-trip through `fast-mvt`.

use std::num::NonZeroU32;

use fast_mvt::{MvtLayerBuilder, MvtReaderRef};
use geo_types::{Coord, Geometry, GeometryCollection, LineString, MultiLineString, Point, Polygon};
use map_tile_toolkit::{SliceOptions, slice_all_tiles, slice_tile};

/// Half the Web Mercator plane extent, in meters.
const ORIGIN_SHIFT: f64 = 40_075_016.685_578_5 / 2.0;

fn opts() -> SliceOptions {
    SliceOptions::new(NonZeroU32::new(4096).expect("nonzero"), 64)
}

/// Every coordinate of a sliced geometry must fall within the tile plus its buffer.
///
/// The transform floors coordinates and flips Y, which can nudge an extreme value one
/// unit past the exact buffer edge, so the bound carries a `±1` slack.
fn assert_within_buffer(geom: &Geometry<i32>, opts: SliceOptions) {
    let buffer = i32::try_from(opts.buffer).expect("buffer fits i32");
    let extent = i32::try_from(opts.extent.get()).expect("extent fits i32");
    let lo = -buffer - 1;
    let hi = extent + buffer + 1;
    for c in coords(geom) {
        assert!(
            c.x >= lo && c.x <= hi && c.y >= lo && c.y <= hi,
            "coord {c:?} outside [{lo}, {hi}]"
        );
    }
}

/// Collect the polygons of a sliced area geometry (a single polygon comes back wrapped in a
/// `MultiPolygon`, since `geo`'s intersection always returns one).
fn polygons(geom: &Geometry<i32>) -> Vec<Polygon<i32>> {
    match geom {
        Geometry::Polygon(p) => vec![p.clone()],
        Geometry::MultiPolygon(mp) => mp.0.clone(),
        other => panic!("expected polygonal geometry, got {other:?}"),
    }
}

fn coords(geom: &Geometry<i32>) -> Vec<Coord<i32>> {
    use geo::CoordsIter as _;
    geom.coords_iter().collect()
}

/// Shoelace signed area (doubled) of a ring, in integer tile space.
fn signed_area2(ring: &LineString<i32>) -> i64 {
    let pts: Vec<_> = ring.coords().collect();
    let mut acc: i64 = 0;
    for w in pts.windows(2) {
        acc += i64::from(w[0].x) * i64::from(w[1].y) - i64::from(w[1].x) * i64::from(w[0].y);
    }
    acc
}

#[test]
fn polygon_clipped_to_quadrant_stays_within_buffer() {
    // A polygon covering most of the world, sliced to the NW quadrant tile at z1.
    let s = ORIGIN_SHIFT * 0.9;
    let poly = Geometry::Polygon(Polygon::new(
        LineString::from(vec![(-s, -s), (s, -s), (s, s), (-s, s), (-s, -s)]),
        vec![],
    ));
    let sliced = slice_tile(&poly, (0, 0, 1), opts()).expect("quadrant is covered");
    assert_within_buffer(&sliced, opts());
    assert!(matches!(
        sliced,
        Geometry::Polygon(_) | Geometry::MultiPolygon(_)
    ));
}

#[test]
fn linestring_crossing_boundary_is_clipped() {
    // From inside tile (0,0,1) (x<0) to inside tile (1,0,1) (x>0): crosses the x=0 edge.
    let line = Geometry::LineString(LineString::from(vec![
        (-ORIGIN_SHIFT * 0.5, ORIGIN_SHIFT * 0.5),
        (ORIGIN_SHIFT * 0.5, ORIGIN_SHIFT * 0.5),
    ]));
    let sliced = slice_tile(&line, (0, 0, 1), opts()).expect("line touches the tile");
    assert_within_buffer(&sliced, opts());
    assert!(
        coords(&sliced).len() >= 2,
        "clipped line keeps at least 2 vertices"
    );
}

#[test]
fn multilinestring_is_clipped() {
    let mls = Geometry::MultiLineString(MultiLineString(vec![
        LineString::from(vec![
            (-ORIGIN_SHIFT * 0.5, ORIGIN_SHIFT * 0.5),
            (ORIGIN_SHIFT * 0.5, ORIGIN_SHIFT * 0.5),
        ]),
        LineString::from(vec![
            (-ORIGIN_SHIFT * 0.8, ORIGIN_SHIFT * 0.2),
            (-ORIGIN_SHIFT * 0.2, ORIGIN_SHIFT * 0.2),
        ]),
    ]));
    let sliced = slice_tile(&mls, (0, 0, 1), opts()).expect("lines touch the tile");
    assert_within_buffer(&sliced, opts());
}

#[test]
fn polygon_with_hole_has_opposite_ring_windings() {
    let s = ORIGIN_SHIFT * 0.8;
    let h = ORIGIN_SHIFT * 0.3;
    // Exterior fully inside the NW quadrant, with a hole.
    let poly = Geometry::Polygon(Polygon::new(
        LineString::from(vec![
            (-s, s * 0.1),
            (-s * 0.1, s * 0.1),
            (-s * 0.1, s),
            (-s, s),
            (-s, s * 0.1),
        ]),
        vec![LineString::from(vec![
            (-h - h, h),
            (-h, h),
            (-h, h + h),
            (-h - h, h + h),
            (-h - h, h),
        ])],
    ));
    let sliced = slice_tile(&poly, (0, 0, 1), opts()).expect("polygon is inside the tile");
    let polys = polygons(&sliced);
    let with_hole = polys
        .iter()
        .find(|p| !p.interiors().is_empty())
        .expect("a hole survived slicing");
    let ext = signed_area2(with_hole.exterior());
    for hole in with_hole.interiors() {
        let inner = signed_area2(hole);
        assert!(
            ext.signum() != inner.signum() && inner != 0,
            "exterior ({ext}) and hole ({inner}) must wind oppositely"
        );
    }
}

#[test]
fn geometry_outside_tile_yields_none() {
    // A point in the SE quadrant is not in the NW tile (0,0,1).
    let pt = Geometry::Point(Point::new(ORIGIN_SHIFT * 0.5, -ORIGIN_SHIFT * 0.5));
    assert!(slice_tile(&pt, (0, 0, 1), opts()).is_none());
}

#[test]
fn geometry_collection_is_preserved_per_member() {
    let gc = Geometry::GeometryCollection(GeometryCollection(vec![
        Geometry::Point(Point::new(-ORIGIN_SHIFT * 0.5, ORIGIN_SHIFT * 0.5)),
        Geometry::Point(Point::new(-ORIGIN_SHIFT * 0.2, ORIGIN_SHIFT * 0.2)),
    ]));
    let sliced = slice_tile(&gc, (0, 0, 1), opts()).expect("both points are inside");
    let Geometry::GeometryCollection(members) = &sliced else {
        panic!("expected a GeometryCollection, got {sliced:?}");
    };
    assert_eq!(members.0.len(), 2);
}

#[test]
fn batch_matches_single_tile() {
    use geo::{Area as _, Convert as _};

    let s = ORIGIN_SHIFT * 0.9;
    let poly = Geometry::Polygon(Polygon::new(
        LineString::from(vec![(-s, -s), (s, -s), (s, s), (-s, s), (-s, -s)]),
        vec![],
    ));
    let zoom = 1;
    let mut count = 0;
    for (tile, batch_geom) in slice_all_tiles(&poly, zoom, opts()) {
        count += 1;
        let single = slice_tile(&poly, tile, opts()).expect("batch tile must also slice singly");
        // The batch path uses the eager stripe slicer and the single-tile path uses the
        // rectangle clip. They produce equivalent slices up to ±1px integer snapping, so
        // compare areas with a small tolerance rather than requiring identical vertices.
        let (b, s): (Geometry<f64>, Geometry<f64>) = (batch_geom.convert(), single.convert());
        let (ba, sa) = (b.unsigned_area(), s.unsigned_area());
        assert!(
            ba > 0.0 && (ba - sa).abs() / ba < 0.01,
            "batch ({ba}) and single-tile ({sa}) areas differ for {tile:?}"
        );
    }
    assert_eq!(count, 4, "the world-spanning polygon covers all 4 z1 tiles");
}

#[test]
fn round_trips_through_fast_mvt() {
    let s = ORIGIN_SHIFT * 0.8;
    let poly = Geometry::Polygon(Polygon::new(
        LineString::from(vec![
            (-s, s * 0.1),
            (-s * 0.1, s * 0.1),
            (-s * 0.1, s),
            (-s, s),
            (-s, s * 0.1),
        ]),
        vec![],
    ));
    let extent = NonZeroU32::new(4096).expect("nonzero");
    let sliced = slice_tile(&poly, (0, 0, 1), SliceOptions::new(extent, 64))
        .expect("polygon is inside the tile");

    // Encode with fast-mvt.
    let mut layer = MvtLayerBuilder::new("test").expect("layer name is valid");
    layer.extent(extent);
    let mut feature = layer.feature(&sliced).expect("geometry encodes");
    feature.tag_string("k", "v").expect("tag encodes");
    let bytes = feature.end().encode();

    // Decode and confirm one feature with a polygon geometry survives.
    let tile = MvtReaderRef::new(&bytes)
        .expect("decodes")
        .to_tile()
        .expect("to owned tile");
    assert_eq!(tile.layers.len(), 1);
    let features = &tile.layers[0].features;
    assert_eq!(features.len(), 1);
    assert!(matches!(
        features[0].geometry,
        Geometry::Polygon(_) | Geometry::MultiPolygon(_)
    ));
}

#[test]
fn empty_geometry_yields_no_tiles() {
    let empty = Geometry::MultiPoint(geo_types::MultiPoint(vec![]));
    assert_eq!(slice_all_tiles(&empty, 1, opts()).count(), 0);
}
