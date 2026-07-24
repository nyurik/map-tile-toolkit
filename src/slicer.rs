//! The public slicing API: [`SlicerAll`] (accumulate every tile a polyline touches) and
//! [`SlicerOne`] (accumulate a single, fixed tile).
//!
//! Both wrap the same stateless [`Grid`] engine and accumulate results as **features**. Each polyline
//! added with [`add_feature`](SlicerAll::add_feature) is an independent feature; it is sliced and its
//! per-tile runs recorded under that feature. A single polyline can still yield several runs in one
//! tile (it left the tile and re-entered), and those runs stay grouped as that tile's feature.
//!
//! Read the accumulated state back with iterators, never owned `Vec`s:
//!
//! - [`SlicerAll::iter_tiles`] → [`TileView::iter_features`] → [`FeatureView::iter_polylines`];
//! - [`SlicerOne::iter_features`] → [`FeatureView::iter_polylines`] (one implicit tile, so no tile
//!   level).
//!
//! Runs come out in each tile's **local frame**: the tile's `[0, 0]` corner is the origin, so a
//! vertex at global `(x, y)` is `(x − tile.x·extent, y − tile.y·extent)` (in-tile vertices land in
//! `0..extent`; buffer vertices past the low edge go negative). [`merge`](crate::merge) is the
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
/// Only non-empty features are ever recorded, so every `feat_ends` span holds at least one run.
///
/// `open_step` is the [`SlicerAll`] segment step at which this tile was last written; its direct-build
/// compares it to the current step (run continuity) and to the feature's start step (whether this
/// tile already belongs to the feature being added). It is unused by [`SlicerOne`], which builds whole
/// features at once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TileBuf<V> {
    tile: TileId,
    verts: Vec<V>,
    run_ends: Vec<u32>,
    feat_ends: Vec<u32>,
    open_step: u64,
}

impl<V> TileBuf<V> {
    fn new(tile: TileId) -> Self {
        Self {
            tile,
            verts: Vec::new(),
            run_ends: Vec::new(),
            feat_ends: Vec::new(),
            open_step: 0,
        }
    }

    /// Append one run's vertices to the arena and record its end offset.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a tile holds far fewer than u32::MAX vertices (a polyline is capped at u16 each)"
    )]
    fn push_run(&mut self, run: Vec<V>) {
        self.verts.extend(run);
        self.run_ends.push(self.verts.len() as u32);
    }

    /// Record non-empty `runs` as one new feature in this tile (each added polyline is independent).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "run count per tile stays far below u32::MAX"
    )]
    fn absorb(&mut self, runs: Vec<Vec<V>>) {
        debug_assert!(!runs.is_empty(), "empty features are never recorded");
        for run in runs {
            self.push_run(run);
        }
        self.feat_ends.push(self.run_ends.len() as u32);
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
/// payload rides through slicing unchanged. Add each polyline as an independent feature with
/// [`add_feature`](Self::add_feature) and read them back with [`iter_tiles`](Self::iter_tiles).
///
/// The slicer never panics: bad input (an oversized polyline, or coordinates that overflow the tile
/// math) yields an [`SliceError`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicerAll<V: Vertex = Coord<i32>> {
    grid: Grid,
    /// Per-tile buffers in insertion order; slots are stable (never moved) so `index` can address
    /// them by position.
    tiles: Vec<TileBuf<V>>,
    /// [`TileId`] → slot in `tiles`, ordered so reads come out by tile and find-or-insert is cheap.
    index: BTreeMap<TileId, u32>,
    /// Monotonic segment counter for the direct-build run continuity (see [`RouteSink::emit`]). A gap
    /// is inserted at each polyline boundary so a run never continues across separate polylines.
    step: u64,
    /// The `step` value at the start of the feature currently being added (set in
    /// [`begin_polyline`](RouteSink::begin_polyline)). A tile whose `open_step` predates it has not yet
    /// been touched by this feature, so the next run there opens a new feature rather than extending.
    feature_start: u64,
    /// Most-recently resolved `(tile, slot)`, so consecutive writes to the same tile — the common
    /// case for a dense polyline — skip the `index` lookup entirely. Slots are stable, so this never
    /// goes stale; [`clear`](Self::clear) resets it.
    last_slot: Option<(TileId, u32)>,
}

impl<V: Vertex> SlicerAll<V> {
    fn from_grid(grid: Grid) -> Self {
        Self {
            grid,
            tiles: Vec::new(),
            index: BTreeMap::new(),
            step: 0,
            feature_start: 0,
            last_slot: None,
        }
    }

