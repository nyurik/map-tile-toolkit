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
//!
//! ## Storage
//!
//! A tile's geometry is stored **flattened** in a [`TileBuf`]: every vertex concatenated into one
//! `verts` arena, with run and feature boundaries kept as `u32` offset arrays rather than nested
//! `Vec`s. [`SlicerAll`] keeps the tiles in a `Vec<TileBuf>` (stable slots) plus a `BTreeMap` from
//! [`TileId`] to slot for find-or-insert and ordered reads; [`SlicerOne`] holds a single `TileBuf`.

use std::collections::BTreeMap;

use geo_types::Coord;

use crate::SliceError;
use crate::clip_polyline::to_local;
use crate::grid::{Grid, RouteSink};
use crate::tile::TileId;
use crate::vertex::Vertex;

/// One tile's accumulated geometry, flattened: all runs' vertices concatenated into `verts`, with
/// run and feature boundaries kept as offsets instead of nested `Vec`s.
///
/// - run `r` is `verts[run_ends[r-1] .. run_ends[r]]` (with `run_ends[-1] ≡ 0`);
/// - feature `f` is the runs `run_ends[feat_ends[f-1] .. feat_ends[f]]`.
///
/// `last_id` is the feature id of the most-recent feature written here, so a continuation can tell
/// whether the currently-open feature already reached this tile (extend it) or not (start a new one).
/// Only non-empty features are ever recorded, so every `feat_ends` span holds at least one run.
///
/// `open_step` is the [`SlicerAll`] segment step at which this tile's current run was last extended;
/// [`SlicerAll`]'s direct-build compares it to the current step to decide whether a segment continues
/// the open run or starts a new one. It is unused by [`SlicerOne`] (which builds whole runs at once).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TileBuf<V> {
    tile: TileId,
    verts: Vec<V>,
    run_ends: Vec<u32>,
    feat_ends: Vec<u32>,
    last_id: u32,
    open_step: u64,
}

impl<V> TileBuf<V> {
    fn new(tile: TileId) -> Self {
        Self {
            tile,
            verts: Vec::new(),
            run_ends: Vec::new(),
            feat_ends: Vec::new(),
            last_id: 0,
            open_step: 0,
        }
    }

    /// Append one run's vertices to the arena and record its end offset.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a tile holds far fewer than u32::MAX vertices (a polyline is capped at u16 each)"
    )]
    fn push_run(&mut self, run: Vec<V>) {
        self.verts.extend(run);
        self.run_ends.push(self.verts.len() as u32);
    }

    /// Fold non-empty `runs` into this tile under feature `id`: extend the tile's last feature if it
    /// is already `id` (the open feature reached this tile before), otherwise start a new feature.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "run count per tile stays far below u32::MAX"
    )]
    fn absorb(&mut self, id: u32, runs: Vec<Vec<V>>) {
        debug_assert!(!runs.is_empty(), "empty features are never recorded");
        let extend = self.last_id == id && !self.feat_ends.is_empty();
        for run in runs {
            self.push_run(run);
        }
        if extend {
            *self
                .feat_ends
                .last_mut()
                .expect("extend implies a feature exists") = self.run_ends.len() as u32;
        } else {
            self.feat_ends.push(self.run_ends.len() as u32);
            self.last_id = id;
        }
    }
}

/// Iterate a tile's features from its flat offset arrays, reconstructing each as a [`FeatureView`].
fn features_of<V: Vertex>(
    buf: &TileBuf<V>,
) -> impl Iterator<Item = FeatureView<'_, V>> + use<'_, V> {
    let verts = buf.verts.as_slice();
    let run_ends = buf.run_ends.as_slice();
    let feat_ends = buf.feat_ends.as_slice();
    (0..feat_ends.len()).map(move |f| {
        let rs = if f == 0 { 0 } else { feat_ends[f - 1] as usize };
        let re = feat_ends[f] as usize;
        // Vertex offset where this feature's first run begins (end of the previous feature's run).
        let start = if rs == 0 { 0 } else { run_ends[rs - 1] };
        FeatureView {
            verts,
            run_ends: &run_ends[rs..re],
            start,
        }
    })
}

/// Reserve a fresh feature id and mark it open (for [`SlicerAll::add_feature`]).
fn begin_feature(next: &mut u32, open: &mut Option<u32>) -> u32 {
    let id = *next;
    // Wraps only after u32::MAX features in one slicer — astronomically beyond any real use; wrapping
    // (rather than a debug-panic on overflow) keeps the "never panic" guarantee.
    *next = next.wrapping_add(1);
    *open = Some(id);
    id
}

/// The currently-open feature id, beginning a new one if none is open (for `continue_last_feature`).
fn resume_feature(next: &mut u32, open: &mut Option<u32>) -> u32 {
    match *open {
        Some(id) => id,
        None => begin_feature(next, open),
    }
}

