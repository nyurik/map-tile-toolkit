//! The public **polygon** slicing API. Currently [`PolygonSlicerOne`] (one fixed tile); the
//! all-tiles variant and `Mosaic` reassembly are layered on the same [`clip_ring`] engine (see
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
use crate::clip_polygon::clip_ring;
use crate::clip_polyline::to_local;
use crate::grid::Grid;
use crate::tile::TileId;
use crate::vertex::PolyVertex;

/// One clipped ring in a tile's local frame, tagged exterior vs. hole.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ring<V> {
    verts: Vec<V>,
    is_hole: bool,
}

/// One polygon feature clipped into a tile: its surviving rings (exterior first, then holes) plus the
/// per-feature attribute (cloned into each tile the feature reaches).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PolyFeature<V, A> {
    rings: Vec<Ring<V>>,
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
    features: Vec<PolyFeature<V, A>>,
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
    /// Atomic: the polygon is fully clipped before anything is recorded, so on error the accumulator
    /// is unchanged.
    ///
    /// # Errors
    ///
    /// [`TileError::Overflow`] if the tile's box, a synthetic corner, or a kept vertex overflows
    /// `i32`.
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

        // Clip the exterior first — if it misses the tile entirely, the whole feature is absent here.
        let Some(ext) = clip_ring(exterior, min, max)? else {
            return Ok(self);
        };
        let mut rings = vec![Ring {
            verts: localize(&ext, origin)?,
            is_hole: false,
        }];
        for hole in holes {
            if let Some(clipped) = clip_ring(hole, min, max)? {
                rings.push(Ring {
                    verts: localize(&clipped, origin)?,
                    is_hole: true,
                });
            }
        }
        self.features.push(PolyFeature { rings, attr });
        Ok(self)
    }

    /// Iterate the tile's polygon features, in the order added. Each [`PolyFeatureView`] exposes that
    /// feature's rings and its [`attr`](PolyFeatureView::attr).
    pub fn iter_features(&self) -> impl Iterator<Item = PolyFeatureView<'_, V, A>> {
        self.features.iter().map(|f| PolyFeatureView { feature: f })
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

    /// Discard everything accumulated, keeping the extent/buffer/tile config.
    pub fn clear(&mut self) {
        self.features.clear();
    }
}

impl<V: PolyVertex> PolygonSlicerOne<V, ()> {
    /// Add one polygon (`exterior` + `holes`) with no attribute — shorthand for
    /// [`add_feature_with`](Self::add_feature_with)`(…, ())`. Available only when `A = ()`.
    ///
    /// # Errors
    ///
    /// [`TileError::Overflow`] as in [`add_feature_with`](Self::add_feature_with).
    pub fn add_feature(&mut self, exterior: &[V], holes: &[&[V]]) -> Result<&mut Self, TileError> {
        self.add_feature_with(exterior, holes, ())
    }
}

/// Re-express a clipped ring (global frame) in the tile-local frame (`vertex − origin`).
fn localize<V: PolyVertex>(ring: &[V], origin: Coord<i32>) -> Result<Vec<V>, TileError> {
    ring.iter().map(|&v| to_local(v, origin)).collect()
}

/// A borrowed view of one clipped polygon feature.
pub struct PolyFeatureView<'a, V: PolyVertex, A = ()> {
    feature: &'a PolyFeature<V, A>,
}

impl<'a, V: PolyVertex, A> PolyFeatureView<'a, V, A> {
    /// The feature's per-feature attribute.
    #[must_use]
    pub fn attr(&self) -> &'a A {
        &self.feature.attr
    }

    /// Iterate the feature's rings (exterior first, then holes), each in the tile-local frame.
    pub fn iter_rings(&self) -> impl Iterator<Item = RingView<'a, V>> {
        self.feature.rings.iter().map(|r| RingView {
            verts: &r.verts,
            is_hole: r.is_hole,
        })
    }
}

/// A borrowed view of one clipped ring.
pub struct RingView<'a, V: PolyVertex> {
    verts: &'a [V],
    is_hole: bool,
}

impl<'a, V: PolyVertex> RingView<'a, V> {
    /// The ring's vertices in the tile-local frame, closed (first vertex repeated at the end).
    #[must_use]
    pub fn vertices(&self) -> &'a [V] {
        self.verts
    }

    /// Whether this ring is an interior ring (a hole).
    #[must_use]
    pub fn is_hole(&self) -> bool {
        self.is_hole
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
    fn attribute_rides_through() {
        let mut s = PolygonSlicerOne::<Coord<i32>, &str>::new(25, 0, TileId::new(0, 0)).unwrap();
        let ext = ring(&[(5, 5), (20, 5), (20, 20), (5, 20), (5, 5)]);
        s.add_feature_with(&ext, &[], "lake").unwrap();
        assert_eq!(*s.iter_features().next().unwrap().attr(), "lake");
    }
}
