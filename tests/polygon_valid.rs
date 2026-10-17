//! Every piece the clipped polygon slicer gives a tile must itself be a valid polygon whenever the
//! input polygon is — what a consumer that triangulates each piece on its own (MapLibre's earcut
//! over an MVT tile, say) relies on: simple rings, holes inside the exterior and clear of one another.
//!
//! Each fixture in `tests/polygons/fixtures` is checked at buffers 0 and 5 in
//! [`ClipMode::ClippedValid`], tile by tile across the padded tile span with [`Slicer::tile_polygon`],
//! using `geo`'s OGC validity. Fixtures whose input is not itself valid (deliberately overlapping
//! holes, say) are skipped: nothing is promised for them. Every fixture, valid or not, must also give
//! each tile the same piece from [`Slicer::polygon`] as from [`Slicer::tile_polygon`], as the
//! default mode does (`clip_polygon.rs`).

#![allow(clippy::pedantic, reason = "test tool")]

use std::path::Path;

use geo::Validation;
use geo_types::{Coord, LineString, Polygon};
use map_tile_toolkit::{ClipMode, PolygonSlice, Slicer, TileId};

mod support;

use crate::support::{EXTENT, FixturePolygon};

mod files {
    use test_each_file::test_each_path;

    test_each_path! { for ["geojson"] in "./tests/polygons/fixtures" => super::pieces_are_valid }
}

/// `rings` (exterior first) as an `f64` polygon, which `geo` can validate.
fn as_polygon<R: AsRef<[Coord<i32>]>>(rings: &[R]) -> Polygon<f64> {
    let line = |ring: &R| {
        LineString::new(
            ring.as_ref()
                .iter()
                .map(|c| Coord {
                    x: f64::from(c.x),
                    y: f64::from(c.y),
                })
                .collect(),
        )
    };
    Polygon::new(line(&rings[0]), rings[1..].iter().map(line).collect())
}

fn pieces_are_valid([path]: [&Path; 1]) {
    let polygons: Vec<FixturePolygon> = support::load_polygon_features(path)
        .into_iter()
        .flatten()
        .collect();
    let all_exteriors: Vec<Vec<Coord<i32>>> = polygons.iter().map(|p| p.exterior.clone()).collect();
    let (lo, hi) = support::padded_tile_span(&all_exteriors);

    let mut invalid = Vec::new();
    for buffer in [0, 5] {
        let mut slicer = Slicer::<Coord<i32>>::new(EXTENT, buffer)
            .expect("valid config")
            .with_clip_mode(ClipMode::ClippedValid);
        for (n, polygon) in polygons.iter().enumerate() {
            let input = support::rings(polygon);
            let mut all = std::collections::BTreeMap::new();
            let mut slices = slicer.polygon(&input).expect("slice");
            while let Some(slice) = slices.next() {
                if let PolygonSlice::Piece(piece) = slice.expect("slice") {
                    let rings: Vec<Vec<Coord<i32>>> = piece.rings().map(|r| r.to_vec()).collect();
                    all.insert(piece.tile(), rings);
                }
            }
            let valid_input = as_polygon(&input).check_validation().is_ok();
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    let tile = TileId::new(x, y);
                    let piece = slicer.tile_polygon(tile, &input).expect("clip");
                    let rings: Option<Vec<Vec<Coord<i32>>>> =
                        piece.map(|p| p.rings().map(|r| r.to_vec()).collect());
                    // (A covered tile's fill run is not a piece of `polygon`'s.)
                    if let Some(got) = all.get(&tile) {
                        assert_eq!(
                            Some(got),
                            rings.as_ref(),
                            "buffer {buffer}, polygon {n}, tile {x},{y}"
                        );
                    }
                    let Some(rings) = rings else {
                        continue;
                    };
                    if !valid_input {
                        continue;
                    }
                    if let Err(e) = as_polygon(&rings).check_validation() {
                        invalid.push(format!("buffer {buffer}, polygon {n}, tile {x},{y}: {e}"));
                    }
                }
            }
        }
    }
    assert!(
        invalid.is_empty(),
        "{} invalid piece(s) from a valid polygon:\n{}",
        invalid.len(),
        invalid.join("\n")
    );
}
