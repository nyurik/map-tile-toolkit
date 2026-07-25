//! `geo-types` bridge for the slicers, behind the default `geo` feature.
//!
//! The core slicing API works on plain vertex slices (`&[Coord<i32>]`); this module adds the
//! convenience of feeding a `LineString<i32>` in and reading `Geometry<i32>` pieces back out, for
//! callers already working with `geo-types`. It is only available for `Coord<i32>` vertices, since
//! `geo-types` cannot carry a payload.
//!
//! Each line is **one feature**. Reading back yields one `Geometry` per feature per tile — a
//! `LineString` when the feature stays in one piece, or a `MultiLineString` when clipping split it.

use geo_types::{Coord, Geometry, LineString, MultiLineString};

use crate::{SlicerAll, SlicerOne, TileId};

/// Wrap one feature's kept runs as a single geometry: `None` (no runs), one `LineString`, or a
/// `MultiLineString`.
fn assemble(mut runs: impl Iterator<Item = Vec<Coord<i32>>>) -> Option<Geometry<i32>> {
    let first = runs.next()?;
    let Some(second) = runs.next() else {
        return Some(Geometry::LineString(LineString(first)));
    };
    let mut lines = vec![LineString(first), LineString(second)];
    lines.extend(runs.map(LineString));
    Some(Geometry::MultiLineString(MultiLineString(lines)))
}

impl SlicerAll<Coord<i32>> {
    /// Add a `line` as one feature, slicing it into every tile it touches. Chainable. Feed the lines
    /// of a `MultiLineString` one at a time to add each as its own feature.
    ///
    /// # Errors
    ///
    /// Whatever [`add_feature`](Self::add_feature) returns ([`TileError::PolylineTooLarge`],
    /// [`TileError::TooManyTiles`], [`TileError::Overflow`]).
    ///
    /// [`TileError::PolylineTooLarge`]: crate::TileError::PolylineTooLarge
    /// [`TileError::TooManyTiles`]: crate::TileError::TooManyTiles
    /// [`TileError::Overflow`]: crate::TileError::Overflow
    pub fn add_line(&mut self, line: &LineString<i32>) -> Result<&mut Self, crate::TileError> {
        self.add_feature(line.0.as_slice())?;
        Ok(self)
    }

    /// Read the accumulated pieces back as `(tile, geometry)` pairs — one geometry per feature per
    /// tile, each collapsed into a `LineString` (one run) or `MultiLineString` (several). Tiles come
    /// in [`TileId`] order; within a tile, features come in insertion order.
    pub fn iter_geometries(&self) -> impl Iterator<Item = (TileId, Geometry<i32>)> + '_ {
        self.iter_tiles().flat_map(|tile| {
            let id = tile.id();
            tile.iter_features().filter_map(move |v| {
                assemble(v.iter_polylines().map(<[_]>::to_vec)).map(|g| (id, g))
            })
        })
    }
}

impl SlicerOne<Coord<i32>> {
    /// Add a `line` as one feature, clipped to this slicer's tile. Chainable. Feed the lines of a
    /// `MultiLineString` one at a time to add each as its own feature.
    ///
    /// # Errors
    ///
    /// Whatever [`add_feature`](Self::add_feature) returns ([`TileError::Overflow`]).
    ///
    /// [`TileError::Overflow`]: crate::TileError::Overflow
    pub fn add_line(&mut self, line: &LineString<i32>) -> Result<&mut Self, crate::TileError> {
        self.add_feature(line.0.as_slice())?;
        Ok(self)
    }

    /// Read the tile's pieces back as one `Geometry` per feature, each collapsed into a `LineString`
    /// (one run) or `MultiLineString` (several), in feature-insertion order.
    pub fn iter_geometries(&self) -> impl Iterator<Item = Geometry<i32>> + '_ {
        self.iter_features()
            .filter_map(|v| assemble(v.iter_polylines().map(<[_]>::to_vec)))
    }
}