    /// Create a slicer with the given tile side / per-tile output resolution `extent` and `buffer`.
    ///
    /// Coordinates are integers in tile space: a vertex belongs to tile `x.div_euclid(extent)` and is
    /// emitted at `x − tile·extent ∈ [0, extent)`. Project / simplify / affine-scale float source data
    /// into this space (e.g. with `geo`) before slicing.
    ///
    /// # Errors
    ///
    /// - [`SliceError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    /// - [`SliceError::BufferTooLarge`] if `buffer` is not strictly less than half the `extent`.
    pub fn new(extent: u32, buffer: u16) -> Result<Self, SliceError> {
        Ok(Self::from_grid(Grid::new(extent, buffer)?))
    }

    /// The tile side / per-tile output resolution: kept vertices land in `0..extent`.
    #[must_use]
    pub fn extent(&self) -> u32 {
        self.grid.extent()
    }

    /// The buffer kept around every tile, in tile-space units.
    #[must_use]
    pub fn buffer(&self) -> u16 {
        self.grid.buffer()
    }

    /// Add `polyline` as an independent feature: slice it into every tile it touches, storing its runs
    /// as a fresh feature in each. Chainable.
    ///
    /// **Atomic:** the polyline is streamed straight into the per-tile buffers, but on error every
    /// piece written for it is rolled back, so a failed polyline contributes nothing and the
    /// accumulator stays exactly as usable as before the call — safe to skip the offending input and
    /// keep adding. (Errors only arise for pathological input: an oversized polyline, an adversarially
    /// wide spread, or coordinates near the `i32` limits.) An unobservable internal step counter still
    /// advances; use [`clear`](Self::clear) to reset the accumulator entirely.
    ///
    /// # Errors
    ///
    /// [`SliceError::PolylineTooLarge`], [`SliceError::TooManyTiles`], or [`SliceError::Overflow`].
    pub fn add_feature<P: AsRef<[V]>>(&mut self, polyline: P) -> Result<&mut Self, SliceError> {
        let grid = self.grid; // `Grid` is `Copy`, so the walk can borrow `self` mutably as the sink.
        // Savepoint for rollback: how many tiles existed, and the last step used, before this feature.
        // Every piece this feature writes lands in a tile created after `tiles_before`, or bumps an
        // existing tile's `open_step` past `step_before` — so the two mark exactly what to undo.
        let tiles_before = self.tiles.len();
        let step_before = self.step;
        if let Err(err) = grid.route(polyline.as_ref(), self) {
            self.rollback_feature(tiles_before, step_before);
            return Err(err);
        }
        Ok(self)
    }

    /// Undo a partially-written feature after [`add_feature`](Self::add_feature) errored mid-walk,
    /// restoring the observable state (tiles and their features) to the savepoint. Only ever runs on
    /// the rare error path, so an `O(tiles)` scan is fine.
    fn rollback_feature(&mut self, tiles_before: usize, step_before: u64) {
        // Tiles that already existed but this feature appended to (`open_step` moved past the
        // savepoint): drop the single feature span it added, and the runs/vertices behind it.
        for buf in &mut self.tiles[..tiles_before] {
            if buf.open_step > step_before {
                buf.feat_ends.pop();
                let runs_keep = buf.feat_ends.last().copied().unwrap_or(0) as usize;
                let verts_keep = if runs_keep == 0 {
                    0
                } else {
                    buf.run_ends[runs_keep - 1] as usize
                };
                buf.run_ends.truncate(runs_keep);
                buf.verts.truncate(verts_keep);
            }
        }
        // Tiles created during this feature are contiguous at the end: drop them and their index
        // entries. (Their stale `open_step` on kept tiles is harmless — the step counter only climbs,
        // so a later feature's start still outranks it and reads it as a fresh, separate feature.)
        for buf in self.tiles.drain(tiles_before..) {
            self.index.remove(&buf.tile);
        }
        self.last_slot = None;
    }

    /// The slot of `tile`'s buffer, creating it on first touch. A one-entry cache skips the `index`
    /// lookup when the same tile was just written (consecutive segments in a tile — the common case).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "tile count stays far below u32::MAX"
    )]
    fn tile_slot(&mut self, tile: TileId) -> usize {
        if let Some((last, slot)) = self.last_slot
            && last == tile
        {
            return slot as usize;
        }
        let slot = if let Some(&slot) = self.index.get(&tile) {
            slot
        } else {
            let slot = self.tiles.len() as u32;
            self.tiles.push(TileBuf::new(tile));
            self.index.insert(tile, slot);
            slot
        };
        self.last_slot = Some((tile, slot));
        slot as usize
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

    /// Discard everything accumulated so far, keeping the extent/buffer config.
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.index.clear();
        self.step = 0;
        self.feature_start = 0;
        self.last_slot = None;
    }
}

