//! Ported from the geometry-clipping subset of planetiler `render/FeatureRendererTest.java`.
//!
//! Each test renders a world-space geometry through the (stubbed) slicer via
//! [`support::render`] and checks the per-tile output. Coordinates and expected values are
//! planetiler's. These are **red** until the slicer is implemented.
//!
//! ## Coverage of planetiler `FeatureRendererTest`
//!
//! Every geometry-clipping test is ported. A few cases produce different (but valid) results
//! and are annotated with `DIVERGENCE FROM PLANETILER` where they assert geo's actual output.
//!
//! Intentionally NOT ported, because they exercise pipeline stages other than tile clipping
//! (and would need machinery `geo` does not provide):
//! * simplification: `testSimplifyLine`, `testSimplifyMakesOutputInvalid`;
//! * min-size filtering: `testLimit*`, `testOmitsPolygonUnderMinSize`,
//!   `testIncludesPolygonUnderMinTolerance`, `testUses1pxMinAreaAtMaxZoom`;
//! * encode-grid rounding: `testDuplicatePointsRemovedAfterRounding`,
//!   `testLineStringCollapsesToPointWithRounding`, `testRoundingCollapsesPolygonToLine`,
//!   `testRoundingMakesOutputInvalid`;
//! * label grids: `testLabelGrid*`, `testMultipointWithLabelGridSplits`, `testWrapLabelGrid`;
//! * linear ranges / geometry pipeline: `testLinearRange*`, `testGeometryPipeline*`;
//! * `testSpiral`: its input is built with JTS `buffer()`, which `geo` lacks;
//! * `testEmitPointsRespectShape`: needs planetiler's `bottomrightearth.poly` resource — the
//!   shape-clip path itself is covered by `tile_extents::shape`.

#![allow(clippy::pedantic, reason = "ported test coordinates and literals")]

mod support;

use std::collections::BTreeMap;

use geo::{BooleanOps, MapCoords};
use geo_types::{Coord, Geometry, MultiPolygon, Polygon, Rect};
use map_tile_toolkit::TileId;
use map_tile_toolkit::extents::{ForZoom, TileExtents};
use map_tile_toolkit::geo_utils::{get_world_x, get_world_y};
use map_tile_toolkit::stripe::TiledGeometry;
use support::{
    assert_tiles, line_string, new_line_string, new_multi_line_string, new_multi_point,
    new_multi_polygon, new_point, new_polygon, new_polygon_holes, rectangle,
    rectangle_coord_list_sq, rectangle_sq, render, rotate_tile, rotate_world, tile_bottom,
    tile_bottom_left, tile_bottom_right, tile_fill, tile_left, tile_right, tile_top, tile_top_left,
    tile_top_right,
};

const Z14_TILES: i32 = 1 << 14;
const Z14_WIDTH: f64 = 1.0 / (1 << 14) as f64;
const Z14_PX: f64 = Z14_WIDTH / 256.0;

/// World coordinate from a z14 tile-pixel offset around the tile center.
fn wc(v: f64) -> f64 {
    0.5 + Z14_PX * v
}
/// Map a flat pixel-offset list into world coordinates.
fn wcs(vals: &[f64]) -> Vec<f64> {
    vals.iter().map(|&v| wc(v)).collect()
}
/// Tile at `(center + dx, center + dy)` for z14 (`center = 8192`).
fn c(dx: i32, dy: i32) -> TileId {
    TileId::new((Z14_TILES / 2 + dx) as u32, (Z14_TILES / 2 + dy) as u32, 14)
}
fn t(x: u32, y: u32, z: u8) -> TileId {
    TileId::new(x, y, z)
}

// ===========================================================================
// POINTS
// ===========================================================================

#[test]
fn repeat_single_point_neighboring_tiles() {
    let g = new_point(0.5 + 1.0 / 512.0, 0.5 + 1.0 / 512.0);
    assert_tiles(
        vec![
            (t(0, 0, 0), vec![new_point(128.5, 128.5)]),
            (t(0, 0, 1), vec![new_point(257.0, 257.0)]),
            (t(1, 0, 1), vec![new_point(1.0, 257.0)]),
            (t(0, 1, 1), vec![new_point(257.0, 1.0)]),
            (t(1, 1, 1), vec![new_point(1.0, 1.0)]),
        ],
        &render(&g, 0, 1, 2.0),
    );
}

