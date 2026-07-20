//! Shared test helpers ported from planetiler's `TestUtils` and the per-file helpers in
//! `TiledGeometryTest` / `FeatureRendererTest`.
//!
//! Coordinate conventions match planetiler: geometry constructors take raw coordinates;
//! `render` scales world coords by `2^zoom`, slices, and reassembles into tile-local
//! `0..256` pixel space. Comparisons are topological (winding/orientation-insensitive) after
//! rounding, mirroring `equalsNorm`/`equalsTopo`.

#![allow(
    dead_code,
    clippy::pedantic,
    reason = "test support shared across integration-test crates; not all helpers used in each"
)]

use std::collections::BTreeMap;
use std::fs::read_to_string;
use std::path::Path;

use geo::{AffineOps, AffineTransform, MapCoords, Relate};
use geo_types::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};
use map_tile_toolkit::TileId;
use map_tile_toolkit::extents::ForZoom;
use map_tile_toolkit::stripe::{CoordSeqGroups, TiledGeometry};
use wkt::{ToWkt, TryFromWkt};

/// Tile-local coordinate-space side length (planetiler `SIZE`).
pub const SIZE: f64 = 256.0;

// ---------------------------------------------------------------------------
// Geometry constructors (TestUtils.*)
// ---------------------------------------------------------------------------

/// Turn a flat `[x0, y0, x1, y1, …]` list into a coordinate vector.
pub fn coords(flat: &[f64]) -> Vec<Coord<f64>> {
    flat.chunks_exact(2)
        .map(|c| Coord { x: c[0], y: c[1] })
        .collect()
}

pub fn line_string(flat: &[f64]) -> LineString<f64> {
    LineString(coords(flat))
}

pub fn new_line_string(flat: &[f64]) -> Geometry<f64> {
    Geometry::LineString(line_string(flat))
}

pub fn new_multi_line_string(lines: Vec<LineString<f64>>) -> Geometry<f64> {
    Geometry::MultiLineString(MultiLineString(lines))
}

pub fn new_point(x: f64, y: f64) -> Geometry<f64> {
    Geometry::Point(Point::new(x, y))
}

pub fn new_multi_point(points: &[(f64, f64)]) -> Geometry<f64> {
    Geometry::MultiPoint(MultiPoint(
        points.iter().map(|&(x, y)| Point::new(x, y)).collect(),
    ))
}

