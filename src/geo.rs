//! `geo-types` `Geometry` bridge for the slicers, behind the default `geo` feature.
//!
//! The core slicing API works on plain vertex slices (`&[Coord<i32>]`); this module adds the
//! convenience of feeding a `Geometry<i32>` in and reading `Geometry<i32>` pieces back out, for
//! callers already working with `geo-types`. It is only available for `Coord<i32>` vertices, since
//! `geo-types` cannot carry a payload.
//!
//! Each line becomes **one feature**: a `LineString` is a single feature, and a `MultiLineString`'s
//! lines are added as independent features. Reading back yields one `Geometry` per feature per tile.

use geo_types::{Coord, Geometry, LineString, MultiLineString};

use crate::{SliceError, SlicerAll, SlicerOne, TileId};

/// The component lines of a polyline geometry, borrowed (no allocation). Errors for any other
/// geometry kind rather than panicking.
fn each_line(geom: &Geometry<i32>) -> Result<&[LineString<i32>], SliceError> {
    match geom {
        Geometry::LineString(ls) => Ok(std::slice::from_ref(ls)),
        Geometry::MultiLineString(mls) => Ok(&mls.0),
        other => Err(SliceError::UnsupportedGeometry(geometry_kind(other))),
    }
}

/// The name of a geometry variant, for error messages.
fn geometry_kind(geom: &Geometry<i32>) -> &'static str {
    match geom {
        Geometry::Point(_) => "Point",
        Geometry::Line(_) => "Line",
        Geometry::LineString(_) => "LineString",
        Geometry::Polygon(_) => "Polygon",
        Geometry::MultiPoint(_) => "MultiPoint",
        Geometry::MultiLineString(_) => "MultiLineString",
        Geometry::MultiPolygon(_) => "MultiPolygon",
        Geometry::GeometryCollection(_) => "GeometryCollection",
        Geometry::Rect(_) => "Rect",
        Geometry::Triangle(_) => "Triangle",
    }
}

/// Wrap one feature's kept runs as a single geometry: `None` (no runs), one `LineString`, or a
/// `MultiLineString`.
fn assemble(runs: impl Iterator<Item = Vec<Coord<i32>>>) -> Option<Geometry<i32>> {
    let mut lines: Vec<LineString<i32>> = runs.map(LineString).collect();
    match lines.len() {
        0 => None,
        1 => lines.pop().map(Geometry::LineString),
        _ => Some(Geometry::MultiLineString(MultiLineString(lines))),
    }
}

impl SlicerAll<Coord<i32>> {
    /// Add a polyline `geom` (a `LineString` or `MultiLineString`), slicing it into every tile it
    /// touches. Each line is an independent feature (a `MultiLineString` adds one per line). Chainable.
    ///
    /// # Errors
    ///
    /// - [`SliceError::UnsupportedGeometry`] — `geom` is not a `LineString` / `MultiLineString`.
    /// - Otherwise whatever [`add_feature`](Self::add_feature) returns ([`SliceError::PolylineTooLarge`],
    ///   [`SliceError::TooManyTiles`], [`SliceError::Overflow`]).
    pub fn add_geometry(&mut self, geom: &Geometry<i32>) -> Result<&mut Self, SliceError> {
        for line in each_line(geom)? {
            self.add_feature(line.0.as_slice())?;
        }
        Ok(self)
    }

    /// Read the accumulated pieces back as `(tile, geometry)` pairs — one geometry per feature per
    /// tile, each collapsed into a `LineString` (one run) or `MultiLineString` (several). Tiles come
    /// in [`TileId`] order; within a tile, features come in insertion order.
    pub fn iter_geometries(&self) -> impl Iterator<Item = (TileId, Geometry<i32>)> + '_ {
        self.iter_tiles().flat_map(|tile| {
            let id = tile.id();
            tile.iter_features().filter_map(move |feat| {
                assemble(feat.iter_polylines().map(<[_]>::to_vec)).map(|g| (id, g))
            })
        })
    }
}

impl SlicerOne<Coord<i32>> {
    /// Add a polyline `geom` (a `LineString` or `MultiLineString`), clipped to this slicer's tile.
    /// Each line is an independent feature (a `MultiLineString` adds one per line). Chainable.
    ///
    /// # Errors
    ///
    /// - [`SliceError::UnsupportedGeometry`] — `geom` is not a `LineString` / `MultiLineString`.
    /// - Otherwise whatever [`add_feature`](Self::add_feature) returns ([`SliceError::Overflow`]).
    pub fn add_geometry(&mut self, geom: &Geometry<i32>) -> Result<&mut Self, SliceError> {
        for line in each_line(geom)? {
            self.add_feature(line.0.as_slice())?;
        }
        Ok(self)
    }

    /// Read the tile's pieces back as one `Geometry` per feature, each collapsed into a `LineString`
    /// (one run) or `MultiLineString` (several), in feature-insertion order.
    pub fn iter_geometries(&self) -> impl Iterator<Item = Geometry<i32>> + '_ {
        self.iter_features()
            .filter_map(|feat| assemble(feat.iter_polylines().map(<[_]>::to_vec)))
    }
}