#[test]
fn repeat_single_point_neighboring_tiles_buffer0() {
    let g = new_point(0.5, 0.5);
    assert_tiles(
        vec![
            (t(0, 0, 1), vec![new_point(256.0, 256.0)]),
            (t(1, 0, 1), vec![new_point(0.0, 256.0)]),
            (t(0, 1, 1), vec![new_point(256.0, 0.0)]),
            (t(1, 1, 1), vec![new_point(0.0, 0.0)]),
        ],
        &render(&g, 1, 1, 0.0),
    );
}

#[test]
fn z0_full_tile_buffer() {
    let g = new_point(0.25, 0.25);
    assert_tiles(
        vec![
            (
                t(0, 0, 0),
                vec![new_multi_point(&[
                    (-192.0, 64.0),
                    (64.0, 64.0),
                    (320.0, 64.0),
                ])],
            ),
            (t(0, 0, 1), vec![new_point(128.0, 128.0)]),
            (
                t(1, 0, 1),
                vec![new_multi_point(&[(-128.0, 128.0), (384.0, 128.0)])],
            ),
            (t(0, 1, 1), vec![new_point(128.0, -128.0)]),
            (
                t(1, 1, 1),
                vec![new_multi_point(&[(-128.0, -128.0), (384.0, -128.0)])],
            ),
        ],
        &render(&g, 0, 1, 256.0),
    );
}

#[test]
fn multipoint_no_label_grid() {
    let g = new_multi_point(&[(0.25, 0.25), (0.25 + 1.0 / 256.0, 0.25 + 1.0 / 256.0)]);
    assert_tiles(
        vec![
            (
                t(0, 0, 0),
                vec![new_multi_point(&[(64.0, 64.0), (65.0, 65.0)])],
            ),
            (
                t(0, 0, 1),
                vec![new_multi_point(&[(128.0, 128.0), (130.0, 130.0)])],
            ),
        ],
        &render(&g, 0, 1, 4.0),
    );
}

// ===========================================================================
// LINES
// ===========================================================================

#[test]
fn split_line_single_tile() {
    let h = Z14_WIDTH;
    let g = new_line_string(&[
        0.5 + h / 4.0,
        0.5 + h / 4.0,
        0.5 + h * 3.0 / 4.0,
        0.5 + h * 3.0 / 4.0,
    ]);
    assert_tiles(
        vec![(c(0, 0), vec![new_line_string(&[64., 64., 192., 192.])])],
        &render(&g, 14, 14, 8.0),
    );
}

#[test]
fn split_line_touching_neighboring_tile() {
    let h = Z14_WIDTH;
    let end = 0.5 + Z14_WIDTH * (256.0 - 8.0) / 256.0;
    let g = new_line_string(&[0.5 + h / 4.0, 0.5 + h / 4.0, end, end]);
    // Only a single touching point in the neighbor tile, so it is excluded.
    assert_tiles(
        vec![(c(0, 0), vec![new_line_string(&[64., 64., 248., 248.])])],
        &render(&g, 14, 14, 8.0),
    );
}

#[test]
fn split_line_entering_neighboring_tile_boundary() {
    let h = Z14_WIDTH;
    let end = 0.5 + Z14_WIDTH * (256.0 - 7.0) / 256.0;
    let g = new_line_string(&[0.5 + h / 4.0, 0.5 + h / 4.0, end, end]);
    assert_tiles(
        vec![
            (c(0, 0), vec![new_line_string(&[64., 64., 249., 249.])]),
            (c(1, 0), vec![new_line_string(&[-8., 248., -7., 249.])]),
            (c(0, 1), vec![new_line_string(&[248., -8., 249., -7.])]),
            (c(1, 1), vec![new_line_string(&[-8., -8., -7., -7.])]),
        ],
        &render(&g, 14, 14, 8.0),
    );
}

#[test]
fn three_point_line() {
    let w = Z14_WIDTH;
    let g = new_line_string(&[
        0.5 + w / 2.0,
        0.5 + w / 2.0,
        0.5 + 3.0 * w / 2.0,
        0.5 + w / 2.0,
        0.5 + 3.0 * w / 2.0,
        0.5 + 3.0 * w / 2.0,
    ]);
    assert_tiles(
        vec![
            (c(0, 0), vec![new_line_string(&[128., 128., 264., 128.])]),
            (
                c(1, 0),
                vec![new_line_string(&[-8., 128., 128., 128., 128., 264.])],
            ),
            (c(1, 1), vec![new_line_string(&[128., -8., 128., 128.])]),
        ],
        &render(&g, 14, 14, 8.0),
    );
}

