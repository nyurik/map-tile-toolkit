//! The single-tile **polygon** slicer, [`PolygonSlicerOne`], over the [`clip_ring`] engine; the
//! all-tiles [`PolygonSlicerAll`](crate::PolygonSlicerAll) produces identical rings per tile from the
//! same ring-closing core, and [`PolygonMosaic`](crate::PolygonMosaic) reassembles them (see
//! `docs/polygon-slicer.md`).
//!
//! A polygon is an exterior ring plus zero or more interior rings (holes). Each ring is clipped to
//! the tile's buffered box keeping original vertices, and closed with synthetic clip-boundary corners
//! that live strictly outside the box (invisible to a renderer that clips to the tile). One input
//! polygon yields at most one output ring per input ring per tile; a ring that misses the tile is
//! dropped, and a tile fully inside the polygon is filled.
//!
//! The two generic axes match the polyline slicers: a per-vertex [`PolyVertex`] payload `V` (default
//! [`Coord<i32>`]) and a per-feature attribute `A` (default `()`).

use geo_types::Coord;

use crate::TileError;
use crate::clip_polygon::{RingClip, clip_ring};
use crate::clip_polyline::to_local;
use crate::grid::Grid;
use crate::polygon_view::{PolygonView, offset};
use crate::tile::TileId;
use crate::vertex::PolyVertex;

/// One polygon feature clipped into the tile: where its rings (exterior first, then holes) end, and
/// its attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FeatureEntry<A> {
    rings_end: u32,
    attr: A,
}

/// Slices integer **polygons** into pieces for **one fixed tile**, keeping original vertices.
///
/// The polygon counterpart to [`SlicerOne`](crate::SlicerOne): each polygon added is clipped only to
/// this slicer's [`tile`](Self::tile). Generic over the [`PolyVertex`] type `V` (default
/// [`Coord<i32>`]) and the per-feature attribute `A` (default `()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolygonSlicerOne<V: PolyVertex = Coord<i32>, A = ()> {
    grid: Grid,
    tile: TileId,
    /// Every kept ring's tile-local vertices (each ring closed), and where each ring ends.
    verts: Vec<V>,
    ring_ends: Vec<u32>,
    features: Vec<FeatureEntry<A>>,
}

impl<V: PolyVertex, A> PolygonSlicerOne<V, A> {
    /// Create a slicer bound to `tile`, with the given tile side / per-tile output resolution
    /// `extent` and `buffer` (same coordinate model as [`SlicerOne`](crate::SlicerOne)).
    ///
    /// # Errors
    ///
    /// - [`TileError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    /// - [`TileError::BufferTooLarge`] if `buffer` is not strictly less than half the `extent`.
    pub fn new(extent: u32, buffer: u16, tile: TileId) -> Result<Self, TileError> {
        Ok(Self {
            grid: Grid::new(extent, buffer)?,
            tile,
            verts: Vec::new(),
            ring_ends: Vec::new(),
            features: Vec::new(),
        })
    }

    /// The tile side / per-tile output resolution.
    #[must_use]
    pub fn extent(&self) -> u32 {
        self.grid.extent()
    }

    /// The buffer kept around the tile, in tile-space units.
    #[must_use]
    pub fn buffer(&self) -> u16 {
        self.grid.buffer()
    }

    /// The tile this slicer clips into.
    #[must_use]
    pub fn tile(&self) -> TileId {
        self.tile
    }

    /// Add one polygon — `exterior` ring plus `holes` — as an independent feature carrying `attr`,
    /// clipped to this slicer's [`tile`](Self::tile). Chainable. Recorded only if the exterior
    /// survives in the tile; otherwise `attr` is dropped. Rings may be given open or closed (a
    /// repeated first/last vertex is fine).
    ///
    /// When `A = ()`, prefer [`add_feature`](Self::add_feature).
    ///
    /// Atomic: on error the accumulator is unchanged.
    ///
    /// # Errors
    ///
    /// - [`TileError::Overflow`] if the tile's box, a synthetic corner, or a kept vertex overflows
    ///   `i32`.
    /// - [`TileError::PolylineTooLarge`] if the tile's vertices exceed the `u32` indexing of the
    ///   storage.
    pub fn add_feature_with(
        &mut self,
        exterior: &[V],
        holes: &[&[V]],
        attr: A,
    ) -> Result<&mut Self, TileError> {
        let (min, max) = self.grid.tile_buffered_bounds(self.tile)?;
        let origin = self
            .tile
            .origin(self.grid.extent())
            .ok_or(TileError::Overflow)?;

        let (verts_before, rings_before) = (self.verts.len(), self.ring_ends.len());
        let kept = self
            .clip_feature(exterior, holes, min, max, origin)
            .and_then(|kept| kept.then(|| offset(self.ring_ends.len())).transpose());
        match kept {
            Ok(Some(rings_end)) => self.features.push(FeatureEntry { rings_end, attr }),
            other => {
                // Absent from the tile, or failed: either way nothing of it stays.
                self.verts.truncate(verts_before);
                self.ring_ends.truncate(rings_before);
                other?;
            }
        }
        Ok(self)
    }

