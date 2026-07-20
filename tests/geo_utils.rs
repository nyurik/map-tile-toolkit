//! Ported from the clip-relevant subset of planetiler `geo/GeoUtilsTest.java`:
//! world-coordinate projection, polygon→linestring, convexity invariance, snap-and-fix
//! repair, and the min-zoom-for-pixel-size heuristic. All target functions are stubbed, so
//! these are **red** until implemented.

#![allow(clippy::pedantic, reason = "ported test coordinates and literals")]

mod support;

use geo::{AffineOps, AffineTransform, Area, Contains};
use geo_types::{Coord, Geometry, LineString, Point};
use map_tile_toolkit::geo_utils::{
    get_world_x, get_world_y, is_convex, lat_lon_to_world, min_zoom_for_pixel_size,
    polygon_to_linestring, snap_and_fix_polygon, world_to_lat_lon,
};
use support::{is_ccw, line_string, new_line_string, new_multi_line_string, new_polygon};

// --- world coordinates ----------------------------------------------------

#[test]
fn world_coords() {
    // (lat, lon, world_x, world_y)
    let cases: &[(f64, f64, f64, f64)] = &[
        (0.0, 0.0, 0.5, 0.5),
        (0.0, -180.0, 0.0, 0.5),
        (0.0, 180.0, 1.0, 0.5),
        (0.0, 180.0 - 1e-7, 1.0, 0.5),
        (45.0, 0.0, 0.5, 0.359_725),
        (-45.0, 0.0, 0.5, 1.0 - 0.359_725),
        (86.0, -198.0, -0.05, -0.033_912_87),
        (-86.0, 198.0, 1.05, 1.033_912_87),
    ];
    for &(lat, lon, wx, wy) in cases {
        approx::assert_abs_diff_eq!(get_world_y(lat), wy, epsilon = 1e-5);
        approx::assert_abs_diff_eq!(get_world_x(lon), wx, epsilon = 1e-5);

        let actual = lat_lon_to_world(Coord { x: lon, y: lat });
        approx::assert_abs_diff_eq!(actual.x, wx, epsilon = 1e-5);
        approx::assert_abs_diff_eq!(actual.y, wy, epsilon = 1e-5);

        let round_tripped = world_to_lat_lon(actual);
        approx::assert_abs_diff_eq!(round_tripped.x, lon, epsilon = 1e-5);
        approx::assert_abs_diff_eq!(round_tripped.y, lat, epsilon = 1e-5);
    }
}

// --- polygon -> linestring ------------------------------------------------

#[test]
fn polygon_to_line_string() {
    let expected = new_line_string(&[0., 0., 1., 0., 1., 1., 0., 1., 0., 0.]);
    assert_eq!(polygon_to_linestring(&support::rectangle_sq(0.0, 1.0)).unwrap(), expected);
}

#[test]
fn multi_polygon_to_line_string() {
    let expected = new_line_string(&[0., 0., 1., 0., 1., 1., 0., 1., 0., 0.]);
    let mp = support::new_multi_polygon(vec![match support::rectangle_sq(0.0, 1.0) {
        Geometry::Polygon(p) => p,
        _ => unreachable!(),
    }]);
    assert_eq!(polygon_to_linestring(&mp).unwrap(), expected);
}

#[test]
fn complex_polygon_to_line_string() {
    let expected = new_multi_line_string(vec![
        line_string(&[0., 0., 3., 0., 3., 3., 0., 3., 0., 0.]),
        line_string(&[1., 1., 2., 1., 2., 2., 1., 2., 1., 1.]),
    ]);
    let poly = support::new_polygon_holes(
        support::rectangle_coord_list_sq(0.0, 3.0),
        vec![support::rectangle_coord_list_sq(1.0, 2.0)],
    );
    assert_eq!(polygon_to_linestring(&poly).unwrap(), expected);
}

// --- convexity (rotation/flip/reverse/scale invariant) --------------------

fn assert_convex(expected: bool, ring: &LineString<f64>) {
    for rotation in [0.0, 90.0, 180.0, 270.0] {
        let (s, c) = f64::to_radians(rotation).sin_cos();
        let rotated = ring.affine_transform(&AffineTransform::new(c, -s, 0.0, s, c, 0.0));
        for flip in [false, true] {
            let flipped = if flip {
                rotated.affine_transform(&AffineTransform::new(-1.0, 0.0, 0.0, 0.0, 1.0, 0.0))
            } else {
                rotated.clone()
            };
            for reverse in [false, true] {
                let mut ring2 = flipped.clone();
                if reverse {
                    ring2.0.reverse();
                }
                for scale in [1.0, 1e-2, 1.0 / f64::from(1i32 << 14) / 4096.0] {
                    let scaled = ring2
                        .affine_transform(&AffineTransform::new(scale, 0.0, 0.0, 0.0, scale, 0.0));
                    assert_eq!(
                        is_convex(&scaled),
                        expected,
                        "rotation={rotation} flip={flip} reverse={reverse} scale={scale}"
                    );
                }
            }
        }
    }
}

