//! The public slicing API: [`SlicerAll`] (accumulate every tile a polyline touches) and
//! [`SlicerOne`] (accumulate a single, fixed tile).
//!
//! Both wrap the same stateless [`Grid`] engine and accumulate results as **features**. A feature is
//! a caller-defined group of polylines: begin one with [`add_feature`](SlicerAll::add_feature), then
//! extend it with [`continue_last_feature`](SlicerAll::continue_last_feature) — so the several lines
//! of one multi-line geometry become a single feature, while unrelated inputs become separate
//! features. Each polyline is sliced and its per-tile runs folded into the feature it belongs to.
//!
//! Read the accumulated state back with iterators, never owned `Vec`s:
//!
//! - [`SlicerAll::iter_tiles`] → [`TileView::iter_features`] → [`FeatureView::iter_polylines`];
//! - [`SlicerOne::iter_features`] → [`FeatureView::iter_polylines`] (one implicit tile, so no tile
//!   level).
//!
//! Runs come out in each tile's **local frame**: the tile's `[0, 0]` corner is the origin, so a
//! vertex at global `(x, y)` is `(x − tile.x·divider, y − tile.y·divider)` (in-tile vertices land in
//! `0..divider`; buffer vertices past the low edge go negative). [`merge`](crate::merge) is the
//! inverse, stitching a tile's pieces back together.

use std::collections::BTreeMap;

use geo_types::Coord;

use crate::SliceError;
use crate::grid::Grid;
use crate::tile::TileId;
use crate::vertex::Vertex;

/// One accumulated feature: its runs (each a vertex list) in a tile's local frame, tagged with the
/// feature id it belongs to so [`continue_last_feature`](SlicerAll::continue_last_feature) can find
/// the currently-open feature within a tile that may hold several.
type FeatureRuns<V> = (u64, Vec<Vec<V>>);

/// Fold `runs` into `entries` under feature `id`: extend the last feature if it is `id` (the open
/// feature reached this tile before), otherwise start a new feature. Empty `runs` are dropped, so a
/// feature materializes in a tile only once something of it lands there. Feature ids only ever grow,
/// so the open feature — when present — is always the last entry, keeping `entries` in id order.
fn absorb<V: Vertex>(entries: &mut Vec<FeatureRuns<V>>, id: u64, runs: Vec<Vec<V>>) {
    if runs.is_empty() {
        return;
    }
    match entries.last_mut() {
        Some((last, feat)) if *last == id => feat.extend(runs),
        _ => entries.push((id, runs)),
    }
}

/// A borrowed view of one tile's accumulated features, yielded by [`SlicerAll::iter_tiles`].
pub struct TileView<'a, V: Vertex> {
    id: TileId,
    features: &'a [FeatureRuns<V>],
}

impl<'a, V: Vertex> TileView<'a, V> {
    /// The tile this view is for.
    #[must_use]
    pub fn id(&self) -> TileId {
        self.id
    }

    /// Iterate this tile's features, in the order they were first added.
    pub fn iter_features(&self) -> impl Iterator<Item = FeatureView<'a, V>> + use<'a, V> {
        self.features.iter().map(|(_, runs)| FeatureView {
            runs: runs.as_slice(),
        })
    }
}

/// A borrowed view of one feature's clipped polylines within a tile, yielded by
/// [`TileView::iter_features`] / [`SlicerOne::iter_features`].
pub struct FeatureView<'a, V: Vertex> {
    runs: &'a [Vec<V>],
}

impl<'a, V: Vertex> FeatureView<'a, V> {
    /// Iterate this feature's polylines (runs) in this tile, each a vertex slice in the tile's local
    /// frame. A feature yields several polylines where the input left the tile and re-entered.
    pub fn iter_polylines(&self) -> impl Iterator<Item = &'a [V]> + use<'a, V> {
        self.runs.iter().map(Vec::as_slice)
    }
}

/// The next feature id to hand out, and the currently-open one (for `continue_last_feature`).
///
/// Shared bookkeeping between [`SlicerAll`] and [`SlicerOne`]: `add_feature` opens a fresh id;
/// `continue_last_feature` reuses the open id, opening a fresh one if none is open yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FeatureCounter {
    next: u64,
    open: Option<u64>,
}