    /// Append the feature's clipped rings; `false` if it leaves nothing in the tile.
    fn clip_feature(
        &mut self,
        exterior: &[V],
        holes: &[&[V]],
        min: Coord<i32>,
        max: Coord<i32>,
        origin: Coord<i32>,
    ) -> Result<bool, TileError> {
        // Clip the exterior first — if it misses the tile entirely, the whole feature is absent here.
        match clip_ring(exterior, min, max)? {
            RingClip::Outside => return Ok(false),
            RingClip::Covers(ring) | RingClip::Clipped(ring) => self.push_ring(&ring, origin)?,
        }
        for hole in holes {
            match clip_ring(hole, min, max)? {
                // A hole that covers the whole tile leaves nothing to draw here — drop the feature
                // entirely rather than emit a fill exactly cancelled by its hole.
                RingClip::Covers(_) => return Ok(false),
                RingClip::Clipped(clipped) => self.push_ring(&clipped, origin)?,
                RingClip::Outside => {}
            }
        }
        Ok(true)
    }

    /// Append one clipped ring (global frame) in the tile-local frame (`vertex − origin`).
    fn push_ring(&mut self, ring: &[V], origin: Coord<i32>) -> Result<(), TileError> {
        for &v in ring {
            self.verts.push(to_local(v, origin)?);
        }
        self.ring_ends.push(offset(self.verts.len())?);
        Ok(())
    }

    /// Iterate the tile's polygon features that reach it, in the order added, each as the
    /// [`PolygonView`] of its clipped rings and [`attr`](PolygonView::attr).
    pub fn iter_features(&self) -> impl Iterator<Item = PolygonView<'_, V, A>> {
        let (verts, ring_ends, features) = (&self.verts, &self.ring_ends, &self.features);
        (0..features.len()).map(move |f| {
            let start = f
                .checked_sub(1)
                .map_or(0, |p| features[p].rings_end as usize);
            let rings = start..features[f].rings_end as usize;
            PolygonView::new(verts, ring_ends, rings, &features[f].attr)
        })
    }

    /// Number of polygon features accumulated for the tile.
    #[must_use]
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether nothing has been accumulated yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Discard everything accumulated, keeping the extent/buffer/tile config and buffer capacity.
    pub fn clear(&mut self) {
        self.verts.clear();
        self.ring_ends.clear();
        self.features.clear();
    }
}

impl<V: PolyVertex> PolygonSlicerOne<V, ()> {
    /// Add one polygon (`exterior` + `holes`) with no attribute — shorthand for
    /// [`add_feature_with`](Self::add_feature_with)`(…, ())`. Available only when `A = ()`.
    ///
    /// # Errors
    ///
    /// As in [`add_feature_with`](Self::add_feature_with).
    pub fn add_feature(&mut self, exterior: &[V], holes: &[&[V]]) -> Result<&mut Self, TileError> {
        self.add_feature_with(exterior, holes, ())
    }
}

#[cfg(test)]
mod tests {
    use geo_types::Coord;

    use super::*;

    fn ring(pts: &[(i32, i32)]) -> Vec<Coord<i32>> {
        pts.iter().map(|&(x, y)| Coord { x, y }).collect()
    }

    #[test]
    fn inside_polygon_is_kept_local() {
        // Extent 25, tile (0,0): a square fully inside is kept verbatim in local coords.
        let mut s = PolygonSlicerOne::<Coord<i32>>::new(25, 0, TileId::new(0, 0)).unwrap();
        let ext = ring(&[(5, 5), (20, 5), (20, 20), (5, 20), (5, 5)]);
        s.add_feature(&ext, &[]).unwrap();
        assert_eq!(s.len(), 1);
        let f = s.iter_features().next().unwrap();
        let rings: Vec<_> = f.iter_rings().collect();
        assert_eq!(rings.len(), 1);
        assert!(!rings[0].is_hole());
        assert_eq!(rings[0].vertices(), ext.as_slice());
    }

