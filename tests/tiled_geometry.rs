//! Ported from planetiler `render/TiledGeometryTest.java`.
//!
//! These exercise the eager stripe slicer: covered-tile enumeration and slicing polygon
//! rings into per-tile coordinate sequences (with buffer, holes, and interior fill). The
//! slicer is currently stubbed, so these tests are **red** until it is implemented.

#![allow(clippy::pedantic, reason = "ported test coordinates and literals")]

mod support;

use std::collections::BTreeSet;

use geo_types::Geometry;
use insta::assert_snapshot;
use map_tile_toolkit::TileId;
use map_tile_toolkit::extents::ForZoom;
use map_tile_toolkit::stripe::{CoordSeqGroups, GeometryError, TiledGeometry};
use support::{
    flip_and_rotate, line_string, new_geometry_collection, new_line_string, new_multi_line_string,
    new_multi_point, new_multi_polygon, new_point, new_polygon_holes, rectangle_coord_list_sq,
    rectangle_sq, round, wkt,
};

const Z14_TILES: i32 = 1 << 14; // 16384
const Z16_TILES: i32 = 1 << 16; // 65536

fn z14_extents() -> ForZoom {
    ForZoom::new(14, 0, 0, Z14_TILES, Z14_TILES, None)
}
fn z16_extents() -> ForZoom {
    ForZoom::new(16, 0, 0, Z16_TILES, Z16_TILES, None)
}

fn covered(geom: &Geometry<f64>, zoom: u8, ext: &ForZoom) -> BTreeSet<TileId> {
    TiledGeometry::get_covered_tiles(geom, zoom, ext)
        .expect("covered tiles")
        .iter()
        .collect()
}

fn tiles(list: &[(u32, u32, u8)]) -> BTreeSet<TileId> {
    list.iter().map(|&(x, y, z)| TileId::new(x, y, z)).collect()
}

// --- covered-tile enumeration ---------------------------------------------

#[test]
fn point_zoom16() {
    let g = TiledGeometry::get_covered_tiles(&new_point(0.5, 0.5), 16, &z16_extents())
        .expect("covered");
    assert!(g.test(0, 0));
    assert!(!g.test(0, 1));
    assert!(!g.test(1, 0));
    assert_eq!(g.iter().collect::<BTreeSet<_>>(), tiles(&[(0, 0, 16)]));

    // high corner at z16 (65535, 65535)
    let n = (Z16_TILES - 1) as u32;
    let corner = f64::from(Z16_TILES) - 0.5;
    let g = TiledGeometry::get_covered_tiles(&new_point(corner, corner), 16, &z16_extents())
        .expect("covered");
    assert!(g.test(n, n));
    assert!(!g.test(n - 1, n));
    assert!(!g.test(n, n - 1));
    assert_eq!(g.iter().collect::<BTreeSet<_>>(), tiles(&[(n, n, 16)]));
}

#[test]
fn point_zoom14() {
    let g = TiledGeometry::get_covered_tiles(&new_point(0.5, 0.5), 14, &z14_extents())
        .expect("covered");
    assert!(g.test(0, 0));
    assert!(!g.test(0, 1));
    assert!(!g.test(1, 0));
    assert!(!g.test(1, 1));
    assert_eq!(g.iter().collect::<BTreeSet<_>>(), tiles(&[(0, 0, 14)]));

    let n = (Z14_TILES - 1) as u32;
    let corner = f64::from(Z14_TILES) - 0.5;
    let g = TiledGeometry::get_covered_tiles(&new_point(corner, corner), 14, &z14_extents())
        .expect("covered");
    assert!(g.test(n, n));
    assert!(!g.test(n - 1, n));
    assert!(!g.test(n, n - 1));
    assert!(!g.test(n - 1, n - 1));
    assert_eq!(g.iter().collect::<BTreeSet<_>>(), tiles(&[(n, n, 14)]));
}