impl FeatureCounter {
    const fn new() -> Self {
        Self {
            next: 0,
            open: None,
        }
    }

    /// Open a brand-new feature and return its id (for `add_feature`).
    fn open_new(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        self.open = Some(id);
        id
    }

    /// The open feature's id, opening a new one if none is open (for `continue_last_feature`).
    fn open_or_new(&mut self) -> u64 {
        match self.open {
            Some(id) => id,
            None => self.open_new(),
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

/// Slices integer polylines into per-tile pieces on an integer grid, accumulating every tile each
/// polyline touches.
///
/// Generic over the [`Vertex`] type `V` (defaults to [`Coord<i32>`]), so plain coordinates or
/// payload-carrying vertices (e.g. an M value via [`Measured`](crate::Measured)) both work; the
/// payload rides through slicing unchanged. Build features with [`add_feature`](Self::add_feature) /
/// [`continue_last_feature`](Self::continue_last_feature) and read them back with
/// [`iter_tiles`](Self::iter_tiles).
///
/// The slicer never panics: bad input (an oversized polyline, or coordinates that overflow the tile
/// math) yields an [`SliceError`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicerAll<V: Vertex = Coord<i32>> {
    grid: Grid,
    features: FeatureCounter,
    tiles: BTreeMap<TileId, Vec<FeatureRuns<V>>>,
}

impl<V: Vertex> SlicerAll<V> {
    /// Create a slicer with the given tile side and buffer, and an empty accumulator.
    ///
    /// # Errors
    ///
    /// Returns [`SliceError::InvalidDivider`] if `divider` is `0` or greater than `i32::MAX`.
    pub fn new(divider: u32, buffer: u16) -> Result<Self, SliceError> {
        Ok(Self {
            grid: Grid::new(divider, buffer)?,
            features: FeatureCounter::new(),
            tiles: BTreeMap::new(),
        })
    }

    /// The tile side length, in coordinate units.
    #[must_use]
    pub fn divider(&self) -> u32 {
        self.grid.divider()
    }

    /// The buffer kept around every tile, in coordinate units.
    #[must_use]
    pub fn buffer(&self) -> u16 {
        self.grid.buffer()
    }

    /// Begin a new feature from `polyline`: slice it into every tile it touches and store its runs as
    /// a fresh feature in each. Chainable.
    ///
    /// Atomic: the polyline is fully sliced first, so on error the accumulator is left unchanged.
    ///
    /// # Errors
    ///
    /// Whatever the engine returns: [`SliceError::PolylineTooLarge`], [`SliceError::TooManyTiles`], or
    /// [`SliceError::Overflow`].
    pub fn add_feature<P: AsRef<[V]>>(&mut self, polyline: P) -> Result<&mut Self, SliceError> {
        let sliced = self.grid.slice_all(polyline.as_ref())?;
        let id = self.features.open_new();
        for (tile, runs) in sliced {
            absorb(self.tiles.entry(tile).or_default(), id, runs);
        }
        Ok(self)
    }

    /// Extend the feature opened by the last [`add_feature`](Self::add_feature) with another
    /// `polyline` — slice it and fold its per-tile runs into that same feature (so the lines of one
    /// multi-line geometry stay a single feature). If no feature is open yet, this begins one.
    /// Chainable.
    ///
    /// Atomic: the polyline is fully sliced first, so on error the accumulator is left unchanged.
    ///
    /// # Errors
    ///
    /// Whatever the engine returns: [`SliceError::PolylineTooLarge`], [`SliceError::TooManyTiles`], or
    /// [`SliceError::Overflow`].
    pub fn continue_last_feature<P: AsRef<[V]>>(
        &mut self,
        polyline: P,
    ) -> Result<&mut Self, SliceError> {
        let sliced = self.grid.slice_all(polyline.as_ref())?;
        let id = self.features.open_or_new();
        for (tile, runs) in sliced {
            absorb(self.tiles.entry(tile).or_default(), id, runs);
        }
        Ok(self)
    }

    /// Iterate the touched tiles, ordered by [`TileId`], borrowing so the accumulator can keep
    /// growing afterwards. Each [`TileView`] exposes that tile's features.
    pub fn iter_tiles(&self) -> impl Iterator<Item = TileView<'_, V>> {
        self.tiles.iter().map(|(&id, features)| TileView {
            id,
            features: features.as_slice(),
        })
    }

    /// Number of tiles the accumulator has touched.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Whether nothing has been accumulated yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Discard everything accumulated so far, keeping the divider/buffer config.
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.features.reset();
    }
}

