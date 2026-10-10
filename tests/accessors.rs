//! Trivial coverage for the config/size getters and `TileId` conversions.

use geo_types::Coord;
use map_tile_toolkit::{
    Mosaic, PolygonMosaic, PolygonSlicerAll, PolygonSlicerOne, SlicerAll, SlicerOne, TileId,
};

#[test]
fn slicer_all_reports_its_config() {
    let s = SlicerAll::<Coord<i32>>::new(25, 4).expect("valid config");
    assert_eq!(s.extent(), 25);
    assert_eq!(s.buffer(), 4);
}

#[test]
fn slicer_one_reports_its_config() {
    let s = SlicerOne::<Coord<i32>>::new(25, 4, TileId::new(2, 3)).expect("valid config");
    assert_eq!(s.extent(), 25);
    assert_eq!(s.buffer(), 4);
    assert_eq!(s.tile(), TileId::new(2, 3));
    assert_eq!(s.len(), 0);
    assert!(s.is_empty());
}

#[test]
fn polygon_slicer_all_reports_its_config() {
    let s = PolygonSlicerAll::<Coord<i32>>::new(25, 4).expect("valid config");
    assert_eq!(s.extent(), 25);
    assert_eq!(s.buffer(), 4);
    assert_eq!(s.len(), 0);
    assert!(s.is_empty());
}

#[test]
fn polygon_slicer_one_reports_its_config_compares_and_clears() {
    let tile = TileId::new(2, 3);
    let mut s = PolygonSlicerOne::<Coord<i32>>::new(25, 4, tile).expect("valid config");
    assert_eq!((s.extent(), s.buffer(), s.tile()), (25, 4, tile));
    // A square crossing the tile's left edge, so the clip uses (and keeps) its working memory.
    let square = [(40, 80), (65, 80), (65, 90), (40, 90)].map(|(x, y)| Coord { x, y });
    s.add_feature(&square, &[]).expect("polygon");
    assert_eq!(s.len(), 1);
    // That working memory is not part of the slicer's value: a clone (which starts without it)
    // compares equal, and it does not show in the debug output's fields.
    let copy = s.clone();
    assert_eq!(copy, s);
    // Views are `Copy`, and cloning one explicitly gives the same view.
    let view = s.iter_features().next().expect("a feature");
    assert_eq!(
        Clone::clone(&view).iter_rings().count(),
        view.iter_rings().count()
    );
    assert!(format!("{s:?}").contains("ClipScratch"));
    s.clear();
    assert!(s.is_empty());
    assert_ne!(copy, s);
}

#[test]
fn polygon_mosaic_reports_its_config() {
    let m = PolygonMosaic::<Coord<i32>>::new(4096, 64).expect("valid config");
    assert_eq!((m.extent(), m.buffer()), (4096, 64));
}

#[test]
fn mosaic_reports_its_extent() {
    let m = Mosaic::<Coord<i32>>::new(4096).expect("valid config");
    assert_eq!(m.extent(), 4096);
}

#[test]
fn tile_id_from_tuple() {
    assert_eq!(TileId::from((3, -7)), TileId::new(3, -7));
}