    #[test]
    fn polygon_missing_the_tile_is_dropped() {
        let mut s = PolygonSlicerOne::<Coord<i32>>::new(25, 0, TileId::new(0, 0)).unwrap();
        // Entirely in tile (4,4)'s area, far from tile (0,0), and not enclosing it.
        let ext = ring(&[(105, 105), (120, 105), (120, 120), (105, 120), (105, 105)]);
        s.add_feature(&ext, &[]).unwrap();
        assert!(
            s.is_empty(),
            "a polygon that misses the tile records nothing"
        );
    }

    #[test]
    fn tile_fully_inside_polygon_is_filled() {
        // A polygon enclosing tile (1,1) (covering 0..75) with no edge near it → solid fill, all
        // vertices synthetic (outside the tile's buffered box).
        let mut s = PolygonSlicerOne::<Coord<i32>>::new(25, 0, TileId::new(1, 1)).unwrap();
        let ext = ring(&[(-10, -10), (80, -10), (80, 80), (-10, 80), (-10, -10)]);
        s.add_feature(&ext, &[]).unwrap();
        let f = s.iter_features().next().expect("filled feature");
        let rings: Vec<_> = f.iter_rings().collect();
        assert_eq!(rings.len(), 1);
        // Local frame: the fill box spans just beyond [0,24] on each axis.
        let xs: Vec<i32> = rings[0].vertices().iter().map(|c| c.x).collect();
        assert!(
            xs.iter().all(|&x| !(0..25).contains(&x)),
            "fill box is outside the core cell"
        );
    }

    #[test]
    fn hole_inside_the_tile_is_kept() {
        let mut s = PolygonSlicerOne::<Coord<i32>>::new(25, 0, TileId::new(0, 0)).unwrap();
        let ext = ring(&[(2, 2), (22, 2), (22, 22), (2, 22), (2, 2)]);
        let hole = ring(&[(8, 8), (16, 8), (16, 16), (8, 16), (8, 8)]);
        s.add_feature(&ext, &[&hole]).unwrap();
        let f = s.iter_features().next().unwrap();
        let rings: Vec<_> = f.iter_rings().collect();
        assert_eq!(rings.len(), 2);
        assert!(!rings[0].is_hole());
        assert!(rings[1].is_hole());
        assert_eq!(rings[1].vertices(), hole.as_slice());
    }

    #[test]
    fn tile_fully_inside_hole_records_nothing() {
        // Exterior covers tiles 0..2 on each axis; the hole covers tile (1,1) [25,49] entirely. That
        // tile is all hole, so there is nothing to draw — no feature is recorded (rather than a fill
        // exactly cancelled by its hole).
        let ext = ring(&[(2, 2), (72, 2), (72, 72), (2, 72), (2, 2)]);
        let hole = ring(&[(22, 22), (22, 52), (52, 52), (52, 22), (22, 22)]);

        let mut inside = PolygonSlicerOne::<Coord<i32>>::new(25, 0, TileId::new(1, 1)).unwrap();
        inside.add_feature(&ext, &[&hole]).unwrap();
        assert!(
            inside.is_empty(),
            "a tile entirely inside a hole records nothing"
        );

        // A neighbor the hole only partially covers still keeps its (holed) fill.
        let mut edge = PolygonSlicerOne::<Coord<i32>>::new(25, 0, TileId::new(0, 1)).unwrap();
        edge.add_feature(&ext, &[&hole]).unwrap();
        assert_eq!(
            edge.len(),
            1,
            "a partially-holed tile still records a feature"
        );
    }

    #[test]
    fn attribute_rides_through() {
        let mut s = PolygonSlicerOne::<Coord<i32>, &str>::new(25, 0, TileId::new(0, 0)).unwrap();
        let ext = ring(&[(5, 5), (20, 5), (20, 20), (5, 20), (5, 5)]);
        s.add_feature_with(&ext, &[], "lake").unwrap();
        assert_eq!(*s.iter_features().next().unwrap().attr(), "lake");
    }
}
