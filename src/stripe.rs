//! Eager stripe-slicer (geojson-vt / planetiler style).
//!
//! Slices one polygon or polyline into the set of tiles it touches at a zoom, producing
//! per-tile clipped coordinate sequences in tile-local space, plus the set of fully-filled
//! interior tiles. Neighboring tiles overlap slightly because of the clip buffer.
//!
//! Ported from planetiler's `TiledGeometry` / `GeometryCoordinateSequences` /
//! `MutableCoordinateSequence`, which are in turn adapted from mapbox/geojson-vt.
//!
//! Coordinate model:
//! * input geometry/coordinate-sequences are in "world scaled to `2^zoom` tiles" (1 unit =
//!   1 tile);
//! * output per-tile coordinates are tile-local in `0..`[`TILE_SIZE`];
//! * `buffer` is a fraction of a tile (e.g. `0.1`, or `buffer_pixels / TILE_SIZE`).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::float_cmp,
    clippy::collapsible_if,
    clippy::items_after_statements,
    clippy::map_entry,
    clippy::unnecessary_wraps,
    clippy::single_match_else,
    reason = "faithful numeric port of planetiler's stripe-clipping algorithm"
)]

use std::collections::{BTreeMap, BTreeSet};

use geo_types::{Coord, Geometry, LineString};

use crate::TileId;
use crate::extents::ForZoom;

/// Side length of a tile's local coordinate space (planetiler's `SIZE`).
pub const TILE_SIZE: f64 = 256.0;

const NEIGHBOR_BUFFER_EPS: f64 = 0.1 / 4096.0;

/// One input geometry expressed as groups of coordinate sequences: for an area, each group
/// is `[exterior, hole, hole, …]`; for a line, `[line]`. Coordinates are in `2^zoom` tile
/// units on input and tile-local `0..TILE_SIZE` on output.
pub type CoordSeqGroups = Vec<Vec<LineString<f64>>>;

/// Error raised while slicing, mirroring planetiler's `GeometryException`.
#[derive(Debug, thiserror::Error)]
pub enum GeometryError {
    /// A polygon hole implied an interior fill that the shell does not actually cover, so
    /// the caller must repair the geometry and retry.
    #[error("bad polygon fill: {0}")]
    BadPolygonFill(String),
}

// ---------------------------------------------------------------------------
// A growable coordinate sequence with optional translate+scale-on-insert and dedup.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MutSeq {
    pts: Vec<Coord<f64>>,
    scale: f64,
    rel_x: f64,
    rel_y: f64,
}

impl MutSeq {
    fn new() -> Self {
        Self {
            pts: Vec::new(),
            scale: 1.0,
            rel_x: 0.0,
            rel_y: 0.0,
        }
    }

    fn new_scaling(rel_x: f64, rel_y: f64, scale: f64) -> Self {
        Self {
            pts: Vec::new(),
            scale,
            rel_x,
            rel_y,
        }
    }

    fn add_point(&mut self, x: f64, y: f64) {
        let sx = self.scale * (x - self.rel_x);
        let sy = self.scale * (y - self.rel_y);
        match self.pts.last() {
            Some(last) if last.x == sx && last.y == sy => {}
            _ => self.pts.push(Coord { x: sx, y: sy }),
        }
    }

    fn close_ring(&mut self) {
        if let (Some(&first), Some(&last)) = (self.pts.first(), self.pts.last()) {
            if first.x != last.x || first.y != last.y {
                self.pts.push(first);
            }
        }
    }

    fn size(&self) -> usize {
        self.pts.len()
    }

    fn to_line_string(&self) -> LineString<f64> {
        LineString(self.pts.clone())
    }
}

/// The whole-tile fill ring for a buffered tile, in tile-local pixel space.
fn fill_seq(buffer: f64) -> MutSeq {
    let min = -TILE_SIZE * buffer;
    let max = TILE_SIZE - min;
    MutSeq {
        pts: vec![
            Coord { x: min, y: min },
            Coord { x: max, y: min },
            Coord { x: max, y: max },
            Coord { x: min, y: max },
            Coord { x: min, y: min },
        ],
        scale: 1.0,
        rel_x: 0.0,
        rel_y: 0.0,
    }
}