/// CCW ring for a rectangle, matching planetiler's `rectangleCoordList`:
/// `(minX,minY),(maxX,minY),(maxX,maxY),(minX,maxY),(minX,minY)`.
pub fn rectangle_coord_list(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> LineString<f64> {
    line_string(&[
        min_x, min_y, max_x, min_y, max_x, max_y, min_x, max_y, min_x, min_y,
    ])
}

pub fn rectangle_coord_list_sq(min: f64, max: f64) -> LineString<f64> {
    rectangle_coord_list(min, min, max, max)
}

pub fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Geometry<f64> {
    Geometry::Polygon(Polygon::new(
        rectangle_coord_list(min_x, min_y, max_x, max_y),
        vec![],
    ))
}

pub fn rectangle_sq(min: f64, max: f64) -> Geometry<f64> {
    rectangle(min, min, max, max)
}

pub fn new_polygon(flat: &[f64]) -> Geometry<f64> {
    Geometry::Polygon(Polygon::new(line_string(flat), vec![]))
}

pub fn new_polygon_holes(exterior: LineString<f64>, holes: Vec<LineString<f64>>) -> Geometry<f64> {
    Geometry::Polygon(Polygon::new(exterior, holes))
}

pub fn new_multi_polygon(polys: Vec<Polygon<f64>>) -> Geometry<f64> {
    Geometry::MultiPolygon(MultiPolygon(polys))
}

pub fn new_geometry_collection(geoms: Vec<Geometry<f64>>) -> Geometry<f64> {
    Geometry::GeometryCollection(GeometryCollection(geoms))
}

// Tile-piece builders used by the fill tests (tile-local 0..256 space plus buffer).
pub fn tile_fill(buffer: f64) -> LineString<f64> {
    rectangle_coord_list(-buffer, -buffer, SIZE + buffer, SIZE + buffer)
}
pub fn tile_top(buffer: f64) -> Geometry<f64> {
    rectangle(-buffer, -buffer, SIZE + buffer, SIZE / 2.0)
}
pub fn tile_bottom(buffer: f64) -> Geometry<f64> {
    rectangle(-buffer, SIZE / 2.0, SIZE + buffer, SIZE + buffer)
}
pub fn tile_left(buffer: f64) -> Geometry<f64> {
    rectangle(-buffer, -buffer, SIZE / 2.0, SIZE + buffer)
}
pub fn tile_right(buffer: f64) -> Geometry<f64> {
    rectangle(SIZE / 2.0, -buffer, SIZE + buffer, SIZE + buffer)
}
pub fn tile_top_left(buffer: f64) -> Geometry<f64> {
    rectangle(-buffer, -buffer, SIZE / 2.0, SIZE / 2.0)
}
pub fn tile_top_right(buffer: f64) -> Geometry<f64> {
    rectangle(SIZE / 2.0, -buffer, SIZE + buffer, SIZE / 2.0)
}
pub fn tile_bottom_left(buffer: f64) -> Geometry<f64> {
    rectangle(-buffer, SIZE / 2.0, SIZE / 2.0, SIZE + buffer)
}
pub fn tile_bottom_right(buffer: f64) -> Geometry<f64> {
    rectangle(SIZE / 2.0, SIZE / 2.0, SIZE + buffer, SIZE + buffer)
}

// ---------------------------------------------------------------------------
// Affine transforms (rotate / flipAndRotate / world / tile)
// ---------------------------------------------------------------------------

/// Rotation about `(ox, oy)` by `degrees`, as an affine transform.
fn rotation(ox: f64, oy: f64, degrees: f64) -> AffineTransform<f64> {
    let (s, c) = degrees.to_radians().sin_cos();
    AffineTransform::new(c, -s, ox - c * ox + s * oy, s, c, oy - s * ox - c * oy)
}

/// Reflection across the vertical line `x = ox`.
fn reflect_x(ox: f64) -> AffineTransform<f64> {
    AffineTransform::new(-1.0, 0.0, 2.0 * ox, 0.0, 1.0, 0.0)
}

/// Reflection across the horizontal line `y = oy`.
fn reflect_y(oy: f64) -> AffineTransform<f64> {
    AffineTransform::new(1.0, 0.0, 0.0, 0.0, -1.0, 2.0 * oy)
}

/// Rotate a ring in place about `(x, y)` (planetiler `rotate`).
pub fn rotate(seq: &mut LineString<f64>, x: f64, y: f64, degrees: i32) {
    seq.affine_transform_mut(&rotation(x, y, f64::from(degrees)));
}

/// Optionally reflect about the vertical/horizontal line through `(x, y)`, then rotate;
/// reverse the ring after a single flip so winding is preserved (planetiler `flipAndRotate`).
pub fn flip_and_rotate(
    seq: &mut LineString<f64>,
    x: f64,
    y: f64,
    flip_x: bool,
    flip_y: bool,
    degrees: i32,
) {
    if flip_x {
        seq.affine_transform_mut(&reflect_x(x));
    }
    if flip_y {
        seq.affine_transform_mut(&reflect_y(y));
    }
    rotate(seq, x, y, degrees);
    if flip_x ^ flip_y {
        seq.0.reverse();
    }
}

/// Rotate a whole geometry about a world origin.
pub fn rotate_world(geom: &Geometry<f64>, ox: f64, oy: f64, degrees: i32) -> Geometry<f64> {
    geom.affine_transform(&rotation(ox, oy, f64::from(degrees)))
}

/// Rotate a whole geometry about the tile pixel center `(128, 128)` (planetiler `rotateTile`).
pub fn rotate_tile(geom: &Geometry<f64>, degrees: i32) -> Geometry<f64> {
    geom.affine_transform(&rotation(SIZE / 2.0, SIZE / 2.0, f64::from(degrees)))
}

// ---------------------------------------------------------------------------
// Rounding / comparison / validation (round, equalsNorm/equalsTopo, validateGeometry)
// ---------------------------------------------------------------------------

/// Round every ordinate to `1/delta` precision (planetiler `round`, default `delta = 1e5`).
pub fn round(geom: &Geometry<f64>, delta: f64) -> Geometry<f64> {
    // `+ 0.0` normalizes `-0.0` to `0.0` so rounded output matches planetiler's WKT.
    geom.map_coords(|c| Coord {
        x: (c.x * delta).round() / delta + 0.0,
        y: (c.y * delta).round() / delta + 0.0,
    })
}

pub fn round_default(geom: &Geometry<f64>) -> Geometry<f64> {
    round(geom, 1e5)
}

/// Doubled shoelace signed area of a ring; `> 0` is CCW in a y-up frame.
pub fn signed_area2(ring: &LineString<f64>) -> f64 {
    let pts = &ring.0;
    let mut acc = 0.0;
    for w in pts.windows(2) {
        acc += w[0].x * w[1].y - w[1].x * w[0].y;
    }
    acc
}

pub fn is_ccw(ring: &LineString<f64>) -> bool {
    signed_area2(ring) > 0.0
}

/// Topological equality after default rounding (planetiler `RoundGeometry`/`equalsNorm`).
pub fn assert_same_normalized(expected: &Geometry<f64>, actual: &Geometry<f64>) {
    let e = round_default(expected);
    let a = round_default(actual);
    assert!(
        e.relate(&a).is_equal_topo(),
        "geometries differ:\n expected: {}\n   actual: {}",
        e.wkt_string(),
        a.wkt_string()
    );
}

/// Topological equality without rounding (planetiler `TopoGeometry`).
pub fn assert_topo_eq(expected: &Geometry<f64>, actual: &Geometry<f64>) {
    assert!(
        expected.relate(actual).is_equal_topo(),
        "geometries differ:\n expected: {}\n   actual: {}",
        expected.wkt_string(),
        actual.wkt_string()
    );
}

/// Assert a rendered per-tile map matches the expected tiles/geometries, order-insensitively
/// and topologically after rounding (planetiler `assertSameNormalizedFeatures`). Pass expected
/// as `[(TileId, vec![geom, …]), …]`.
pub fn assert_tiles(
    expected: Vec<(TileId, Vec<Geometry<f64>>)>,
    actual: &BTreeMap<TileId, Vec<Geometry<f64>>>,
) {
    let expected: BTreeMap<TileId, Vec<Geometry<f64>>> = expected.into_iter().collect();
    let exp_keys: Vec<_> = expected.keys().copied().collect();
    let act_keys: Vec<_> = actual.keys().copied().collect();
    assert_eq!(exp_keys, act_keys, "tile set differs");
    for (tile, want) in &expected {
        let got = &actual[tile];
        assert_eq!(want.len(), got.len(), "geometry count differs at {tile:?}");
        let mut remaining: Vec<&Geometry<f64>> = got.iter().collect();
        for w in want {
            let wr = round_default(w);
            let pos = remaining
                .iter()
                .position(|g| round_default(g).relate(&wr).is_equal_topo())
                .unwrap_or_else(|| {
                    panic!(
                        "no match for {} at {tile:?} among {:?}",
                        wr.wkt_string(),
                        remaining.iter().map(|g| g.wkt_string()).collect::<Vec<_>>()
                    )
                });
            remaining.swap_remove(pos);
        }
    }
}

/// Recursively validate a rendered geometry (planetiler `validateGeometry`): points
/// non-empty; lines ≥2 points; polygon exterior CCW + ≥4 pts + closed, holes CW + closed.
pub fn validate_geometry(geom: &Geometry<f64>) {
    match geom {
        Geometry::Point(_) => {}
        Geometry::MultiPoint(mp) => assert!(!mp.0.is_empty(), "empty multipoint"),
        Geometry::LineString(ls) => assert!(ls.0.len() >= 2, "line has <2 points"),
        Geometry::MultiLineString(mls) => {
            for ls in &mls.0 {
                assert!(ls.0.len() >= 2, "line has <2 points");
            }
        }
        Geometry::Polygon(p) => validate_polygon(p),
        Geometry::MultiPolygon(mp) => mp.0.iter().for_each(validate_polygon),
        Geometry::GeometryCollection(gc) => gc.0.iter().for_each(validate_geometry),
        other => panic!("unexpected geometry variant: {other:?}"),
    }
}

fn validate_polygon(p: &Polygon<f64>) {
    let ext = p.exterior();
    assert!(ext.0.len() >= 4, "exterior ring has <4 points");
    assert!(ext.is_closed(), "exterior ring not closed");
    assert!(is_ccw(ext), "exterior ring must be CCW");
    for hole in p.interiors() {
        assert!(hole.0.len() >= 4, "hole ring has <4 points");
        assert!(hole.is_closed(), "hole ring not closed");
        assert!(!is_ccw(hole), "hole ring must be CW");
    }
}

// ---------------------------------------------------------------------------
// WKT fixtures / snapshots
// ---------------------------------------------------------------------------

/// Parse a `.wkt` fixture into a geometry.
pub fn load_wkt(path: impl AsRef<Path>) -> Geometry<f64> {
    let text = read_to_string(path).expect("read wkt fixture");
    Geometry::try_from_wkt_str(text.trim()).expect("parse wkt fixture")
}

/// A stable WKT string for snapshotting a geometry.
pub fn wkt(geom: &Geometry<f64>) -> String {
    geom.wkt_string()
}

// ---------------------------------------------------------------------------
// render: world geometry -> per-tile reassembled geometry (drives the stripe slicer)
// ---------------------------------------------------------------------------

/// Full-world tile extents for a zoom.
pub fn full_world_extents(z: u8) -> ForZoom {
    let n = 1i32 << z;
    ForZoom::new(z, 0, 0, n, n, None)
}

/// Render a world-space geometry across `min_zoom..=max_zoom` with a pixel buffer, returning
/// per-tile geometries in tile-local pixel space. Composes scale → extract → slice →
/// reassemble, mirroring `FeatureRenderer`.
pub fn render(
    world: &Geometry<f64>,
    min_zoom: u8,
    max_zoom: u8,
    buffer_px: f64,
) -> BTreeMap<TileId, Vec<Geometry<f64>>> {
    render_with(world, min_zoom, max_zoom, buffer_px, full_world_extents)
}

pub fn render_with<F: Fn(u8) -> ForZoom>(
    world: &Geometry<f64>,
    min_zoom: u8,
    max_zoom: u8,
    buffer_px: f64,
    extents_for: F,
) -> BTreeMap<TileId, Vec<Geometry<f64>>> {
    let buffer = buffer_px / SIZE;
    let mut out: BTreeMap<TileId, Vec<Geometry<f64>>> = BTreeMap::new();
    for z in min_zoom..=max_zoom {
        let scale = f64::from(1u32 << z);
        let scaled = world.map_coords(|c| Coord {
            x: c.x * scale,
            y: c.y * scale,
        });
        let extents = extents_for(z);
        let tiled = slice_scaled(&scaled, buffer, z, &extents);
        for (tile, groups) in tiled.tile_data() {
            out.entry(*tile)
                .or_default()
                .push(reassemble(groups, geom_kind(&scaled)));
        }
        for tile in tiled.filled_tiles() {
            out.entry(tile)
                .or_default()
                .push(Geometry::Polygon(Polygon::new(
                    tile_fill(buffer * SIZE),
                    vec![],
                )));
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Point,
    Line,
    Area,
}

fn geom_kind(g: &Geometry<f64>) -> Kind {
    match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => Kind::Point,
        Geometry::LineString(_) | Geometry::MultiLineString(_) => Kind::Line,
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => Kind::Area,
        Geometry::GeometryCollection(gc) => gc.0.first().map_or(Kind::Area, geom_kind),
        other => panic!("unsupported geometry for render: {other:?}"),
    }
}

/// Dispatch a scaled geometry to the appropriate slicer entry point.
fn slice_scaled(scaled: &Geometry<f64>, buffer: f64, z: u8, extents: &ForZoom) -> TiledGeometry {
    match scaled {
        Geometry::Point(p) => TiledGeometry::slice_points_into_tiles(&[p.0], buffer, z, extents)
            .expect("slice points"),
        Geometry::MultiPoint(mp) => {
            let cs: Vec<Coord<f64>> = mp.0.iter().map(|p| p.0).collect();
            TiledGeometry::slice_points_into_tiles(&cs, buffer, z, extents).expect("slice points")
        }
        _ => {
            let area = geom_kind(scaled) == Kind::Area;
            let groups = extract_groups(scaled);
            TiledGeometry::slice_into_tiles(&groups, buffer, area, z, extents).expect("slice")
        }
    }
}

/// Convert a geometry into slicer input groups, normalizing all rings to CCW (planetiler
/// `extractGroups`). Holes are reversed back to CW during reassembly.
fn extract_groups(geom: &Geometry<f64>) -> CoordSeqGroups {
    fn ccw(mut ls: LineString<f64>) -> LineString<f64> {
        if !is_ccw(&ls) {
            ls.0.reverse();
        }
        ls
    }
    match geom {
        Geometry::LineString(ls) => vec![vec![ls.clone()]],
        Geometry::MultiLineString(mls) => mls.0.iter().map(|l| vec![l.clone()]).collect(),
        Geometry::Polygon(p) => {
            let mut group = vec![ccw(p.exterior().clone())];
            group.extend(p.interiors().iter().cloned().map(ccw));
            vec![group]
        }
        Geometry::MultiPolygon(mp) => {
            mp.0.iter()
                .map(|p| {
                    let mut group = vec![ccw(p.exterior().clone())];
                    group.extend(p.interiors().iter().cloned().map(ccw));
                    group
                })
                .collect()
        }
        Geometry::GeometryCollection(gc) => gc.0.iter().flat_map(extract_groups).collect(),
        other => panic!("cannot extract groups from {other:?}"),
    }
}

/// Reassemble tile-local slicer output into a single geometry (planetiler reassemble*).
fn reassemble(groups: &CoordSeqGroups, kind: Kind) -> Geometry<f64> {
    match kind {
        Kind::Point => {
            let pts: Vec<Point<f64>> = groups
                .iter()
                .flatten()
                .flat_map(|ls| ls.0.iter().map(|c| Point(*c)))
                .collect();
            if pts.len() == 1 {
                Geometry::Point(pts[0])
            } else {
                Geometry::MultiPoint(MultiPoint(pts))
            }
        }
        Kind::Line => {
            let lines: Vec<LineString<f64>> = groups.iter().flatten().cloned().collect();
            if lines.len() == 1 {
                Geometry::LineString(lines.into_iter().next().expect("one line"))
            } else {
                Geometry::MultiLineString(MultiLineString(lines))
            }
        }
        Kind::Area => {
            let polys: Vec<Polygon<f64>> = groups
                .iter()
                .map(|g| {
                    let exterior = g.first().cloned().unwrap_or_else(|| LineString(vec![]));
                    // Reverse inner rings so holes wind opposite the exterior (planetiler
                    // reassemblePolygon), which the overlay union requires to keep them holes.
                    let holes = g
                        .iter()
                        .skip(1)
                        .map(|h| {
                            let mut r = h.clone();
                            r.0.reverse();
                            r
                        })
                        .collect();
                    Polygon::new(exterior, holes)
                })
                .collect();
            // Planetiler runs snapAndFixPolygon (buffer(0)) on every rendered polygon, which
            // repairs self-touches and merges the overlapping antimeridian world-copies. geo's
            // overlay union gives the topologically-equivalent result.
            let unioned = geo::unary_union([&MultiPolygon(polys)]);
            if unioned.0.len() == 1 {
                Geometry::Polygon(unioned.0.into_iter().next().expect("one polygon"))
            } else {
                Geometry::MultiPolygon(unioned)
            }
        }
    }
}