#[test]
fn is_convex_triangle() {
    assert_convex(true, &line_string(&[0., 0., 1., 0., 0., 1., 0., 0.]));
}

#[test]
fn is_convex_rectangle() {
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0., 1., 0., 0.]));
}

#[test]
fn barely_convex_rectangle() {
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0.5, 0.5, 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0.4, 0.4, 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0.7, 0.7, 0., 0.]));
}

#[test]
fn concave_rectangle_double_points() {
    assert_convex(true, &line_string(&[0., 0., 0., 0., 1., 0., 1., 1., 0., 1., 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 0., 1., 1., 0., 1., 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 1., 1., 0., 1., 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0., 1., 0., 1., 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0., 1., 0., 0., 0., 0.]));
}

#[test]
fn barely_concave_triangle() {
    assert_convex(false, &line_string(&[0., 0., 1., 0., 1., 1., 0.51, 0.5, 0., 0.]));
}

#[test]
fn allow_very_small_concavity() {
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0.5001, 0.5, 0., 0.]));
    assert_convex(true, &line_string(&[0., 0., 1., 0., 1., 1., 0.5, 0.4999, 0., 0.]));
}

#[test]
fn five_points_concave() {
    assert_convex(false, &line_string(&[0., 0., 0.5, 0.1, 1., 0., 1., 1., 0., 1., 0., 0.]));
    assert_convex(false, &line_string(&[0., 0., 1., 0., 0.9, 0.5, 1., 1., 0., 1., 0., 0.]));
    assert_convex(false, &line_string(&[0., 0., 1., 0., 1., 1., 0.5, 0.9, 0., 1., 0., 0.]));
}

// --- snap-and-fix repair --------------------------------------------------

fn is_polygonal(g: &Geometry<f64>) -> bool {
    matches!(g, Geometry::Polygon(_) | Geometry::MultiPolygon(_))
}

#[test]
fn snap_and_fix_issue_511() {
    let orig = support::load_wkt("tests/fixtures/snap_and_fix_511.wkt");
    let result = snap_and_fix_polygon(&orig).unwrap();
    assert!(is_polygonal(&result));
    approx::assert_abs_diff_eq!(result.unsigned_area(), 3.083_984_375, epsilon = 1e-5);
}

#[test]
fn snap_and_fix_issue_546() {
    let orig = support::new_polygon_holes(
        line_string(&[0., 0., 2., 0., 2., 1., 0., 1., 0., 0.]),
        vec![line_string(&[
            1.190_535_964_444_28, 0.902_969_333_237_7,
            1.064_392_817_777_84, 0.921_844_841_244_82,
            1.427_876_408_888_55, 0.825_773_768_301_6,
            0.945_790_862_222_57, 0.561_750_468_032_2,
            1.190_535_964_444_28, 0.902_969_333_237_7,
        ])],
    );
    let result = snap_and_fix_polygon(&orig).unwrap();
    assert!(is_polygonal(&result));
    let point = Point::new(1.146_020_029_629_65, 0.769_789_692_526_22);
    assert!(!result.contains(&point));
}

#[test]
fn snap_and_fix_issue_546_2() {
    let orig = new_polygon(&[
        1.190_535_964_444_28, 0.902_969_333_237_7,
        1.064_392_817_777_84, 0.921_844_841_244_82,
        1.427_876_408_888_55, 0.825_773_768_301_6,
        0.945_790_862_222_57, 0.561_750_468_032_2,
        1.190_535_964_444_28, 0.902_969_333_237_7,
    ]);
    let result = snap_and_fix_polygon(&orig).unwrap();
    assert!(is_polygonal(&result));
    if let Geometry::Polygon(p) = &result {
        assert!(!is_ccw(p.exterior()), "result must be CW-wound");
    } else {
        panic!("expected a polygon, got {result:?}");
    }
}

// --- min zoom for pixel size ----------------------------------------------

#[test]
fn min_zoom_for_pixel_size_cases() {
    // (world_geometry_size, min_pixel_size, expected_min_zoom)
    let cases: &[(f64, f64, u8)] = &[
        (1.0, 0.0, 0), (1.0, 10.0, 0), (1.0, 255.0, 0),
        (0.5, 0.0, 0), (0.5, 128.0, 0), (0.5, 129.0, 1), (0.5, 256.0, 1),
        (0.25, 0.0, 0), (0.25, 128.0, 1), (0.25, 129.0, 2), (0.25, 256.0, 2),
    ];
    for &(size, min_px, expected) in cases {
        assert_eq!(min_zoom_for_pixel_size(size, min_px), expected, "size={size} min_px={min_px}");
    }
}

#[test]
fn min_zoom_for_pixel_sizes_at_z9_10() {
    assert_eq!(min_zoom_for_pixel_size(3.1 / f64::from(256i32 << 10), 3.0), 10);
    assert_eq!(min_zoom_for_pixel_size(6.1 / f64::from(256i32 << 10), 3.0), 9);
}