// ---------------------------------------------------------------------------
// A set of integer Y values supporting the even-odd fill bookkeeping.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct RangeSet {
    set: BTreeSet<i32>,
}

impl RangeSet {
    /// Toggle membership of the inclusive range `[lo, hi]`.
    fn xor(&mut self, lo: i32, hi: i32) {
        for y in lo..=hi {
            if !self.set.remove(&y) {
                self.set.insert(y);
            }
        }
    }
    fn contains(&self, y: i32) -> bool {
        self.set.contains(&y)
    }
    fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
    fn intersect(&self, other: &Self) -> Self {
        Self {
            set: self.set.intersection(&other.set).copied().collect(),
        }
    }
    fn add_all(&mut self, other: &Self) {
        self.set.extend(other.set.iter().copied());
    }
    fn remove_all(&mut self, other: &Self) {
        for y in &other.set {
            self.set.remove(y);
        }
    }
    fn iter(&self) -> impl Iterator<Item = i32> + '_ {
        self.set.iter().copied()
    }
}

// ---------------------------------------------------------------------------
// Covered tiles
// ---------------------------------------------------------------------------

/// The set of tiles a geometry touches at a given zoom, mirroring planetiler `CoveredTiles`.
#[derive(Debug, Default, Clone)]
pub struct CoveredTiles {
    z: u8,
    tiles: BTreeSet<TileId>,
}

impl CoveredTiles {
    fn new(z: u8) -> Self {
        Self {
            z,
            tiles: BTreeSet::new(),
        }
    }

    /// Whether the tile at `(x, y)` is covered.
    #[must_use]
    pub fn test(&self, x: u32, y: u32) -> bool {
        self.tiles.contains(&TileId::new(x, y, self.z))
    }

    /// Iterate the covered tiles in row-major (x, then y) order.
    pub fn iter(&self) -> impl Iterator<Item = TileId> + '_ {
        self.tiles.iter().copied()
    }

    /// The union of two covered-tile sets at the same zoom.
    #[must_use]
    pub fn merge(mut self, other: &Self) -> Self {
        self.tiles.extend(other.tiles.iter().copied());
        self
    }
}

impl<'a> IntoIterator for &'a CoveredTiles {
    type Item = TileId;
    type IntoIter = std::iter::Copied<std::collections::btree_set::Iter<'a, TileId>>;

    fn into_iter(self) -> Self::IntoIter {
        self.tiles.iter().copied()
    }
}

// ---------------------------------------------------------------------------
// TiledGeometry
// ---------------------------------------------------------------------------

/// A geometry sliced into per-tile pieces, mirroring planetiler `TiledGeometry`.
#[derive(Debug)]
pub struct TiledGeometry {
    extents: ForZoom,
    buffer: f64,
    neighbor_buffer: f64,
    z: u8,
    area: bool,
    max_tiles: i32,
    tile_contents: BTreeMap<TileId, CoordSeqGroups>,
    /// Per-X-column filled Y ranges (`None` until a polygon fill is found).
    filled_ranges: Option<BTreeMap<i32, RangeSet>>,
}

/// Which world-copy edge content spilled past, for antimeridian wrapping.
#[derive(Default, Clone, Copy)]
struct Overflow {
    left: bool,
    right: bool,
}

impl TiledGeometry {
    fn new(extents: &ForZoom, buffer: f64, z: u8, area: bool) -> Self {
        Self {
            extents: extents.clone(),
            buffer,
            neighbor_buffer: buffer + NEIGHBOR_BUFFER_EPS,
            z,
            area,
            max_tiles: 1 << z,
            tile_contents: BTreeMap::new(),
            filled_ranges: None,
        }
    }