#[test]
fn multi_point() {
    let g = new_multi_point(&[(0.5, 0.5), (2.5, 1.5)]);
    assert_eq!(
        covered(&g, 14, &z14_extents()),
        tiles(&[(0, 0, 14), (2, 1, 14)])
    );
}

#[test]
fn line() {
    let g = new_line_string(&[0.5, 0.5, 1.5, 0.5]);
    assert_eq!(
        covered(&g, 14, &z14_extents()),
        tiles(&[(0, 0, 14), (1, 0, 14)])
    );
}

#[test]
fn line_zoom16() {
    let g = new_line_string(&[0.5, 0.5, 1.5, 0.5]);
    assert_eq!(
        covered(&g, 16, &z16_extents()),
        tiles(&[(0, 0, 16), (1, 0, 16)])
    );
}

#[test]
fn multi_line() {
    let g = new_multi_line_string(vec![
        line_string(&[0.5, 0.5, 1.5, 0.5]),
        line_string(&[3.5, 1.5, 4.5, 1.5]),
    ]);
    assert_eq!(
        covered(&g, 14, &z14_extents()),
        tiles(&[(0, 0, 14), (1, 0, 14), (3, 1, 14), (4, 1, 14)])
    );
}

#[test]
fn polygon_with_hole_skips_interior_tile() {
    // Outer 25.5..27.5, hole 25.9..27.1: the center tile (26,26) is removed by the hole.
    let g = new_polygon_holes(
        rectangle_coord_list_sq(25.5, 27.5),
        vec![rectangle_coord_list_sq(25.9, 27.1)],
    );
    assert_eq!(
        covered(&g, 14, &z14_extents()),
        tiles(&[
            (25, 25, 14),
            (26, 25, 14),
            (27, 25, 14),
            (25, 26, 14),
            /* (26,26) skipped */ (27, 26, 14),
            (25, 27, 14),
            (26, 27, 14),
            (27, 27, 14),
        ])
    );
}

#[test]
fn polygon_zoom16() {
    let g = rectangle_sq(0.1, 1.9);
    assert_eq!(
        covered(&g, 16, &z16_extents()),
        tiles(&[(0, 0, 16), (0, 1, 16), (1, 0, 16), (1, 1, 16)])
    );
}

#[test]
fn multi_polygon() {
    let g = new_multi_polygon(vec![
        match rectangle_sq(25.5, 26.5) {
            Geometry::Polygon(p) => p,
            _ => unreachable!(),
        },
        match rectangle_sq(30.1, 30.9) {
            Geometry::Polygon(p) => p,
            _ => unreachable!(),
        },
    ]);
    assert_eq!(
        covered(&g, 14, &z14_extents()),
        tiles(&[
            (25, 25, 14),
            (25, 26, 14),
            (26, 25, 14),
            (26, 26, 14),
            (30, 30, 14)
        ])
    );
}

#[test]
fn covered_tiles_edge_cases() {
    // Rectangles at the two opposite world corners; only those corners must emit.
    let n = f64::from(Z16_TILES);
    let g = new_geometry_collection(vec![rectangle_sq(0.0, 10.0), rectangle_sq(n - 10.0, n)]);
    let set = covered(&g, 16, &z16_extents());
    let last = (Z16_TILES - 1) as u32;
    assert!(set.contains(&TileId::new(0, 0, 16)), "top-left");
    assert!(set.contains(&TileId::new(last, last, 16)), "bottom-right");
    assert!(!set.contains(&TileId::new(last, 0, 16)), "top-right");
    assert!(!set.contains(&TileId::new(0, last, 16)), "bottom-left");
}

#[test]
fn empty() {
    let g = new_geometry_collection(vec![]);
    assert!(covered(&g, 14, &z14_extents()).is_empty());
}

#[test]
fn covered_tiles_iterator_exhausts() {
    let g = TiledGeometry::get_covered_tiles(&new_point(0.5, 0.5), 14, &z14_extents())
        .expect("covered");
    let mut it = g.iter();
    assert!(it.next().is_some());
    assert!(it.next().is_none());
}