#[test]
fn self_intersecting_line_ok() {
    let g = new_line_string(&wcs(&[10., 10., 20., 20., 10., 20., 20., 10., 10., 10.]));
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_line_string(&[
                10., 10., 20., 20., 10., 20., 20., 10., 10., 10.,
            ])],
        )],
        &render(&g, 14, 14, 4.0),
    );
}

#[test]
fn line_wrap() {
    let g = new_line_string(&[-1.0 / 256.0, -1.0 / 256.0, 257.0 / 256.0, 257.0 / 256.0]);
    assert_tiles(
        vec![
            (
                t(0, 0, 0),
                vec![new_multi_line_string(vec![
                    line_string(&[-1., -1., 257., 257.]),
                    line_string(&[-4., 252., 1., 257.]),
                    line_string(&[255., -1., 260., 4.]),
                ])],
            ),
            (t(0, 0, 1), vec![new_line_string(&[-2., -2., 260., 260.])]),
            (
                t(1, 0, 1),
                vec![new_multi_line_string(vec![
                    line_string(&[-4., 252., 4., 260.]),
                    line_string(&[254., -2., 260., 4.]),
                ])],
            ),
            (
                t(0, 1, 1),
                vec![new_multi_line_string(vec![
                    line_string(&[252., -4., 260., 4.]),
                    line_string(&[-4., 252., 2., 258.]),
                ])],
            ),
            (t(1, 1, 1), vec![new_line_string(&[-4., -4., 258., 258.])]),
        ],
        &render(&g, 0, 1, 4.0),
    );
}

// ===========================================================================
// POLYGONS
// ===========================================================================

#[test]
fn simple_triangle_ccw() {
    let g = new_polygon(&wcs(&[10., 10., 20., 10., 10., 20., 10., 10.]));
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_polygon(&[10., 10., 20., 10., 10., 20., 10., 10.])],
        )],
        &render(&g, 14, 14, 0.0),
    );
}

#[test]
fn simple_triangle_cw() {
    let g = new_polygon(&wcs(&[10., 10., 10., 20., 20., 10., 10., 10.]));
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_polygon(&[10., 10., 10., 20., 20., 10., 10., 10.])],
        )],
        &render(&g, 14, 14, 0.0),
    );
}

#[test]
fn triangle_touching_neighboring_tile_does_not_emit() {
    let g = new_polygon(&wcs(&[10., 10., 256., 10., 10., 20., 10., 10.]));
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_polygon(&[10., 10., 256., 10., 10., 20., 10., 10.])],
        )],
        &render(&g, 14, 14, 0.0),
    );
}

#[test]
fn rectangle_touching_neighboring_tiles_does_not_emit() {
    // (x1, x2, y1, y2) pixel bounds; each must render into the center tile only.
    let cases: &[(f64, f64, f64, f64)] = &[
        (0., 256., 0., 256.),
        (0., 10., 0., 10.),
        (5., 10., 0., 10.),
        (250., 256., 0., 10.),
        (250., 256., 0., 256.),
        (250., 256., 10., 250.),
        (250., 256., 250., 256.),
        (0., 256., 250., 256.),
        (240., 250., 250., 256.),
        (0., 10., 250., 256.),
        (0., 10., 0., 256.),
        (0., 10., 240., 250.),
    ];
    for &(x1, x2, y1, y2) in cases {
        let g = rectangle(wc(x1), wc(y1), wc(x2), wc(y2));
        assert_tiles(
            vec![(c(0, 0), vec![rectangle(x1, y1, x2, y2)])],
            &render(&g, 14, 14, 0.0),
        );
    }
}