    /// Enumerate the tiles a geometry touches at `zoom`, without producing clipped output.
    ///
    /// # Errors
    /// Propagates [`GeometryError`] from slicing (e.g. an unfillable polygon hole).
    pub fn get_covered_tiles(
        geom: &Geometry<f64>,
        zoom: u8,
        extents: &ForZoom,
    ) -> Result<CoveredTiles, GeometryError> {
        match geom {
            Geometry::GeometryCollection(gc) => {
                let mut result = CoveredTiles::new(zoom);
                for g in &gc.0 {
                    result = result.merge(&Self::get_covered_tiles(g, zoom, extents)?);
                }
                Ok(result)
            }
            _ => Ok(Self::slice_geometry(geom, 0.0, 0.0, zoom, extents)?.covered_tiles()),
        }
    }

    /// Dispatch a geometry to the appropriate slicing path (planetiler `sliceIntoTiles`).
    ///
    /// # Errors
    /// Propagates [`GeometryError`] from polygon slicing.
    pub fn slice_geometry(
        geom: &Geometry<f64>,
        min_size: f64,
        buffer: f64,
        z: u8,
        extents: &ForZoom,
    ) -> Result<Self, GeometryError> {
        match geom {
            Geometry::Point(p) => Self::slice_points_into_tiles(&[p.0], buffer, z, extents),
            Geometry::MultiPoint(mp) => {
                let coords: Vec<Coord<f64>> = mp.0.iter().map(|p| p.0).collect();
                Self::slice_points_into_tiles(&coords, buffer, z, extents)
            }
            Geometry::LineString(_)
            | Geometry::MultiLineString(_)
            | Geometry::Polygon(_)
            | Geometry::MultiPolygon(_) => {
                let groups = extract_groups(geom, min_size);
                let area = matches!(geom, Geometry::Polygon(_) | Geometry::MultiPolygon(_));
                Self::slice_into_tiles(&groups, buffer, area, z, extents)
            }
            Geometry::GeometryCollection(_) => {
                // Collections are flattened by the caller for covered-tile queries.
                Ok(Self::new(extents, buffer, z, false))
            }
            _ => Ok(Self::new(extents, buffer, z, false)),
        }
    }

    /// Slice a set of points into every tile they touch (points are replicated into all
    /// buffered neighbor tiles). Mirrors planetiler's `slicePointsIntoTiles`.
    ///
    /// # Errors
    /// Never fails today; returns `Result` for API symmetry with the other slicers.
    pub fn slice_points_into_tiles(
        coords: &[Coord<f64>],
        buffer: f64,
        z: u8,
        extents: &ForZoom,
    ) -> Result<Self, GeometryError> {
        let mut result = Self::new(extents, buffer, z, false);
        for coord in coords {
            result.slice_point(*coord);
        }
        Ok(result)
    }

    /// Slice pre-extracted coordinate-sequence groups into every tile they touch.
    ///
    /// `area` selects polygon semantics (rings stay closed, interior fill is inferred) vs.
    /// line semantics (segments are dropped on exit so re-entry starts a fresh line).
    ///
    /// # Errors
    /// Returns [`GeometryError::BadPolygonFill`] for a polygon whose hole cannot be filled.
    pub fn slice_into_tiles(
        groups: &CoordSeqGroups,
        buffer: f64,
        area: bool,
        z: u8,
        extents: &ForZoom,
    ) -> Result<Self, GeometryError> {
        let mut result = Self::new(extents, buffer, z, area);
        let wrap = result.slice_world_copy(groups, 0)?;
        if wrap.right {
            result.slice_world_copy(groups, -result.max_tiles)?;
        }
        if wrap.left {
            result.slice_world_copy(groups, result.max_tiles)?;
        }
        Ok(result)
    }