#[test]
fn geometry_collection() {
    let g = new_geometry_collection(vec![
        rectangle_sq(0.1, 0.9),
        new_point(1.5, 1.5),
        new_geometry_collection(vec![new_line_string(&[3.5, 10.5, 4.5, 10.5])]),
    ]);
    assert_eq!(
        covered(&g, 14, &z14_extents()),
        tiles(&[(0, 0, 14), (1, 1, 14), (3, 10, 14), (4, 10, 14)])
    );
}

// --- x/y packing ----------------------------------------------------------

#[test]
fn encode_decode() {
    let cases: &[(u8, u32, u32)] = &[
        (0, 0, 0),
        (2, 0, 0),
        (2, 3, 3),
        (3, 7, 6),
        (3, 7, 7),
        (15, 0, 0),
        (15, 32767, 0),
        (15, 0, 32767),
        (15, 32767, 32767),
        (16, 0, 0),
        (16, 1, 2),
        (16, 65535, 0),
        (16, 65535, 65535),
        (16, 0, 65535),
    ];
    for &(z, x, y) in cases {
        let max = 1u64 << z;
        let encoded = TiledGeometry::encode(max, x, y);
        assert_eq!(
            TiledGeometry::decode(max, u64::from(encoded as u32), z),
            TileId::new(x, y, z)
        );
    }
}

// --- slice-into-tiles (clipping core) -------------------------------------

fn test_render(groups: &CoordSeqGroups) -> TiledGeometry {
    TiledGeometry::slice_into_tiles(
        groups,
        0.0,
        true,
        14,
        &ForZoom::new(14, -10, -10, Z14_TILES, Z14_TILES, None),
    )
    .expect("slice")
}

const ROT_ONLY: &[(i32, bool, bool)] = &[
    (0, false, false),
    (90, false, false),
    (180, false, false),
    (270, false, false),
];
const ROT_FLIP: &[(i32, bool, bool)] = &[
    (0, false, false),
    (90, false, false),
    (180, false, false),
    (270, false, false),
    (0, true, false),
    (0, false, true),
    (0, true, true),
];

#[test]
fn only_hole_touches_other_cell_bottom_errors() {
    // A hole that falls outside the shell and touches a neighbor tile must be rejected.
    for &(degrees, _, _) in ROT_ONLY {
        let mut outer = line_string(&[1.5, 1.5, 1.6, 1.5, 1.5, 1.6, 1.5, 1.5]);
        let mut inner = line_string(&[1.4, 1.8, 1.6, 1.8, 1.5, 2.0, 1.4, 1.8]);
        support::rotate(&mut outer, 1.5, 1.5, degrees);
        support::rotate(&mut inner, 1.5, 1.5, degrees);
        let groups: CoordSeqGroups = vec![vec![outer, inner]];
        let result = TiledGeometry::slice_into_tiles(
            &groups,
            0.1,
            true,
            11,
            &ForZoom::new(11, 0, 0, 1 << 11, 1 << 11, None),
        );
        assert!(
            matches!(result, Err(GeometryError::BadPolygonFill(_))),
            "degrees={degrees}: expected BadPolygonFill"
        );
    }
}

#[test]
fn overlapping_holes() {
    for &(degrees, flip_x, flip_y) in ROT_FLIP {
        let mut outer = line_string(&[1., 1., 10., 1., 10., 10., 1., 10., 1., 1.]);
        let mut inner1 = line_string(&[2., 2., 2., 9., 9., 9., 3., 5., 9., 2., 2., 2.]);
        let mut inner2 = line_string(&[9., 3., 9., 8., 4., 5., 9., 3.]);
        for r in [&mut outer, &mut inner1, &mut inner2] {
            flip_and_rotate(r, 6.0, 6.0, flip_x, flip_y, degrees);
        }
        test_render(&vec![vec![outer.clone(), inner1.clone()]]);
        test_render(&vec![vec![outer.clone(), inner2.clone()]]);
        test_render(&vec![vec![outer.clone(), inner1.clone(), inner2.clone()]]);
        let result = test_render(&vec![vec![outer, inner2, inner1]]);
        if degrees == 0 && !flip_x && !flip_y {
            let c = result.covered_tiles();
            assert!(!c.test(7, 4));
            assert!(!c.test(3, 3));
            assert!(c.test(1, 1));
            assert!(c.test(9, 9));
        }
    }
}