impl<V: Vertex> RouteSink<V> for SlicerAll<V> {
    /// A polyline boundary: burn one step so the first segment of this polyline cannot continue a run
    /// left open by the previous polyline (its runs stay separate, even in a shared tile), and record
    /// that step as this feature's start so `emit` knows which tiles it has already reached.
    fn begin_polyline(&mut self) {
        self.step = self.step.wrapping_add(1);
        self.feature_start = self.step;
    }

    /// Advance to the next segment.
    fn begin_segment(&mut self) {
        self.step = self.step.wrapping_add(1);
    }

    /// Append segment `a`–`c` to `tile`'s buffer (localized by `origin`), extending the tile's open
    /// run if the immediately preceding segment also landed here, else starting a new run — and, when
    /// a new run, opening a new feature unless this feature already reached the tile (a re-entry).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "run/vertex counts per tile stay far below u32::MAX"
    )]
    fn emit(&mut self, tile: TileId, origin: Coord<i32>, a: V, c: V) -> Result<(), SliceError> {
        let step = self.step;
        let feature_start = self.feature_start;
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
            // Every step of this feature is ≥ `feature_start`, so an earlier `open_step` means this
            // tile has not yet been touched by the current feature → open a new one. Otherwise the
            // feature already has a run here (it left and re-entered) → extend that run span.
            if buf.open_step < feature_start {
                buf.feat_ends.push(buf.run_ends.len() as u32);
            } else {
                *buf.feat_ends
                    .last_mut()
                    .expect("a prior run this feature implies a feature span") =
                    buf.run_ends.len() as u32;
            }
        }
        buf.open_step = step;
        Ok(())
    }
}

/// Slices integer polylines into pieces for **one fixed tile**, accumulating only that tile.
///
/// The single-tile counterpart to [`SlicerAll`]: [`add_feature`](Self::add_feature) works exactly the
/// same, but each polyline is clipped only to this slicer's [`tile`](Self::tile). Because there is a
/// single tile, the read API skips the tile level — [`iter_features`](Self::iter_features) yields the
/// features directly.
///
/// Generic over the [`Vertex`] type `V` (defaults to [`Coord<i32>`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicerOne<V: Vertex = Coord<i32>> {
    grid: Grid,
    buf: TileBuf<V>,
}

impl<V: Vertex> SlicerOne<V> {
    fn from_grid(grid: Grid, tile: TileId) -> Self {
        Self {
            grid,
            buf: TileBuf::new(tile),
        }
    }

    /// Create a slicer bound to `tile`, with the given tile side / per-tile output resolution `extent`
    /// and `buffer` (see [`SlicerAll::new`](crate::SlicerAll::new) for the coordinate model).
    ///
    /// # Errors
    ///
    /// - [`SliceError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    /// - [`SliceError::BufferTooLarge`] if `buffer` is not strictly less than half the `extent`.
    pub fn new(extent: u32, buffer: u16, tile: TileId) -> Result<Self, SliceError> {
        Ok(Self::from_grid(Grid::new(extent, buffer)?, tile))
    }

    /// The tile side / per-tile output resolution: kept vertices land in `0..extent`.
    #[must_use]
    pub fn extent(&self) -> u32 {
        self.grid.extent()
    }

    /// The buffer kept around every tile, in tile-space units.
    #[must_use]
    pub fn buffer(&self) -> u16 {
        self.grid.buffer()
    }

    /// The tile this slicer clips into.
    #[must_use]
    pub fn tile(&self) -> TileId {
        self.buf.tile
    }

    /// Add `polyline` as an independent feature, clipped to this slicer's [`tile`](Self::tile).
    /// Chainable. A feature is recorded only if something of `polyline` lands in the tile.
    ///
    /// Atomic: the polyline is fully sliced first, so on error the accumulator is left unchanged.
    ///
    /// # Errors
    ///
    /// [`SliceError::Overflow`] if the tile's box or a kept vertex overflows `i32`.
    pub fn add_feature<P: AsRef<[V]>>(&mut self, polyline: P) -> Result<&mut Self, SliceError> {
        let runs = self.grid.slice_one(polyline.as_ref(), self.buf.tile)?;
        if !runs.is_empty() {
            self.buf.absorb(runs);
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

    /// Discard everything accumulated so far, keeping the extent/buffer/tile config.
    pub fn clear(&mut self) {
        self.buf = TileBuf::new(self.buf.tile);
    }
}