    /// The tiles this geometry touches (partial + filled).
    #[must_use]
    pub fn covered_tiles(&self) -> CoveredTiles {
        let mut covered = CoveredTiles::new(self.z);
        covered.tiles.extend(self.tile_contents.keys().copied());
        if let Some(ranges) = &self.filled_ranges {
            for (&x, ys) in ranges {
                if let Ok(xu) = u32::try_from(x) {
                    for y in ys.iter() {
                        if let Ok(yu) = u32::try_from(y) {
                            covered.tiles.insert(TileId::new(xu, yu, self.z));
                        }
                    }
                }
            }
        }
        covered
    }

    /// Tiles fully covered by a polygon interior, carrying no partial detail.
    pub fn filled_tiles(&self) -> impl Iterator<Item = TileId> {
        let mut out = Vec::new();
        if let Some(ranges) = &self.filled_ranges {
            for (&x, ys) in ranges {
                for y in ys.iter() {
                    if self.extents.test(x, y) {
                        if let (Ok(xu), Ok(yu)) = (u32::try_from(x), u32::try_from(y)) {
                            let tile = TileId::new(xu, yu, self.z);
                            if !self.tile_contents.contains_key(&tile) {
                                out.push(tile);
                            }
                        }
                    }
                }
            }
        }
        out.into_iter()
    }

    /// Per-tile clipped rings/lines in tile-local `0..TILE_SIZE` coordinates.
    #[must_use]
    pub fn tile_data(&self) -> &BTreeMap<TileId, CoordSeqGroups> {
        &self.tile_contents
    }

    /// Pack a tile `(x, y)` into a single integer for a zoom with `max_tiles_at_zoom` tiles
    /// per axis (planetiler's internal encoding; wraps to 32 bits like Java's `int`).
    #[must_use]
    pub fn encode(max_tiles_at_zoom: u64, x: u32, y: u32) -> i32 {
        let v = max_tiles_at_zoom
            .wrapping_mul(u64::from(x))
            .wrapping_add(u64::from(y));
        (v as u32) as i32
    }

    /// Inverse of [`Self::encode`].
    #[must_use]
    pub fn decode(max_tiles_at_zoom: u64, encoded: u64, z: u8) -> TileId {
        TileId::new(
            (encoded / max_tiles_at_zoom) as u32,
            (encoded % max_tiles_at_zoom) as u32,
            z,
        )
    }

    // -- slicing internals --------------------------------------------------

    fn slice_point(&mut self, coord: Coord<f64>) {
        let (world_x, world_y) = (coord.x, coord.y);
        let min_x = (world_x - self.neighbor_buffer).floor() as i32;
        let max_x = (world_x + self.neighbor_buffer).floor() as i32;
        let min_y = self
            .extents
            .min_y()
            .max((world_y - self.neighbor_buffer).floor() as i32);
        let max_y = (self.extents.max_y() - 1).min((world_y + self.neighbor_buffer).floor() as i32);
        for x in min_x..=max_x {
            let tile_x = world_x - f64::from(x);
            let wrapped_x = wrap_int(x, self.max_tiles);
            if self.extents.test_x(wrapped_x) {
                for y in min_y..=max_y {
                    if self.extents.test(wrapped_x, y) {
                        if let (Ok(xu), Ok(yu)) = (u32::try_from(wrapped_x), u32::try_from(y)) {
                            let tile = TileId::new(xu, yu, self.z);
                            let tile_y = world_y - f64::from(y);
                            let groups = self
                                .tile_contents
                                .entry(tile)
                                .or_insert_with(|| vec![Vec::new()]);
                            groups[0].push(LineString(vec![Coord {
                                x: tile_x * TILE_SIZE,
                                y: tile_y * TILE_SIZE,
                            }]));
                        }
                    }
                }
            }
        }
    }