#[test]
fn inside_complex_hole() {
    for &(degrees, flip_x, flip_y) in ROT_FLIP {
        let mut outer = line_string(&[1., 1., 10., 1., 10., 10., 1., 10., 1., 1.]);
        let mut inner1 = line_string(&[
            6.5, 1.5, 2., 2., 2., 9., 9., 9., 9., 2., 4.6, 2., 8., 8., 3., 8., 4., 2., 6.5, 1.5,
        ]);
        let mut inner2 = line_string(&[5.5, 6.5, 5.5, 6.6, 5.6, 6.6, 5.5, 6.5]);
        for r in [&mut outer, &mut inner1, &mut inner2] {
            flip_and_rotate(r, 6.0, 6.0, flip_x, flip_y, degrees);
        }
        // Winding pre-conditions before slicing.
        assert!(support::is_ccw(&outer));
        assert!(!support::is_ccw(&inner1));
        assert!(!support::is_ccw(&inner2));

        test_render(&vec![vec![outer.clone(), inner1.clone()]]);
        test_render(&vec![vec![outer.clone(), inner2.clone()]]);
        test_render(&vec![vec![outer.clone(), inner1.clone(), inner2.clone()]]);
        let result = test_render(&vec![vec![outer, inner2, inner1]]);
        if degrees == 0 && !flip_x && !flip_y {
            let filled: BTreeSet<TileId> = result.filled_tiles().collect();
            assert!(filled.contains(&TileId::new(5, 5, 14)));
            assert!(filled.contains(&TileId::new(4, 6, 14)));
            assert!(!filled.contains(&TileId::new(5, 6, 14)));
        }
    }
}

#[test]
fn side_of_hole_intercepted() {
    for &(degrees, flip_x, flip_y) in ROT_FLIP {
        let mut outer = line_string(&[1., 1., 10., 1., 10., 10., 1., 10., 1., 1.]);
        let mut inner1 = line_string(&[
            2., 2., 2., 9., 9., 9., 3., 5., 9., 2., 9., 4.2, 7.5, 4.2, 7.5, 4.8, 9.5, 4.8, 9.5,
            1.8, 2., 2.,
        ]);
        flip_and_rotate(&mut outer, 5.0, 5.0, flip_x, flip_y, degrees);
        flip_and_rotate(&mut inner1, 5.0, 5.0, flip_x, flip_y, degrees);
        let result = test_render(&vec![vec![outer, inner1]]);
        if degrees == 0 && !flip_x && !flip_y {
            let filled: BTreeSet<TileId> = result.filled_tiles().collect();
            assert!(!filled.contains(&TileId::new(7, 4, 14)));

            // Exact clipped rings for tile (7,4,14): a full-tile square plus the clipped hole.
            let groups = result
                .tile_data()
                .get(&TileId::new(7, 4, 14))
                .expect("tile (7,4)");
            let poly = {
                let exterior = groups[0][0].clone();
                let holes = groups[0].iter().skip(1).cloned().collect();
                Geometry::Polygon(geo_types::Polygon::new(exterior, holes))
            };
            assert_snapshot!(
                wkt(&round(&poly, 10.0)),
                @"POLYGON((0 0,256 0,256 256,0 256,0 0),(0 256,0 0,256 0,256 51.2,128 51.2,128 204.8,256 204.8,256 0,0 0,0 256))"
            );
        }
    }
}
