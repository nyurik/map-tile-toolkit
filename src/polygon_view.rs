//! Borrowed views of clipped polygons, shared by [`PolygonSlicerOne`](crate::PolygonSlicerOne) and
//! [`PolygonSlicerAll`](crate::PolygonSlicerAll), over their flat arenas: tile-local ring vertices
//! and each ring's end offset.

use core::ops::Range;

use crate::TileError;
use crate::vertex::PolyVertex;

/// `len` as a `u32` arena offset. The flat storage indexes with `u32`; geometry beyond that is
/// rejected rather than truncated.
pub(crate) fn offset(len: usize) -> Result<u32, TileError> {
    u32::try_from(len).map_err(|_| TileError::PolylineTooLarge)
}

/// The arena span of item `i`, given every item's end offset.
pub(crate) fn span(ends: &[u32], i: usize) -> Range<usize> {
    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
    start..ends[i] as usize
}

/// A borrowed view of one clipped polygon in one tile: its exterior ring, then its holes, and the
/// attribute of the feature it comes from.
pub struct PolygonView<'a, V: PolyVertex, A = ()> {
    verts: &'a [V],
    ring_ends: &'a [u32],
    /// This polygon's rings, `first..end`, as indices into `ring_ends`.
    first: usize,
    end: usize,
    attr: &'a A,
}

impl<'a, V: PolyVertex, A> PolygonView<'a, V, A> {
    pub(crate) fn new(
        verts: &'a [V],
        ring_ends: &'a [u32],
        rings: Range<usize>,
        attr: &'a A,
    ) -> Self {
        Self {
            verts,
            ring_ends,
            first: rings.start,
            end: rings.end,
            attr,
        }
    }

    /// The attribute of the feature this polygon comes from, as passed to `add_feature_with`.
    #[must_use]
    pub fn attr(&self) -> &'a A {
        self.attr
    }

    /// Iterate the polygon's rings (exterior first, then holes), each closed and in the tile-local
    /// frame.
    pub fn iter_rings(&self) -> impl Iterator<Item = RingView<'a, V>> + use<'a, V, A> {
        let (verts, ends, first) = (self.verts, self.ring_ends, self.first);
        (first..self.end).map(move |r| RingView {
            verts: &verts[span(ends, r)],
            is_hole: r != first,
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

/// `Clone` and `Copy` for borrowed views, without requiring them of the attribute or vertex type.
macro_rules! copy_view {
    ($($view:ident<$($param:ident),*>: $bound:path),* $(,)?) => {$(
        #[allow(
            clippy::expl_impl_clone_on_copy,
            reason = "a derive would require the vertex and attribute types to be Copy too; some views trip the lint and some do not"
        )]
        impl<V: $bound, $($param),*> Clone for $view<'_, V, $($param),*> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<V: $bound, $($param),*> Copy for $view<'_, V, $($param),*> {}
    )*};
}
pub(crate) use copy_view;

copy_view!(PolygonView<A>: PolyVertex, RingView<>: PolyVertex);