#[test]
fn overlap_tile_horizontal() {
    let g = rectangle(wc(10.), wc(10.), wc(258.), wc(20.));
    assert_tiles(
        vec![
            (c(0, 0), vec![rectangle(10., 10., 257., 20.)]),
            (c(1, 0), vec![rectangle(-1., 10., 2., 20.)]),
        ],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn overlap_tile_vertical() {
    let g = rectangle(wc(10.), wc(10.), wc(20.), wc(258.));
    assert_tiles(
        vec![
            (c(0, 0), vec![rectangle(10., 10., 20., 257.)]),
            (c(0, 1), vec![rectangle(10., -1., 20., 2.)]),
        ],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn overlap_tile_corner() {
    let g = rectangle(wc(-10.), wc(-10.), wc(10.), wc(10.));
    assert_tiles(
        vec![
            (c(-1, -1), vec![rectangle_sq(246., 257.)]),
            (c(0, -1), vec![rectangle(-1., 246., 10., 257.)]),
            (c(-1, 0), vec![rectangle(246., -1., 257., 10.)]),
            (c(0, 0), vec![rectangle_sq(-1., 10.)]),
        ],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn fill() {
    let g = rectangle_sq(0.5 - Z14_WIDTH / 2.0, 0.5 + 3.0 * Z14_WIDTH / 2.0);
    assert_tiles(
        vec![
            (c(-1, -1), vec![tile_bottom_right(1.0)]),
            (c(0, -1), vec![tile_bottom(1.0)]),
            (c(1, -1), vec![tile_bottom_left(1.0)]),
            (c(-1, 0), vec![tile_right(1.0)]),
            (
                c(0, 0),
                vec![Geometry::Polygon(Polygon::new(tile_fill(1.0), vec![]))],
            ),
            (c(1, 0), vec![tile_left(1.0)]),
            (c(-1, 1), vec![tile_top_right(1.0)]),
            (c(0, 1), vec![tile_top(1.0)]),
            (c(1, 1), vec![tile_top_left(1.0)]),
        ],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn complex_polygon() {
    let g = new_polygon_holes(
        rectangle_coord_list_sq(wc(1.), wc(255.)),
        vec![rectangle_coord_list_sq(wc(10.), wc(250.))],
    );
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_polygon_holes(
                rectangle_coord_list_sq(1., 255.),
                vec![rectangle_coord_list_sq(10., 250.)],
            )],
        )],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn complex_polygon_hole_infers_outer_fill() {
    let g = new_polygon_holes(
        rectangle_coord_list_sq(0.5 - Z14_WIDTH / 2.0, 0.5 + 3.0 * Z14_WIDTH / 2.0),
        vec![rectangle_coord_list_sq(wc(10.), wc(250.))],
    );
    let center = new_polygon_holes(tile_fill(1.0), vec![rectangle_coord_list_sq(10., 250.)]);
    assert_tiles(
        vec![
            (c(-1, -1), vec![tile_bottom_right(1.0)]),
            (c(0, -1), vec![tile_bottom(1.0)]),
            (c(1, -1), vec![tile_bottom_left(1.0)]),
            (c(-1, 0), vec![tile_right(1.0)]),
            (c(0, 0), vec![center]),
            (c(1, 0), vec![tile_left(1.0)]),
            (c(-1, 1), vec![tile_top_right(1.0)]),
            (c(0, 1), vec![tile_top(1.0)]),
            (c(1, 1), vec![tile_top_left(1.0)]),
        ],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn complex_polygon_hole_blocks_fill() {
    let g = new_polygon_holes(
        rectangle_coord_list_sq(0.5 - Z14_WIDTH / 2.0, 0.5 + 3.0 * Z14_WIDTH / 2.0),
        vec![rectangle_coord_list_sq(wc(-10.), wc(260.))],
    );
    let rendered = render(&g, 14, 14, 1.0);
    // Center tile is entirely inside the hole → not emitted.
    assert!(
        !rendered.contains_key(&c(0, 0)),
        "center tile must be absent (inside hole)"
    );
    // Notch taken out of the bottom-right of the top-left tile.
    let notch = new_polygon(&[
        128., 128., 257., 128., 257., 246., 246., 246., 246., 257., 128., 257., 128., 128.,
    ]);
    assert_tiles_subset(&rendered, c(-1, -1), &notch);
    // 4px taken out of the top of the tile below center.
    assert_tiles_subset(&rendered, c(0, 1), &rectangle(-1., 4., 257., 128.));
}

/// Assert one tile of a rendered map contains exactly one geometry topo-equal to `expected`.
fn assert_tiles_subset(
    rendered: &BTreeMap<TileId, Vec<Geometry<f64>>>,
    tile: TileId,
    expected: &Geometry<f64>,
) {
    assert_tiles(vec![(tile, vec![expected.clone()])], &{
        let mut m = BTreeMap::new();
        m.insert(tile, rendered.get(&tile).cloned().unwrap_or_default());
        m
    });
}

#[test]
fn multipolygon() {
    let g = new_multi_polygon(vec![
        poly(rectangle(wc(10.), wc(10.), wc(20.), wc(20.))),
        poly(rectangle(wc(30.), wc(30.), wc(40.), wc(40.))),
    ]);
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_multi_polygon(vec![
                poly(rectangle_sq(10., 20.)),
                poly(rectangle_sq(30., 40.)),
            ])],
        )],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn fix_invalid_input_geometry() {
    // Bow-tie (self-intersecting) polygon must be repaired to a valid shape.
    let g = new_polygon(&wcs(&[10., 10., 30., 10., 10., 20., 20., 20., 10., 10.]));
    // DIVERGENCE FROM PLANETILER: planetiler's repaired apex is (16.6875, 16.6875), a JTS
    // `buffer(0)` snapping artifact. geo's overlay repair splits the bow-tie at the
    // geometrically-exact self-intersection point (16.6667, 16.6667). Both are valid single
    // triangles; we assert geo's exact apex so the test keeps running.
    let apex = 50.0 / 3.0; // 16.6666… — geo's exact intersection point
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_polygon(&[10., 10., 30., 10., apex, apex, 10., 10.])],
        )],
        &render(&g, 14, 14, 1.0),
    );
}

#[test]
fn polygon_wrap() {
    let g = rectangle(-1.0 / 256.0, -1.0 / 256.0, 257.0 / 256.0, 1.0 / 256.0);
    assert_tiles(
        vec![
            (t(0, 0, 0), vec![rectangle(-4., -1., 260., 1.)]),
            (t(0, 0, 1), vec![rectangle(-4., -2., 260., 2.)]),
            (t(1, 0, 1), vec![rectangle(-4., -2., 260., 2.)]),
        ],
        &render(&g, 0, 1, 4.0),
    );
}

// ===========================================================================
// Clip-vs-intersection oracle (planetiler testClipWithRotation / testSpiral)
// ===========================================================================

fn poly(g: Geometry<f64>) -> Polygon<f64> {
    match g {
        Geometry::Polygon(p) => p,
        _ => unreachable!(),
    }
}

/// The rendered clip of a polygon (in tile-pixel coords) must be topologically equal to the
/// `geo` intersection with the buffered tile rectangle `(-4..260)`, at any rotation. This is
/// planetiler's `testClipWithRotation` oracle, using `geo::BooleanOps::intersection` as the
/// independent reference.
fn clip_with_rotation(rotation: i32, input_tile: &Geometry<f64>) {
    // Scale pixel coords into world space, then rotate about the world tile center.
    let world = input_tile.map_coords(|p| Coord {
        x: 0.5 + p.x / 256.0 / f64::from(Z14_TILES),
        y: 0.5 + p.y / 256.0 / f64::from(Z14_TILES),
    });
    let center = 0.5 + Z14_WIDTH / 2.0;
    let rotated = rotate_world(&world, center, center, -rotation);

    // Oracle: intersect the pixel-space polygon with the tile buffered by 4px, then rotate it
    // in tile space the same way.
    let tile_rect = MultiPolygon(vec![poly(rectangle_sq(-4.0, 260.0))]);
    let oracle = poly(input_tile.clone()).intersection(&tile_rect);
    let expected = rotate_tile(&Geometry::MultiPolygon(oracle), -rotation);

    let rendered = render(&rotated, 14, 14, 4.0);
    let got = &rendered.get(&c(0, 0)).expect("center tile present")[0];
    support::assert_same_normalized(&expected, got);
}

const ROTATIONS: [i32; 4] = [0, 90, 180, -90];

#[test]
fn back_and_forths_outside_tile() {
    let input = new_polygon(&[
        300., -10., 310., 300., 320., -10., 330., 300., 340., 400., 128., 400., 128., 128., 128.,
        -10., 300., -10.,
    ]);
    for r in ROTATIONS {
        clip_with_rotation(r, &input);
    }
}

#[test]
fn replay_edges_outer_poly() {
    let input = new_polygon(&[
        130., -10., 270., -10., 270., 270., -10., 270., -10., -10., 120., -10., 120., 10., 130.,
        10., 130., -10.,
    ]);
    for r in ROTATIONS {
        clip_with_rotation(r, &input);
    }
}

#[test]
fn replay_edges_inner_poly() {
    let mut inner = line_string(&[
        130., -10., 270., -10., 270., 270., -10., 270., -10., -10., 120., -10., 120., 10., 130.,
        10., 130., -10.,
    ]);
    inner.0.reverse();
    let input = new_polygon_holes(rectangle_coord_list_sq(-20., 300.), vec![inner]);
    for r in ROTATIONS {
        clip_with_rotation(r, &input);
    }
}

// ===========================================================================
// Additional clipping cases (points, nested/overlapping polygons, world fill,
// bounds clipping, dateline wrapping)
// ===========================================================================

#[test]
fn empty_geometry() {
    let g = new_multi_point(&[]);
    assert_tiles(vec![], &render(&g, 14, 14, 0.0));
}

#[test]
fn single_point() {
    let g = new_point(0.5 + Z14_WIDTH / 2.0, 0.5 + Z14_WIDTH / 2.0);
    assert_tiles(
        vec![(c(0, 0), vec![new_point(128., 128.)])],
        &render(&g, 14, 14, 0.0),
    );
}

#[test]
fn triangle_touching_neighboring_tile_below_does_not_emit() {
    let g = new_polygon(&wcs(&[10., 10., 20., 10., 10., 256., 10., 10.]));
    assert_tiles(
        vec![(
            c(0, 0),
            vec![new_polygon(&[10., 10., 20., 10., 10., 256., 10., 10.])],
        )],
        &render(&g, 14, 14, 0.0),
    );
}

#[test]
fn nested_multipolygon() {
    let outer = new_polygon_holes(
        rectangle_coord_list_sq(wc(10.), wc(200.)),
        vec![rectangle_coord_list_sq(wc(20.), wc(190.))],
    );
    let g = new_multi_polygon(vec![poly(outer), poly(rectangle_sq(wc(30.), wc(180.)))]);
    let expected = new_multi_polygon(vec![
        poly(new_polygon_holes(
            rectangle_coord_list_sq(10., 200.),
            vec![rectangle_coord_list_sq(20., 190.)],
        )),
        poly(rectangle_sq(30., 180.)),
    ]);
    assert_tiles(vec![(c(0, 0), vec![expected])], &render(&g, 14, 14, 1.0));
}

#[test]
fn nested_multipolygon_fill() {
    let outer = new_polygon_holes(
        rectangle_coord_list_sq(wc(-30.), wc(286.)),
        vec![rectangle_coord_list_sq(wc(-20.), wc(276.))],
    );
    let g = new_multi_polygon(vec![poly(outer), poly(rectangle_sq(wc(-10.), wc(266.)))]);
    // Center tile is a solid fill.
    assert_tiles_subset(&render(&g, 14, 14, 1.0), c(0, 0), &rectangle_sq(-1., 257.));
}

#[test]
fn nested_multipolygon_infers_outer_fill() {
    let outer = new_polygon_holes(
        rectangle_coord_list_sq(wc(-30.), wc(286.)),
        vec![rectangle_coord_list_sq(wc(-20.), wc(276.))],
    );
    let inner = new_polygon_holes(
        rectangle_coord_list_sq(wc(-10.), wc(266.)),
        vec![rectangle_coord_list_sq(wc(10.), wc(246.))],
    );
    let g = new_multi_polygon(vec![poly(outer), poly(inner)]);
    let expected = new_polygon_holes(
        rectangle_coord_list_sq(-1., 257.),
        vec![rectangle_coord_list_sq(10., 246.)],
    );
    assert_tiles_subset(&render(&g, 14, 14, 1.0), c(0, 0), &expected);
}

#[test]
fn nested_multipolygon_cancels_out_inner_fill() {
    let outer = new_polygon_holes(
        rectangle_coord_list_sq(wc(-30.), wc(286.)),
        vec![rectangle_coord_list_sq(wc(-20.), wc(276.))],
    );
    let inner = new_polygon_holes(
        rectangle_coord_list_sq(wc(-10.), wc(266.)),
        vec![rectangle_coord_list_sq(wc(-5.), wc(261.))],
    );
    let g = new_multi_polygon(vec![poly(outer), poly(inner)]);
    assert!(
        !render(&g, 14, 14, 1.0).contains_key(&c(0, 0)),
        "center fill cancels out"
    );
}

#[test]
fn overlapping_multipolygon() {
    let g = new_multi_polygon(vec![
        poly(rectangle(10. / 256., 10. / 256., 30. / 256., 30. / 256.)),
        poly(rectangle(20. / 256., 20. / 256., 40. / 256., 40. / 256.)),
    ]);
    let expected = new_polygon(&[
        10., 10., 30., 10., 30., 20., 40., 20., 40., 40., 20., 40., 20., 30., 10., 30., 10., 10.,
    ]);
    assert_tiles(vec![(t(0, 0, 0), vec![expected])], &render(&g, 0, 0, 4.0));
}

#[test]
fn overlapping_multipolygon_side_by_side() {
    let g = new_multi_polygon(vec![
        poly(rectangle(10. / 256., 10. / 256., 20. / 256., 20. / 256.)),
        poly(rectangle(15. / 256., 10. / 256., 25. / 256., 20. / 256.)),
    ]);
    assert_tiles(
        vec![(t(0, 0, 0), vec![rectangle(10., 10., 25., 20.)])],
        &render(&g, 0, 0, 4.0),
    );
}

#[test]
fn world_fill() {
    // A near-world rectangle at z8 fills every one of the 4^8 tiles.
    let s = rectangle_sq(Z14_WIDTH / 2.0, 1.0 - Z14_WIDTH / 2.0);
    let scaled = s.map_coords(|p| Coord {
        x: p.x * 256.0,
        y: p.y * 256.0,
    });
    let extents = ForZoom::new(8, 0, 0, 256, 256, None);
    let tiled = TiledGeometry::slice_geometry(&scaled, 0.0, 0.0, 8, &extents).expect("slice");
    assert_eq!(tiled.covered_tiles().iter().count(), 4usize.pow(8));
}

#[test]
fn emit_points_respect_extents() {
    // bounds = lon 0..180, lat -80..0 (planetiler "0,-80,180,0").
    let bounds = Rect::new(
        Coord {
            x: get_world_x(0.0),
            y: get_world_y(0.0),
        },
        Coord {
            x: get_world_x(180.0),
            y: get_world_y(-80.0),
        },
    );
    let extents = TileExtents::compute_from_world_bounds(1, bounds);
    let g = new_point(0.5 + 1.0 / 512.0, 0.5 + 1.0 / 512.0);
    let rendered = support::render_with(&g, 0, 1, 2.0, |z| extents.for_zoom(z));
    assert_tiles(
        vec![
            (t(0, 0, 0), vec![new_point(128.5, 128.5)]),
            (t(1, 1, 1), vec![new_point(1., 1.)]),
        ],
        &rendered,
    );
}

#[test]
fn process_points_near_dateline_and_poles() {
    let d = 1.0 / 512.0;
    // (x, wrapped, z1x0, z1x1)
    let xs = [
        (-d, 1.0 - d, -1.0, 255.0),
        (d, 1.0 + d, 1.0, 257.0),
        (1.0 - d, -d, -1.0, 255.0),
        (1.0 + d, d, 1.0, 257.0),
    ];
    // (y, z1ty, tyoff)
    let ys = [
        (0.25, 0u32, 128.0),
        (-d, 0, -1.0),
        (d, 0, 1.0),
        (1.0 - d, 1, 255.0),
        (1.0 + d, 1, 257.0),
    ];
    for &(x, wrapped, z1x0, z1x1) in &xs {
        for &(y, z1ty, tyoff) in &ys {
            let g = new_point(x, y);
            let rendered = render(&g, 0, 1, 2.0);
            assert_tiles(
                vec![
                    (
                        t(0, 0, 0),
                        vec![new_multi_point(&[
                            (x * 256., y * 256.),
                            (wrapped * 256., y * 256.),
                        ])],
                    ),
                    (t(0, z1ty, 1), vec![new_point(z1x0, tyoff)]),
                    (t(1, z1ty, 1), vec![new_point(z1x1, tyoff)]),
                ],
                &rendered,
            );
        }
    }
}