/// Slices integer polylines into pieces for **one fixed tile**, accumulating only that tile.
///
/// The single-tile counterpart to [`SlicerAll`]: [`add_feature`](Self::add_feature) /
/// [`continue_last_feature`](Self::continue_last_feature) work exactly the same, but each polyline is
/// clipped only to this slicer's [`tile`](Self::tile). Because there is a single tile, the read API
/// skips the tile level — [`iter_features`](Self::iter_features) yields the features directly.
///
/// Generic over the [`Vertex`] type `V` (defaults to [`Coord<i32>`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicerOne<V: Vertex = Coord<i32>> {
    grid: Grid,
    tile: TileId,
    features: FeatureCounter,
    runs: Vec<FeatureRuns<V>>,
}

impl<V: Vertex> SlicerOne<V> {
    /// Create a slicer bound to `tile`, with the given tile side and buffer, and an empty
    /// accumulator.
    ///
    /// # Errors
    ///
    /// Returns [`SliceError::InvalidDivider`] if `divider` is `0` or greater than `i32::MAX`.
    pub fn new(divider: u32, buffer: u16, tile: TileId) -> Result<Self, SliceError> {
        Ok(Self {
            grid: Grid::new(divider, buffer)?,
            tile,
            features: FeatureCounter::new(),
            runs: Vec::new(),
        })
    }

    /// The tile side length, in coordinate units.
    #[must_use]
    pub fn divider(&self) -> u32 {
        self.grid.divider()
    }

    /// The buffer kept around every tile, in coordinate units.
    #[must_use]
    pub fn buffer(&self) -> u16 {
        self.grid.buffer()
    }

    /// The tile this slicer clips into.
    #[must_use]
    pub fn tile(&self) -> TileId {
        self.tile
    }

    /// Begin a new feature from `polyline`, clipped to this slicer's [`tile`](Self::tile). Chainable.
    /// A feature is recorded only if something of `polyline` lands in the tile.
    ///
    /// Atomic: the polyline is fully sliced first, so on error the accumulator is left unchanged.
    ///
    /// # Errors
    ///
    /// [`SliceError::Overflow`] if the tile's box or a kept vertex overflows `i32`.
    pub fn add_feature<P: AsRef<[V]>>(&mut self, polyline: P) -> Result<&mut Self, SliceError> {
        let runs = self.grid.slice_one(polyline.as_ref(), self.tile)?;
        let id = self.features.open_new();
        absorb(&mut self.runs, id, runs);
        Ok(self)
    }

    /// Extend the feature opened by the last [`add_feature`](Self::add_feature) with another
    /// `polyline`, clipped to this slicer's [`tile`](Self::tile). If no feature is open yet, this
    /// begins one. Chainable.
    ///
    /// Atomic: the polyline is fully sliced first, so on error the accumulator is left unchanged.
    ///
    /// # Errors
    ///
    /// [`SliceError::Overflow`] if the tile's box or a kept vertex overflows `i32`.
    pub fn continue_last_feature<P: AsRef<[V]>>(
        &mut self,
        polyline: P,
    ) -> Result<&mut Self, SliceError> {
        let runs = self.grid.slice_one(polyline.as_ref(), self.tile)?;
        let id = self.features.open_or_new();
        absorb(&mut self.runs, id, runs);
        Ok(self)
    }

    /// Iterate this tile's features, in the order they were first added. Each [`FeatureView`] exposes
    /// that feature's clipped polylines.
    pub fn iter_features(&self) -> impl Iterator<Item = FeatureView<'_, V>> {
        self.runs.iter().map(|(_, runs)| FeatureView {
            runs: runs.as_slice(),
        })
    }

    /// Number of features accumulated for the tile.
    #[must_use]
    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// Whether nothing has been accumulated yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Discard everything accumulated so far, keeping the divider/buffer/tile config.
    pub fn clear(&mut self) {
        self.runs.clear();
        self.features.reset();
    }
}
