//! Trivial coverage for the config/size getters and `TileId` conversions.

use geo_types::Coord;
use map_tile_toolkit::{Mosaic, SlicerAll, SlicerOne, TileId};

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
fn mosaic_reports_its_extent() {
    let m = Mosaic::<Coord<i32>>::new(4096).expect("valid config");
    assert_eq!(m.extent(), 4096);
}

#[test]
fn tile_id_from_tuple() {
    assert_eq!(TileId::from((3, -7)), TileId::new(3, -7));
}