    fn slice_world_copy(
        &mut self,
        groups: &CoordSeqGroups,
        x_offset: i32,
    ) -> Result<Overflow, GeometryError> {
        let mut overflow = Overflow::default();
        for group in groups {
            let mut in_progress: BTreeMap<TileId, Vec<MutSeq>> = BTreeMap::new();
            for (i, segment) in group.iter().enumerate() {
                let is_outer = i == 0;
                let x_slices = self.slice_x(segment);
                for (key, stripes) in x_slices {
                    let x = key + x_offset;
                    if x >= self.max_tiles {
                        overflow.right = true;
                    } else if x < 0 {
                        overflow.left = true;
                    } else {
                        for stripe in &stripes {
                            let filled = self.slice_y(stripe, x, is_outer, &mut in_progress)?;
                            if self.area {
                                if let Some(range) = filled {
                                    if is_outer {
                                        self.add_filled_range(x, &range);
                                    } else {
                                        self.remove_filled_range(x, &range);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            self.add_shape_to_results(in_progress);
        }
        Ok(overflow)
    }

    fn add_shape_to_results(&mut self, in_progress: BTreeMap<TileId, Vec<MutSeq>>) {
        for (tile, seqs) in in_progress {
            if self.area && seqs.first().is_none_or(|s| s.size() < 4) {
                continue;
            }
            let min_points = if self.area { 4 } else { 2 };
            let out: Vec<LineString<f64>> = seqs
                .iter()
                .filter(|s| s.size() >= min_points)
                .map(MutSeq::to_line_string)
                .collect();
            if !out.is_empty() && self.extents.test(tile.x as i32, tile.y as i32) {
                self.tile_contents.entry(tile).or_default().push(out);
            }
        }
    }

    /// Slice one segment into vertical stripes, keyed by tile X column.
    fn slice_x(&self, segment: &LineString<f64>) -> BTreeMap<i32, Vec<MutSeq>> {
        let left = -self.buffer;
        let right = 1.0 + self.buffer;
        let mut new_geoms: BTreeMap<i32, Vec<MutSeq>> = BTreeMap::new();
        // x -> index of the currently-open slice within new_geoms[x]
        let mut open: BTreeMap<i32, usize> = BTreeMap::new();
        let pts = &segment.0;
        if pts.is_empty() {
            return new_geoms;
        }

        for w in pts.windows(2) {
            let (ax, ay, bx, by) = (w[0].x, w[0].y, w[1].x, w[1].y);
            let start_x = (ax.min(bx) - self.neighbor_buffer).floor() as i32;
            let end_x = (ax.max(bx) + self.neighbor_buffer).floor() as i32;
            for x in start_x..=end_x {
                let ax_tile = ax - f64::from(x);
                let bx_tile = bx - f64::from(x);
                let idx = match open.get(&x) {
                    Some(&i) => i,
                    None => {
                        let v = new_geoms.entry(x).or_default();
                        v.push(MutSeq::new());
                        let i = v.len() - 1;
                        open.insert(x, i);
                        i
                    }
                };
                let slice = &mut new_geoms.get_mut(&x).expect("slice column exists")[idx];
                let mut exited = false;
                if ax_tile < left {
                    if bx_tile > left {
                        intersect_x(slice, ax_tile, ay, bx_tile, by, left);
                    }
                } else if ax_tile > right {
                    if bx_tile < right {
                        intersect_x(slice, ax_tile, ay, bx_tile, by, right);
                    }
                } else {
                    slice.add_point(ax_tile, ay);
                }
                if bx_tile < left && ax_tile >= left {
                    intersect_x(slice, ax_tile, ay, bx_tile, by, left);
                    exited = true;
                }
                if bx_tile > right && ax_tile <= right {
                    intersect_x(slice, ax_tile, ay, bx_tile, by, right);
                    exited = true;
                }
                if !self.area && exited {
                    open.remove(&x);
                }
            }
        }

        // add the last point
        let last = pts[pts.len() - 1];
        let (ax, ay) = (last.x, last.y);
        let start_x = (ax - self.neighbor_buffer).floor() as i32;
        let end_x = (ax + self.neighbor_buffer).floor() as i32;
        for x in (start_x - 1)..=(end_x + 1) {
            let ax_tile = ax - f64::from(x);
            if let Some(&idx) = open.get(&x) {
                if ax_tile >= left && ax_tile <= right {
                    new_geoms.get_mut(&x).expect("open column exists")[idx].add_point(ax_tile, ay);
                }
            }
        }

        if self.area {
            for slices in new_geoms.values_mut() {
                for s in slices.iter_mut() {
                    s.close_ring();
                }
            }
        }
        let max = self.max_tiles;
        let extents = &self.extents;
        new_geoms.retain(|&x, _| extents.test_x(wrap_x(x, max)));
        new_geoms
    }

    /// Split a vertical X-column stripe into Y rows, storing detail in `in_progress` and
    /// returning the filled Y range for a polygon (planetiler `sliceY`).
    fn slice_y(
        &self,
        stripe: &MutSeq,
        x: i32,
        outer: bool,
        in_progress: &mut BTreeMap<TileId, Vec<MutSeq>>,
    ) -> Result<Option<RangeSet>, GeometryError> {
        let pts = &stripe.pts;
        if pts.is_empty() || x < 0 || x >= self.max_tiles {
            return Ok(None);
        }
        let xu = x as u32;
        let left_edge = -self.buffer;
        let right_edge = 1.0 + self.buffer;

        let mut tile_ys_with_detail: Option<BTreeSet<i32>> = None;
        let mut right_filled: Option<RangeSet> = None;
        let mut left_filled: Option<RangeSet> = None;
        // y -> index of the open slice within in_progress[tile(x,y)]
        let mut y_slices: BTreeMap<i32, usize> = BTreeMap::new();

        struct Skipped {
            left: bool,
            lo: i32,
            hi: i32,
            asc: bool,
        }
        let mut skipped: Vec<Skipped> = Vec::new();

        let extent_min_y = self.extents.min_y();
        let extent_max_y = self.extents.max_y();

        for w in pts.windows(2) {
            let (ax, ay, bx, by) = (w[0].x, w[0].y, w[1].x, w[1].y);
            let min_y = ay.min(by);
            let max_y = ay.max(by);
            let start_y = extent_min_y.max((min_y - self.neighbor_buffer).floor() as i32);
            let end_start_y = extent_min_y.max((min_y + self.neighbor_buffer).floor() as i32);
            let start_end_y = (extent_max_y - 1).min((max_y - self.neighbor_buffer).floor() as i32);
            let end_y = (extent_max_y - 1).min((max_y + self.neighbor_buffer).floor() as i32);

            let on_right_edge = self.area && ax == bx && ax == right_edge;
            let on_left_edge = self.area && ax == bx && ax == left_edge;

            let mut y = start_y;
            while y <= end_y {
                // skip over filled tiles until the next tile that already has detail
                if self.area
                    && y > end_start_y
                    && y < start_end_y
                    && (on_right_edge || on_left_edge)
                {
                    let detail = tile_ys_with_detail
                        .get_or_insert_with(|| y_slices.keys().copied().collect());
                    let next = detail.range(y..).next().copied();
                    let next_non_edge = next.map_or(start_end_y, |n| n.min(start_end_y));
                    let end_skip = next_non_edge - 1;
                    if end_skip >= y {
                        skipped.push(Skipped {
                            left: on_left_edge,
                            lo: y,
                            hi: end_skip,
                            asc: by > ay,
                        });
                        if right_filled.is_none() {
                            right_filled = Some(RangeSet::default());
                            left_filled = Some(RangeSet::default());
                        }
                        let target = if on_right_edge {
                            right_filled.as_mut()
                        } else {
                            left_filled.as_mut()
                        };
                        if let Some(rs) = target {
                            rs.xor(y, end_skip);
                        }
                        y = next_non_edge;
                    }
                }

                // emit linestring/polygon ring detail
                let top_limit = f64::from(y) - self.buffer;
                let bottom_limit = f64::from(y) + 1.0 + self.buffer;
                let tile = TileId::new(xu, y as u32, self.z);

                if !y_slices.contains_key(&y) {
                    if let Some(detail) = tile_ys_with_detail.as_mut() {
                        detail.insert(y);
                    }
                    let entry = in_progress.entry(tile).or_default();
                    // infer a fill if a hole is the first thing to touch a filled interior tile
                    if self.area && !outer && entry.is_empty() {
                        if !self.is_filled(x, y) {
                            return Err(GeometryError::BadPolygonFill(format!(
                                "{x}, {y} is not filled!"
                            )));
                        }
                        entry.push(fill_seq(self.buffer));
                    }
                    entry.push(MutSeq::new_scaling(0.0, f64::from(y), TILE_SIZE));
                    let idx = entry.len() - 1;
                    y_slices.insert(y, idx);

                    // backfill edges skipped for this now-detailed tile
                    let contains = left_filled.as_ref().is_some_and(|l| l.contains(y))
                        || right_filled.as_ref().is_some_and(|r| r.contains(y));
                    if self.area && contains {
                        for s in &skipped {
                            if s.lo <= y && s.hi >= y {
                                let top = f64::from(y) - self.buffer;
                                let bottom = f64::from(y) + 1.0 + self.buffer;
                                let (start, end) =
                                    if s.asc { (top, bottom) } else { (bottom, top) };
                                let edge_x = if s.left {
                                    -self.buffer
                                } else {
                                    1.0 + self.buffer
                                };
                                let slice = &mut in_progress.get_mut(&tile).expect("tile")[idx];
                                slice.add_point(edge_x, start);
                                slice.add_point(edge_x, end);
                            }
                        }
                    }
                }

                let idx = *y_slices.get(&y).expect("y slice exists");
                let slice = &mut in_progress.get_mut(&tile).expect("tile exists")[idx];
                let mut exited = false;
                if ay < top_limit {
                    if by > top_limit {
                        intersect_y(slice, ax, ay, bx, by, top_limit);
                    }
                } else if ay > bottom_limit {
                    if by < bottom_limit {
                        intersect_y(slice, ax, ay, bx, by, bottom_limit);
                    }
                } else {
                    slice.add_point(ax, ay);
                }
                if by < top_limit && ay >= top_limit {
                    intersect_y(slice, ax, ay, bx, by, top_limit);
                    exited = true;
                }
                if by > bottom_limit && ay <= bottom_limit {
                    intersect_y(slice, ax, ay, bx, by, bottom_limit);
                    exited = true;
                }
                if !self.area && exited {
                    y_slices.remove(&y);
                }
                y += 1;
            }
        }

        // add the last point
        let last = pts[pts.len() - 1];
        let (ax, ay) = (last.x, last.y);
        let start_y = (ay - self.neighbor_buffer).floor() as i32;
        let end_y = (ay + self.neighbor_buffer).floor() as i32;
        for y in (start_y - 1)..=(end_y + 1) {
            let k1 = f64::from(y) - self.buffer;
            let k2 = f64::from(y) + 1.0 + self.buffer;
            if ay >= k1 && ay <= k2 {
                if let Some(&idx) = y_slices.get(&y) {
                    let tile = TileId::new(xu, y as u32, self.z);
                    in_progress.get_mut(&tile).expect("tile exists")[idx].add_point(ax, ay);
                }
            }
        }

        if self.area {
            close_open_rings(&y_slices, xu, self.z, in_progress);
        }

        Ok(match (right_filled, left_filled) {
            (Some(r), Some(l)) => Some(r.intersect(&l)),
            _ => None,
        })
    }

    fn add_filled_range(&mut self, x: i32, range: &RangeSet) {
        if range.is_empty() {
            return;
        }
        let ranges = self.filled_ranges.get_or_insert_with(BTreeMap::new);
        ranges.entry(x).or_default().add_all(range);
    }

    fn remove_filled_range(&mut self, x: i32, range: &RangeSet) {
        if range.is_empty() {
            return;
        }
        let ranges = self.filled_ranges.get_or_insert_with(BTreeMap::new);
        if let Some(existing) = ranges.get_mut(&x) {
            existing.remove_all(range);
        }
    }

    fn is_filled(&self, x: i32, y: i32) -> bool {
        self.filled_ranges
            .as_ref()
            .and_then(|r| r.get(&x))
            .is_some_and(|col| col.contains(y))
    }
}

/// Close every open ring in `y_slices` (borrow-checker-friendly helper for [`TiledGeometry::slice_y`]).
fn close_open_rings(
    y_slices: &BTreeMap<i32, usize>,
    xu: u32,
    z: u8,
    in_progress: &mut BTreeMap<TileId, Vec<MutSeq>>,
) {
    for (&y, &idx) in y_slices {
        let tile = TileId::new(xu, y as u32, z);
        if let Some(seqs) = in_progress.get_mut(&tile) {
            if let Some(slice) = seqs.get_mut(idx) {
                slice.close_ring();
            }
        }
    }
}

fn intersect_x(out: &mut MutSeq, ax: f64, ay: f64, bx: f64, by: f64, x: f64) {
    let t = (x - ax) / (bx - ax);
    out.add_point(x, ay + (by - ay) * t);
}

fn intersect_y(out: &mut MutSeq, ax: f64, ay: f64, bx: f64, by: f64, y: f64) {
    let t = (y - ay) / (by - ay);
    out.add_point(ax + (bx - ax) * t, y);
}

fn wrap_int(value: i32, max: i32) -> i32 {
    let mut v = value % max;
    if v < 0 {
        v += max;
    }
    v
}

fn wrap_x(x: i32, max: i32) -> i32 {
    wrap_int(x, max)
}

// ---------------------------------------------------------------------------
// Geometry -> coordinate-sequence groups (planetiler GeometryCoordinateSequences)
// ---------------------------------------------------------------------------

/// Doubled shoelace signed area; `> 0` is CCW in a y-up frame.
fn signed_area2(ring: &LineString<f64>) -> f64 {
    let mut acc = 0.0;
    for w in ring.0.windows(2) {
        acc += w[0].x * w[1].y - w[1].x * w[0].y;
    }
    acc
}

fn to_ccw(mut ring: LineString<f64>) -> LineString<f64> {
    if signed_area2(&ring) < 0.0 {
        ring.0.reverse();
    }
    ring
}

fn line_length(ls: &LineString<f64>) -> f64 {
    ls.0.windows(2)
        .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
        .sum()
}

/// Extract slicing groups from a geometry, normalizing all rings to CCW and dropping
/// linestrings/rings below `min_size` (planetiler `extractGroups`).
fn extract_groups(geom: &Geometry<f64>, min_size: f64) -> CoordSeqGroups {
    let mut out = Vec::new();
    extract_into(geom, min_size, &mut out);
    out
}

fn extract_into(geom: &Geometry<f64>, min_size: f64, out: &mut CoordSeqGroups) {
    match geom {
        Geometry::GeometryCollection(gc) => {
            for g in &gc.0 {
                extract_into(g, min_size, out);
            }
        }
        Geometry::Polygon(p) => extract_polygon(p, min_size, out),
        Geometry::MultiPolygon(mp) => {
            for p in &mp.0 {
                extract_polygon(p, min_size, out);
            }
        }
        Geometry::LineString(ls) => {
            if line_length(ls) >= min_size {
                out.push(vec![ls.clone()]);
            }
        }
        Geometry::MultiLineString(mls) => {
            for ls in &mls.0 {
                if line_length(ls) >= min_size {
                    out.push(vec![ls.clone()]);
                }
            }
        }
        _ => {}
    }
}

fn extract_polygon(polygon: &geo_types::Polygon<f64>, min_area: f64, out: &mut CoordSeqGroups) {
    let outer = polygon.exterior();
    if (signed_area2(outer) / 2.0).abs() >= min_area {
        let mut group = vec![to_ccw(outer.clone())];
        for inner in polygon.interiors() {
            if (signed_area2(inner) / 2.0).abs() >= min_area {
                group.push(to_ccw(inner.clone()));
            }
        }
        out.push(group);
    }
}