/// A borrowed view of one tile's accumulated features, yielded by [`SlicerAll::iter_tiles`].
pub struct TileView<'a, V: Vertex> {
    buf: &'a TileBuf<V>,
}

impl<'a, V: Vertex> TileView<'a, V> {
    /// The tile this view is for.
    #[must_use]
    pub fn id(&self) -> TileId {
        self.buf.tile
    }

    /// Iterate this tile's features, in the order they were first added.
    pub fn iter_features(&self) -> impl Iterator<Item = FeatureView<'a, V>> + use<'a, V> {
        features_of(self.buf)
    }
}

/// A borrowed view of one feature's clipped polylines within a tile, yielded by
/// [`TileView::iter_features`] / [`SlicerOne::iter_features`].
pub struct FeatureView<'a, V: Vertex> {
    /// The whole tile's vertex arena; polylines are subslices of it.
    verts: &'a [V],
    /// End offsets (into `verts`) of this feature's runs.
    run_ends: &'a [u32],
    /// Vertex offset where this feature's first run begins.
    start: u32,
}

impl<'a, V: Vertex> FeatureView<'a, V> {
    /// Iterate this feature's polylines (runs) in this tile, each a vertex slice in the tile's local
    /// frame. A feature yields several polylines where the input left the tile and re-entered.
    pub fn iter_polylines(&self) -> impl Iterator<Item = &'a [V]> + use<'a, V> {
        let verts = self.verts;
        let mut prev = self.start as usize;
        self.run_ends.iter().map(move |&end| {
            let end = end as usize;
            let run = &verts[prev..end];
            prev = end;
            run
        })
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
    /// Next feature id to hand out.
    next_feature: u32,
    /// The currently-open feature id (for `continue_last_feature`), if any.
    open: Option<u32>,
    /// Per-tile buffers in insertion order; slots are stable (never moved) so `index` can address
    /// them by position.
    tiles: Vec<TileBuf<V>>,
    /// [`TileId`] → slot in `tiles`, ordered so reads come out by tile and find-or-insert is cheap.
    index: BTreeMap<TileId, u32>,
    /// Monotonic segment counter for the direct-build run continuity (see [`RouteSink::emit`]). A gap
    /// is inserted at each polyline boundary so a run never continues across separate polylines.
    step: u64,
}

impl<V: Vertex> SlicerAll<V> {
    /// Create a slicer with the given tile side and buffer, and an empty accumulator.
    ///
    /// # Errors
    ///
    /// - [`SliceError::InvalidDivider`] if `divider` is `0` or greater than `i32::MAX`.
    /// - [`SliceError::BufferTooLarge`] if `buffer` is not strictly less than half the `divider`.
    pub fn new(divider: u32, buffer: u16) -> Result<Self, SliceError> {
        Ok(Self {
            grid: Grid::new(divider, buffer)?,
            next_feature: 0,
            open: None,
            tiles: Vec::new(),
            index: BTreeMap::new(),
            step: 0,
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

    /// Begin a new feature from `polyline`: slice it into every tile it touches, storing its runs as a
    /// fresh feature in each. Chainable.
    ///
    /// Not atomic: pieces are written into the per-tile buffers as the polyline is walked, so on error
    /// the accumulator may hold a partial result — discard it or [`clear`](Self::clear) after an error.
    /// (Errors only arise for pathological input: an oversized polyline, an adversarially wide spread,
    /// or coordinates near the `i32` limits.)
    ///
    /// # Errors
    ///
    /// [`SliceError::PolylineTooLarge`], [`SliceError::TooManyTiles`], or [`SliceError::Overflow`].
    pub fn add_feature<P: AsRef<[V]>>(&mut self, polyline: P) -> Result<&mut Self, SliceError> {
        begin_feature(&mut self.next_feature, &mut self.open);
        let grid = self.grid; // `Grid` is `Copy`, so the walk can borrow `self` mutably as the sink.
        grid.route(polyline.as_ref(), self)?;
        Ok(self)
    }

    /// Extend the feature opened by the last [`add_feature`](Self::add_feature) with another `polyline`
    /// — slice it and fold its per-tile runs into that same feature (so the lines of one multi-line
    /// geometry stay a single feature, as separate runs). If no feature is open yet, this begins one.
    /// Chainable.
    ///
    /// Not atomic (see [`add_feature`](Self::add_feature)).
    ///
    /// # Errors
    ///
    /// [`SliceError::PolylineTooLarge`], [`SliceError::TooManyTiles`], or [`SliceError::Overflow`].
    pub fn continue_last_feature<P: AsRef<[V]>>(
        &mut self,
        polyline: P,
    ) -> Result<&mut Self, SliceError> {
        resume_feature(&mut self.next_feature, &mut self.open);
        let grid = self.grid;
        grid.route(polyline.as_ref(), self)?;
        Ok(self)
    }

    /// The slot of `tile`'s buffer, creating it on first touch.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "tile count stays far below u32::MAX"
    )]
    fn tile_slot(&mut self, tile: TileId) -> usize {
        if let Some(&slot) = self.index.get(&tile) {
            slot as usize
        } else {
            let slot = self.tiles.len() as u32;
            self.tiles.push(TileBuf::new(tile));
            self.index.insert(tile, slot);
            slot as usize
        }
    }

    /// Iterate the touched tiles, ordered by [`TileId`], borrowing so the accumulator can keep
    /// growing afterwards. Each [`TileView`] exposes that tile's features.
    pub fn iter_tiles(&self) -> impl Iterator<Item = TileView<'_, V>> {
        let tiles = self.tiles.as_slice();
        self.index.values().map(move |&slot| TileView {
            buf: &tiles[slot as usize],
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
        self.index.clear();
        self.next_feature = 0;
        self.open = None;
        self.step = 0;
    }
}

impl<V: Vertex> RouteSink<V> for SlicerAll<V> {
    /// A polyline boundary: burn one step so the first segment of this polyline cannot continue a run
    /// left open by the previous polyline (its runs stay separate, even in a shared tile).
    fn begin_polyline(&mut self) {
        self.step = self.step.wrapping_add(1);
    }

    /// Advance to the next segment.
    fn begin_segment(&mut self) {
        self.step = self.step.wrapping_add(1);
    }

    /// Append segment `a`–`c` to `tile`'s buffer (localized by `origin`), extending the tile's open
    /// run if the immediately preceding segment also landed here (same feature), else starting a new
    /// run. The current feature is [`self.open`](Self::open) (set before routing).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "run/vertex counts per tile stay far below u32::MAX"
    )]
    fn emit(&mut self, tile: TileId, origin: Coord<i32>, a: V, c: V) -> Result<(), SliceError> {
        let fid = self.open.expect("a feature is open while routing");
        let step = self.step;
        let slot = self.tile_slot(tile);
        let buf = &mut self.tiles[slot];
        // Continue the open run iff the previous segment (step − 1) also landed in this tile. The
        // per-polyline gap in `begin_polyline` guarantees this never matches across polylines, and a
        // fresh tile (`open_step == 0`) never matches (the first step is ≥ 2 after the gap).
        let continues = buf.open_step == step.wrapping_sub(1);
        let c_local = to_local(c, origin)?;
        if continues {
            buf.verts.push(c_local);
            *buf.run_ends
                .last_mut()
                .expect("continuing implies an open run") = buf.verts.len() as u32;
        } else {
            let a_local = to_local(a, origin)?;
            buf.verts.push(a_local);
            buf.verts.push(c_local);
            buf.run_ends.push(buf.verts.len() as u32);
            if buf.last_id == fid && buf.feat_ends.last().is_some() {
                // Another run in the feature already present here: extend its run span.
                *buf.feat_ends.last_mut().expect("feature present") = buf.run_ends.len() as u32;
            } else {
                // First run of this feature in this tile.
                buf.feat_ends.push(buf.run_ends.len() as u32);
                buf.last_id = fid;
            }
        }
        buf.open_step = step;
        Ok(())
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
    next_feature: u32,
    open: Option<u32>,
    buf: TileBuf<V>,
}

impl<V: Vertex> SlicerOne<V> {
    /// Create a slicer bound to `tile`, with the given tile side and buffer, and an empty
    /// accumulator.
    ///
    /// # Errors
    ///
    /// - [`SliceError::InvalidDivider`] if `divider` is `0` or greater than `i32::MAX`.
    /// - [`SliceError::BufferTooLarge`] if `buffer` is not strictly less than half the `divider`.
    pub fn new(divider: u32, buffer: u16, tile: TileId) -> Result<Self, SliceError> {
        Ok(Self {
            grid: Grid::new(divider, buffer)?,
            next_feature: 0,
            open: None,
            buf: TileBuf::new(tile),
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
        self.buf.tile
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
        let runs = self.grid.slice_one(polyline.as_ref(), self.buf.tile)?;
        let id = begin_feature(&mut self.next_feature, &mut self.open);
        if !runs.is_empty() {
            self.buf.absorb(id, runs);
        }
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
        let runs = self.grid.slice_one(polyline.as_ref(), self.buf.tile)?;
        let id = resume_feature(&mut self.next_feature, &mut self.open);
        if !runs.is_empty() {
            self.buf.absorb(id, runs);
        }
        Ok(self)
    }

    /// Iterate this tile's features, in the order they were first added. Each [`FeatureView`] exposes
    /// that feature's clipped polylines.
    pub fn iter_features(&self) -> impl Iterator<Item = FeatureView<'_, V>> {
        features_of(&self.buf)
    }

    /// Number of features accumulated for the tile.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.feat_ends.len()
    }

    /// Whether nothing has been accumulated yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.feat_ends.is_empty()
    }

    /// Discard everything accumulated so far, keeping the divider/buffer/tile config.
    pub fn clear(&mut self) {
        self.buf = TileBuf::new(self.buf.tile);
        self.next_feature = 0;
        self.open = None;
    }
}
