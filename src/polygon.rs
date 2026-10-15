//! Polygon slicing for [`Slicer`](crate::Slicer): [`Slicer::polygon`](crate::Slicer::polygon)
//! yields every piece of a polygon in every tile it reaches, and
//! [`Slicer::tile_polygon`](crate::Slicer::tile_polygon) its piece in one tile.
//!
//! A polygon is sliced lazily by a row-by-row scanline (see `docs/polygon-slicer.md` §6/§8):
//!
//! 1. **Prepare** — one pass over the polygon's rings, borrowing them in place: drop degenerate rings,
//!    take the bounding box, and record the `y`-range of every run of [`CHUNK`] consecutive edges. A
//!    polygon inside one tile's inner box is yielded whole, without the rest.
//! 2. **Scan rows** top to bottom. A row looks only at the chunks whose `y`-range reaches it: their
//!    edges that touch the row's buffered slab give the row's tile spans, and those crossing the row's
//!    center line `y = cy` give its crossings. The center line carries the winding ray of every tile in
//!    the row, so the crossings answer both remaining questions: how far an excursion outside a tile
//!    winds around it (the detour's shape), and whether a ring with no edge in a tile contains it.
//! 3. **Sweep** the row left to right over its edge tiles, closing every touching ring's arcs with the
//!    shared [`close_ring`] core, and yielding the covered spans between them as [`FillRun`]s. A
//!    Fenwick tree over the row's crossings (in edge order) answers "signed crossings right of this
//!    tile among edges `i..j`" in `O(log n)`.
//!
//! Nothing outside the current row is kept, and a piece is built in one reused buffer, in the global
//! frame: [`Local`] moves it into the tile's frame as it is read.
//!
//! Unclipped ([`ClipMode::Unclipped`]), the sweep is the same but builds nothing: each edge tile that
//! would get a piece, and each tile of a fill run, gets the whole polygon, lent from the input.

use core::cmp::Ordering;
use core::ops::Range;

use geo_types::Coord;

use crate::TileError;
use crate::clip_polygon::{
    ClipScratch, RingClip, clip_ring, close_ring, fill_box, keep_edge, push_distinct,
    push_fill_box, ring_meets_box,
};
use crate::clip_polyline::to_local;
use crate::geom::{crossing_x_ceil, ring_orientation};
use crate::grid::{Grid, Seg};
use crate::slicer::{ClipMode, Local};
use crate::tile::TileId;
use crate::vertex::{PolyVertex, Vertex};

/// Edges per chunk of the row index: a chunk's `y`-range decides which rows look at its edges.
const CHUNK: usize = 32;

/// How many tiles of one row an edge must touch to be kept as a single [`Span`] for the row rather
/// than one [`Hit`] per tile.
const LONG_EDGE: i64 = 4;

/// A horizontal run of tiles lying **entirely inside** the polygon: row `y`, columns `x.start..x.end`.
///
/// No ring edge touches these tiles' buffered boxes, so each is uniformly covered: in each of them,
/// [`Slicer::tile_polygon`](crate::Slicer::tile_polygon) yields this polygon as a single
/// all-synthetic box ring around the buffered box, which [`PolygonSlices::fill_ring`] (or
/// [`Slicer::fill_ring`](crate::Slicer::fill_ring)) returns — the same tile-local ring for every tile
/// of the run. Runs are maximal, and never overlap the polygon's edge tiles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct FillRun {
    /// The tile row.
    pub y: i32,
    /// The tile columns, end-exclusive.
    pub x: Range<i32>,
    /// The polygon's exterior winding, which its fill ring follows.
    orient: Ordering,
}

/// One slice of a polygon.
#[derive(Debug)]
pub enum PolygonSlice<'a, V, R> {
    /// One polygon's piece in one tile.
    Piece(PolygonPiece<'a, V, R>),
    /// Tiles a polygon covers entirely, whose content is [`PolygonSlices::fill_ring`] in each.
    Run(FillRun),
}

/// One polygon's rings in one tile: exterior first, then its holes that reach the tile (all of them,
/// and each whole, when unclipped).
#[derive(Debug)]
pub struct PolygonPiece<'a, V, R> {
    tile: TileId,
    rings: PieceRings<'a, V, R>,
}

#[derive(Debug)]
enum PieceRings<'a, V, R> {
    /// Clipped rings in the piece buffer (global, each closed), ending at `ends`, and the tile
    /// origin.
    Built {
        verts: &'a [V],
        ends: &'a [u32],
        origin: Coord<i32>,
    },
    /// The whole polygon — lying in the tile, or unclipped: its kept rings, lent from the input (or
    /// the copy of a ring that had duplicate vertices).
    Whole {
        rings: &'a [RingInfo],
        input: &'a [R],
        copies: &'a [V],
        origin: Coord<i32>,
    },
}

// Borrowed views: `Clone`/`Copy` without requiring them of the vertex or ring types.
impl<V, R> Clone for PieceRings<'_, V, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<V, R> Copy for PieceRings<'_, V, R> {}
impl<V, R> Clone for PolygonPiece<'_, V, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<V, R> Copy for PolygonPiece<'_, V, R> {}

impl<'a, V: Vertex, R: AsRef<[V]>> PolygonPiece<'a, V, R> {
    /// The tile this piece belongs to.
    #[must_use]
    pub fn tile(&self) -> TileId {
        self.tile
    }

    /// The piece's rings, exterior first, each closed and in the tile's local frame.
    pub fn rings(&self) -> impl Iterator<Item = Local<'a, V>> + use<'a, V, R> {
        let rings = self.rings;
        let n = match rings {
            PieceRings::Built { ends, .. } => ends.len(),
            PieceRings::Whole { rings, .. } => rings.len(),
        };
        (0..n).map(move |i| match rings {
            PieceRings::Built {
                verts,
                ends,
                origin,
            } => {
                let start = i.checked_sub(1).map_or(0, |p| ends[p] as usize);
                Local::ring(&verts[start..ends[i] as usize], origin, true)
            }
            PieceRings::Whole {
                rings,
                input,
                copies,
                origin,
            } => Local::ring(rings[i].pts(input, copies), origin, false),
        })
    }
}

/// Where one kept ring's distinct vertices are.
#[derive(Debug, Clone, Copy)]
enum Src {
    /// The first `len` vertices of the input ring.
    Input,
    /// `copies[start..start + len]`, the ring with its duplicate vertices dropped.
    Copy(usize),
}

/// One kept ring (at least three distinct vertices) of the polygon being sliced: ring 0 is its
/// exterior. Edge `i` runs from vertex `i` to vertex `(i + 1) % len`, and is edge `edge0 + i` of the
/// polygon.
#[derive(Debug, Clone, Copy)]
struct RingInfo {
    input: u32,
    src: Src,
    len: u32,
    edge0: u32,
}

impl RingInfo {
    fn pts<'a, V, R: AsRef<[V]>>(&self, input: &'a [R], copies: &'a [V]) -> &'a [V] {
        let len = self.len as usize;
        match self.src {
            Src::Input => &input[self.input as usize].as_ref()[..len],
            Src::Copy(start) => &copies[start..start + len],
        }
    }
}

/// [`CHUNK`] consecutive edges of one ring, and the tile rows their buffered `y`-range reaches.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    ring: u32,
    first: u32,
    count: u32,
    rows: (i64, i64),
}

/// A ring edge touching tile `tx` of the current row. An edge touching fewer than [`LONG_EDGE`]
/// tiles there has one hit per tile, so a tile's hits are one contiguous run, used as is.
#[derive(Debug, Clone, Copy)]
struct Hit {
    tx: i32,
    edge: u32,
    ring: u32,
}

/// A long edge touching tiles `first..=last` of the current row: one entry for the row, so a row
/// costs memory per edge, never per tile, however far an edge runs along it.
#[derive(Debug, Clone, Copy)]
struct Span {
    first: i32,
    last: i32,
    edge: u32,
    ring: u32,
}

/// A ring edge crossing the current row's center line, the line of every winding ray in that row
/// (half-open in `y`, so each crossing is counted once). The crossing lies right of tile `tx`'s
/// buffered box — on its winding ray — iff `tx <= right_of`.
#[derive(Debug, Clone, Copy)]
struct Crossing {
    edge: u32,
    ring: u32,
    up: bool,
    right_of: i64,
}

impl Crossing {
    fn sign(self) -> i64 {
        if self.up { 1 } else { -1 }
    }
}

/// Row `ty`'s center line: the midpoint of its buffered box `[ty·e − b, ty·e + e − 1 + b]`, rounded
/// toward zero, as the clip computes it with `midpoint` — in closed form: the buffer cancels, and the
/// sum `2·ty·e + e − 1` is negative exactly when `ty < 0`, where rounding toward zero rounds up.
fn row_center(ty: i64, extent: i64) -> i64 {
    ty * extent + if ty < 0 { extent / 2 } else { (extent - 1) / 2 }
}

/// Rebuild `tree` as a Fenwick (binary indexed) tree over `values`, in `O(n)`.
fn fenwick_build(tree: &mut Vec<i64>, values: impl Iterator<Item = i64>) {
    tree.clear();
    tree.extend(values);
    for i in 0..tree.len() {
        let parent = i | (i + 1);
        if parent < tree.len() {
            tree[parent] += tree[i];
        }
    }
}

/// Add `delta` at position `i`.
fn fenwick_add(tree: &mut [i64], mut i: usize, delta: i64) {
    while i < tree.len() {
        tree[i] += delta;
        i |= i + 1;
    }
}

/// Sum of the first `end` positions.
fn fenwick_prefix(tree: &[i64], mut end: usize) -> i64 {
    let mut sum = 0;
    while end > 0 {
        sum += tree[end - 1];
        end &= end - 1;
    }
    sum
}

/// Sort one row's `hits` by `(tx, edge)`. They arrive in ascending edge order, so a stable counting
/// sort by column finishes the job in `O(n + columns)`; a column range much wider than the hits are
/// many falls back to a comparison sort. `tmp` and `counts` are reused scratch.
fn sort_hits(hits: &mut [Hit], tmp: &mut Vec<Hit>, counts: &mut Vec<usize>) {
    let Some(head) = hits.first() else {
        return;
    };
    let (lo, hi) = hits.iter().fold((head.tx, head.tx), |(lo, hi), h| {
        (lo.min(h.tx), hi.max(h.tx))
    });
    let columns = hi.abs_diff(lo) as usize + 1;
    if columns == 1 {
        // One column: already in edge order.
    } else if hits.len() < 16 || columns > hits.len().saturating_mul(4) {
        hits.sort_unstable_by_key(|h| (h.tx, h.edge));
    } else {
        counts.clear();
        counts.resize(columns, 0);
        for h in hits.iter() {
            counts[h.tx.abs_diff(lo) as usize] += 1;
        }
        // Exclusive prefix sums: where each column starts.
        let mut start = 0;
        for c in counts.iter_mut() {
            (*c, start) = (start, start + *c);
        }
        tmp.clear();
        tmp.extend_from_slice(hits);
        for h in tmp.iter() {
            let at = &mut counts[h.tx.abs_diff(lo) as usize];
            hits[*at] = *h;
            *at += 1;
        }
    }
    debug_assert!(hits.is_sorted_by_key(|h| (h.tx, h.edge)));
}

/// Check that freshly written vertices fit the tile-local frame of `origin`, as [`Local`] lends them.
fn check_local<V: Vertex>(verts: &[V], origin: Coord<i32>) -> Result<(), TileError> {
    for &v in verts {
        to_local(v, origin)?;
    }
    Ok(())
}

/// Restore edge order to `spans`, whose first `kept` are in edge order and the rest were appended (in
/// first-column order, so in edge order too when they all start in the same column): sort the
/// appended part only if needed, then merge the two in one pass through `tmp`.
fn merge_by_edge<T: Copy>(
    spans: &mut Vec<T>,
    kept: usize,
    tmp: &mut Vec<T>,
    edge: impl Fn(&T) -> u32,
) {
    let added = &mut spans[kept..];
    if !added.is_sorted_by_key(&edge) {
        added.sort_unstable_by_key(&edge);
    }
    if kept == 0 || kept == spans.len() || edge(&spans[kept - 1]) < edge(&spans[kept]) {
        return;
    }
    tmp.clear();
    let (old, new) = spans.split_at(kept);
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if edge(&old[i]) < edge(&new[j]) {
            tmp.push(old[i]);
            i += 1;
        } else {
            tmp.push(new[j]);
            j += 1;
        }
    }
    tmp.extend_from_slice(&old[i..]);
    tmp.extend_from_slice(&new[j..]);
    core::mem::swap(spans, tmp);
}

/// `hits` (the spans covering one tile, in edge order, so each ring's are contiguous) split into runs
/// of the same ring.
fn ring_groups(mut hits: &[Hit]) -> impl Iterator<Item = &[Hit]> {
    core::iter::from_fn(move || {
        let ring = hits.first()?.ring;
        let (group, rest) = hits.split_at(hits.partition_point(|x| x.ring == ring));
        hits = rest;
        Some(group)
    })
}

/// Signed crossings, right of the current tile, of `ring`'s `count` edges from local edge `first`
/// (cyclically): the row's still-active crossings in that edge range, summed by the Fenwick tree.
fn excursion_winding(
    cs: &[Crossing],
    fenwick: &[i64],
    ring: RingInfo,
    first: usize,
    count: usize,
) -> i64 {
    let (start, len) = (ring.edge0 as usize, ring.len as usize);
    let sum = |lo: usize, hi: usize| {
        let i = cs.partition_point(|c| (c.edge as usize) < lo);
        let j = cs.partition_point(|c| (c.edge as usize) < hi);
        fenwick_prefix(fenwick, j) - fenwick_prefix(fenwick, i)
    };
    if first + count <= len {
        sum(start + first, start + first + count)
    } else {
        sum(start + first, start + len) + sum(start, start + first + count - len)
    }
}

/// Reusable working memory for polygon slicing, kept in the [`Slicer`](crate::Slicer) between
/// features (see [`Scratch`]): only its capacity survives a call.
pub(crate) struct PolyScratch<V> {
    /// The current polygon's kept rings, copies of rings that had duplicate vertices, and chunks.
    rings: Vec<RingInfo>,
    copies: Vec<V>,
    chunks: Vec<Chunk>,
    /// Chunk ids by first row, and the chunks reaching the current row (ascending, so edge order).
    order: Vec<u32>,
    active: Vec<u32>,
    /// The current row: its per-tile spans of short edges (by column, then edge), the spans of its
    /// long edges (by first column, then edge), the long ones covering the sweep's current tile (by
    /// edge), and a tile's merged spans when a long edge crosses it. Then its crossings (by edge), the
    /// crossings by `right_of`, and the Fenwick tree of the signs of those still right of the sweep.
    hits: Vec<Hit>,
    long: Vec<Span>,
    long_cover: Vec<Span>,
    covering: Vec<Hit>,
    crossings: Vec<Crossing>,
    by_x: Vec<u32>,
    /// Sorting scratch for the row's spans.
    hits_tmp: Vec<Hit>,
    spans_tmp: Vec<Span>,
    counts: Vec<usize>,
    fenwick: Vec<i64>,
    /// Per ring: odd crossings right of the current tile.
    inside: Vec<bool>,
    /// The piece being yielded: tile-local ring vertices and where each ring ends.
    verts: Vec<V>,
    ring_ends: Vec<u32>,
    arcs: Vec<(usize, usize)>,
    clip: ClipScratch<V>,
}

impl<V> PolyScratch<V> {
    pub(crate) const fn new() -> Self {
        Self {
            rings: Vec::new(),
            copies: Vec::new(),
            chunks: Vec::new(),
            order: Vec::new(),
            active: Vec::new(),
            hits: Vec::new(),
            long: Vec::new(),
            long_cover: Vec::new(),
            covering: Vec::new(),
            crossings: Vec::new(),
            by_x: Vec::new(),
            hits_tmp: Vec::new(),
            spans_tmp: Vec::new(),
            counts: Vec::new(),
            fenwick: Vec::new(),
            inside: Vec::new(),
            verts: Vec::new(),
            ring_ends: Vec::new(),
            arcs: Vec::new(),
            clip: ClipScratch::new(),
        }
    }
}

/// The [`Slicer`](crate::Slicer)'s polygon working memory, allocated by its first polygon, so a
/// slicer used only for lines and points never builds it. It holds nothing between calls that affects
/// results: it compares equal, clones empty, and only its capacity survives a call.
pub(crate) struct Scratch<V>(Option<Box<PolyState<V>>>);

/// The working memory, and the state of the current polygon's sweep (kept out of the iterator, so the
/// iterator stays small to move).
pub(crate) struct PolyState<V> {
    pub(crate) scratch: PolyScratch<V>,
    sweep: Option<Sweep>,
}

impl<V> Scratch<V> {
    pub(crate) const fn new() -> Self {
        Self(None)
    }

    /// The working memory, allocated on first use.
    #[inline]
    pub(crate) fn get(&mut self) -> &mut PolyState<V> {
        #[cold]
        fn alloc<V>() -> Box<PolyState<V>> {
            Box::new(PolyState {
                scratch: PolyScratch::new(),
                sweep: None,
            })
        }
        self.0.get_or_insert_with(alloc)
    }
}

impl<V> Clone for Scratch<V> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<V> PartialEq for Scratch<V> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<V> Eq for Scratch<V> {}

impl<V> core::fmt::Debug for Scratch<V> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PolyScratch").finish_non_exhaustive()
    }
}

/// The fill ring of `run` on `grid`: tile (0, 0)'s buffered box `[-buffer, extent − 1 + buffer]`
/// grown by one unit, wound like the run's polygon — the same in every tile's local frame. It fits
/// `i32` because [`Grid::new`] keeps `extent + buffer` within it.
pub(crate) fn fill_ring<V: PolyVertex>(grid: Grid, run: &FillRun) -> [V; 5] {
    let (extent, buffer) = (grid.extent_i32(), grid.buffer_i32());
    let sw = Coord {
        x: -buffer - 1,
        y: -buffer - 1,
    };
    let ne = Coord {
        x: extent + buffer,
        y: extent + buffer,
    };
    fill_box(run.orient, sw, ne).map(V::synthetic_at)
}

/// What the next slice is; the borrowed view is built from it.
enum Event {
    /// The whole current polygon, for `tile`: it lies in the tile's inner box, or is unclipped.
    Whole {
        tile: TileId,
        origin: Coord<i32>,
    },
    /// A piece built into the piece buffer, for the tile at `origin`.
    Built {
        tile: TileId,
        origin: Coord<i32>,
    },
    Run(FillRun),
}

/// The row sweep of the current polygon.
struct Sweep {
    /// The next row to load, and the last row.
    row: i64,
    last_row: i64,
    /// Chunks not yet active, as an index into the by-first-row order.
    next_chunk: usize,
    /// Whether a row is loaded, and the sweep's place in it: the next crossing (by `right_of`), the
    /// next short-edge span, the next long-edge span, and the first tile not yet yielded or filled.
    in_row: bool,
    ty: i32,
    k: usize,
    h: usize,
    lh: usize,
    next: i64,
    /// The spans of the current tile: its short-edge spans as they are (`Some`, the usual case: no
    /// copy), or merged with the long edges crossing it into the scratch's `covering` (`None`).
    cover: Option<Range<usize>>,
    /// The last column a long edge covering the sweep reaches (`i64::MIN` when none does).
    long_last: i64,
    /// How many holes have odd parity right of the sweep.
    holes_in: u32,
    /// A covered span of the current row not yet yielded (it may still grow), as `start..end`.
    pending: Option<(i64, i64)>,
    /// A run to yield before anything else, and an edge tile to build after it (from the spans
    /// covering it).
    ready: Option<FillRun>,
    tile_ready: Option<i32>,
    /// The exterior's winding, once a fill needs it.
    orient: Option<Ordering>,
}

/// Lazy iterator over the slices of a polygon in every tile it reaches; see
/// [`Slicer::polygon`](crate::Slicer::polygon).
///
/// A **lending** iterator: a [`PolygonPiece`] borrows the iterator's piece buffer, so it is valid
/// until the next call to [`next`](Self::next). Loop with `while let Some(slice) = slices.next()`.
pub struct PolygonSlices<'s, V, R> {
    grid: Grid,
    /// [`ClipMode::Unclipped`]: every tile gets the whole polygon.
    whole: bool,
    rings: &'s [R],
    s: &'s mut PolyScratch<V>,
    /// How the polygon starts, until the first step; then its sweep, if it has one.
    start: Option<Start>,
    sweep: &'s mut Option<Sweep>,
    done: bool,
}

/// How a loaded polygon starts: small, so the iterator can be built in place, its sweep only on the
/// first step.
enum Start {
    /// It lies wholly in `tile`'s inner box.
    Whole { tile: TileId, origin: Coord<i32> },
    /// It is swept over rows `row..=last_row`.
    Sweep { row: i64, last_row: i64 },
}

impl<'s, V: PolyVertex, R: AsRef<[V]>> PolygonSlices<'s, V, R> {
    /// Load the polygon `rings`, bounding and indexing it.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline]
    pub(crate) fn new(
        grid: Grid,
        mode: ClipMode,
        rings: &'s [R],
        state: &'s mut PolyState<V>,
    ) -> Result<Self, TileError> {
        let PolyState { scratch: s, sweep } = state;
        *sweep = None;
        let start = prepare(grid, rings, s)?;
        Ok(Self {
            grid,
            whole: mode == ClipMode::Unclipped,
            rings,
            s,
            start,
            sweep,
            done: false,
        })
    }

    /// The next slice, or `None` when the polygon is done. An error ends the iteration: only
    /// [`TileError::GeometryTooLarge`], for a piece of more than `u32::MAX` vertices, since
    /// [`Slicer::polygon`](crate::Slicer::polygon) settled every overflow up front.
    #[expect(
        clippy::should_implement_trait,
        reason = "a lending iterator: each item borrows the iterator"
    )]
    pub fn next(&mut self) -> Option<Result<PolygonSlice<'_, V, R>, TileError>> {
        if self.done {
            return None;
        }
        match self.step() {
            Ok(Some(event)) => Some(Ok(self.view(event))),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }

    /// The ring [`Slicer::tile_polygon`](crate::Slicer::tile_polygon) yields for `run`'s polygon in
    /// each tile of the run: the tile's buffered box grown by one unit, tile-local and closed (its
    /// first vertex repeated), all synthetic vertices, wound like the polygon's exterior. It is the
    /// polygon's only ring there. The same as [`Slicer::fill_ring`](crate::Slicer::fill_ring), for use
    /// while the iterator borrows the slicer.
    #[must_use]
    pub fn fill_ring(&self, run: &FillRun) -> [V; 5] {
        fill_ring(self.grid, run)
    }

    fn view(&self, event: Event) -> PolygonSlice<'_, V, R> {
        let (tile, rings) = match event {
            Event::Run(run) => return PolygonSlice::Run(run),
            Event::Built { tile, origin } => (
                tile,
                PieceRings::Built {
                    verts: &self.s.verts,
                    ends: &self.s.ring_ends,
                    origin,
                },
            ),
            Event::Whole { tile, origin } => (
                tile,
                PieceRings::Whole {
                    rings: &self.s.rings,
                    input: self.rings,
                    copies: &self.s.copies,
                    origin,
                },
            ),
        };
        PolygonSlice::Piece(PolygonPiece { tile, rings })
    }

    fn step(&mut self) -> Result<Option<Event>, TileError> {
        match self.start.take() {
            Some(Start::Whole { tile, origin }) => return Ok(Some(Event::Whole { tile, origin })),
            Some(Start::Sweep { row, last_row }) => *self.sweep = Some(Sweep::new(row, last_row)),
            None => {}
        }
        let Some(sw) = self.sweep.as_mut() else {
            return Ok(None);
        };
        let mut cx = Cx {
            grid: self.grid,
            s: &mut *self.s,
            rings: self.rings,
        };
        if self.whole {
            sw.step_whole(&mut cx)
        } else {
            sw.step::<false, V, R>(&mut cx)
        }
    }
}

/// Load the polygon `rings` into `s`: how it starts, or `None` if there is nothing to slice (a
/// degenerate exterior). Every overflow the slicing could meet is settled here, from the polygon's
/// bounds.
// Once per polygon: inlined into `PolygonSlices::new`, it costs small polygons ~4%.
#[inline(never)]
fn prepare<V: PolyVertex, R: AsRef<[V]>>(
    grid: Grid,
    rings: &[R],
    s: &mut PolyScratch<V>,
) -> Result<Option<Start>, TileError> {
    let (extent, buffer) = (i64::from(grid.extent()), i64::from(grid.buffer()));
    s.rings.clear();
    s.copies.clear();
    u32::try_from(rings.len()).map_err(|_| TileError::GeometryTooLarge)?;
    let mut bounds: Option<(Coord<i32>, Coord<i32>)> = None;
    let mut edges = 0_u32;
    for (index, ring) in (0_u32..).zip(rings) {
        let Some((info, lo, hi)) = load_ring(s, index, ring.as_ref(), edges)? else {
            if index == 0 {
                return Ok(None); // a degenerate exterior drops the polygon
            }
            continue;
        };
        edges = edges
            .checked_add(info.len)
            .ok_or(TileError::GeometryTooLarge)?;
        s.rings.push(info);
        bounds = Some(bounds.map_or((lo, hi), |(l, h)| {
            (
                Coord {
                    x: l.x.min(lo.x),
                    y: l.y.min(lo.y),
                },
                Coord {
                    x: h.x.max(hi.x),
                    y: h.y.max(hi.y),
                },
            )
        }));
    }
    let Some((lo, hi)) = bounds else {
        return Ok(None);
    };

    // A polygon inside one tile's inner box touches only that tile, whose box `inner_tile` proved
    // fits: it goes there whole, lent as it is, with nothing else to check.
    if let Some((tile, origin)) = grid.inner_tile(lo, hi) {
        return Ok(Some(Start::Whole { tile, origin }));
    }
    grid.check_polygon_bounds(lo, hi)?;

    s.chunks.clear();
    index_chunks(s, rings, extent, buffer);
    s.order.clear();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "at most one chunk per edge"
    )]
    s.order.extend(0..s.chunks.len() as u32);
    let chunks = &s.chunks;
    s.order.sort_unstable_by_key(|&c| chunks[c as usize].rows.0);
    s.active.clear();
    s.inside.clear();
    s.inside.resize(s.rings.len(), false);
    Ok(Some(Start::Sweep {
        row: (i64::from(lo.y) - buffer).div_euclid(extent),
        last_row: (i64::from(hi.y) + buffer).div_euclid(extent),
    }))
}

/// What the sweep reads besides its own state: the grid, the scratch, and the polygon being swept.
struct Cx<'x, V, R> {
    grid: Grid,
    s: &'x mut PolyScratch<V>,
    rings: &'x [R],
}

impl Sweep {
    /// A sweep over rows `row..=last_row`, before its first row.
    fn new(row: i64, last_row: i64) -> Self {
        Self {
            row,
            last_row,
            next_chunk: 0,
            in_row: false,
            ty: 0,
            k: 0,
            h: 0,
            next: i64::MIN,
            holes_in: 0,
            pending: None,
            ready: None,
            tile_ready: None,
            orient: None,
            lh: 0,
            cover: Some(0..0),
            long_last: i64::MIN,
        }
    }

    /// [`Self::step`] unclipped.
    // Keeps this second copy of the sweep out of `PolygonSlices::next`, so the clipped one inlines as
    // before (any extra code there costs the clipped sweep 2-5%).
    #[inline(never)]
    fn step_whole<V: PolyVertex, R: AsRef<[V]>>(
        &mut self,
        cx: &mut Cx<'_, V, R>,
    ) -> Result<Option<Event>, TileError> {
        self.step::<true, V, R>(cx)
    }

    /// Advance the sweep to its next slice; `None` when the polygon is done. `WHOLE`
    /// ([`ClipMode::Unclipped`]): give every tile the whole polygon, fill runs tile by tile.
    fn step<const WHOLE: bool, V: PolyVertex, R: AsRef<[V]>>(
        &mut self,
        cx: &mut Cx<'_, V, R>,
    ) -> Result<Option<Event>, TileError> {
        loop {
            if let Some(run) = self.ready.take() {
                return Ok(Some(if WHOLE {
                    self.whole_run_tile(cx.grid, run)
                } else {
                    Event::Run(run)
                }));
            }
            if let Some(tx) = self.tile_ready.take() {
                let tile = TileId::new(tx, self.ty);
                if WHOLE {
                    if let Some(event) = self.whole_tile(cx.grid, cx.s, tile) {
                        return Ok(Some(event));
                    }
                } else if let Some(origin) = self.build_tile(cx, tile)? {
                    return Ok(Some(Event::Built { tile, origin }));
                }
                continue;
            }
            if !self.in_row {
                if self.row > self.last_row {
                    return Ok(None);
                }
                self.load_row(cx)?;
                self.ty = i32::try_from(self.row).map_err(|_| TileError::Overflow)?;
                (self.in_row, self.k, self.h, self.lh, self.next) = (true, 0, 0, 0, i64::MIN);
                (self.cover, self.long_last) = (Some(0..0), i64::MIN);
                continue;
            }
            // Tiles `next..=bound` all see the same crossings on their winding rays.
            let s = &*cx.s;
            let bound = s
                .by_x
                .get(self.k)
                .map_or(i64::MAX, |&i| s.crossings[i as usize].right_of);
            // The next edge tile, and its spans. Rows without long edges (most) take the short path.
            let tile = if s.long.is_empty() {
                s.hits.get(self.h).map(|c| i64::from(c.tx))
            } else {
                // The sweep's next tile if a long edge still covers it, else where the next short or
                // long span starts.
                let next = self.next;
                // (`long_last` is `i64::MIN` when no long edge covers the sweep, as at a row start.)
                let covered =
                    (self.long_last != i64::MIN && self.long_last >= next).then_some(next);
                let short = s.hits.get(self.h).map(|c| i64::from(c.tx));
                let long = s.long.get(self.lh).map(|c| i64::from(c.first).max(next));
                [covered, short, long].into_iter().flatten().min()
            };
            if let Some(tx) = tile
                && tx <= bound
            {
                let tx = i32::try_from(tx).map_err(|_| TileError::Overflow)?;
                let s = &mut *cx.s;
                // The tile's short-edge spans: one contiguous run.
                let start = self.h;
                self.h += s.hits[start..].partition_point(|c| c.tx == tx);
                self.cover = Some(start..self.h);
                if !s.long.is_empty() {
                    self.cover_long(s, tx, start);
                }
                self.fill(cx, self.next, i64::from(tx))?;
                // The edge tile ends any covered span before it.
                self.ready = self.take_pending();
                self.tile_ready = Some(tx);
                self.next = i64::from(tx) + 1;
                continue;
            }
            if self.k == s.by_x.len() {
                // Past every crossing each ring's parity is even again: nothing covers.
                debug_assert!(cx.s.inside.iter().all(|&i| !i), "parity ends even");
                self.ready = self.take_pending();
                (self.in_row, self.row) = (false, self.row + 1);
                continue;
            }
            // `bound` is a crossing's `right_of` (|·| ≤ 2^32), so `+ 1` cannot overflow.
            self.fill(cx, self.next, bound + 1)?;
            self.next = bound + 1;
            // Tiles past `bound` have these crossings on their left.
            let s = &mut *cx.s;
            while let Some(&i) = s.by_x.get(self.k)
                && s.crossings[i as usize].right_of == bound
            {
                let crossing = s.crossings[i as usize];
                let r = crossing.ring as usize;
                s.inside[r] = !s.inside[r];
                if r != 0 {
                    if s.inside[r] {
                        self.holes_in += 1;
                    } else {
                        // Parity alternates from an empty row start, so a hole leaves only after
                        // entering.
                        self.holes_in = self.holes_in.saturating_sub(1);
                    }
                }
                fenwick_add(&mut s.fenwick, i as usize, -crossing.sign());
                self.k += 1;
            }
            // Coverage that ends here ends the covered span: it cannot grow past this crossing.
            if !(s.inside[0] && self.holes_in == 0) {
                self.ready = self.take_pending();
            }
        }
    }

    /// Unclipped, each covered tile gets the whole polygon: the first tile of `run`, keeping the rest
    /// of it for the next steps.
    fn whole_run_tile(&mut self, grid: Grid, mut run: FillRun) -> Event {
        let tile = TileId::new(run.x.start, run.y);
        run.x.start += 1;
        if !run.x.is_empty() {
            self.ready = Some(run);
        }
        Event::Whole {
            tile,
            origin: grid.tile_origin(tile),
        }
    }

    /// Add the long edges covering tile `tx` to its spans (the short ones are `s.hits[start..h]`):
    /// drop those ending before it, add those starting by it, and merge both in edge order.
    fn cover_long<V>(&mut self, s: &mut PolyScratch<V>, tx: i32, start: usize) {
        if self.long_last < i64::from(tx) {
            s.long_cover.clear();
        } else {
            s.long_cover.retain(|c| c.last >= tx);
        }
        let kept = s.long_cover.len();
        while let Some(&span) = s.long.get(self.lh)
            && span.first <= tx
        {
            s.long_cover.push(span);
            self.lh += 1;
        }
        if s.long_cover.is_empty() {
            self.long_last = i64::MIN;
            return;
        }
        merge_by_edge(&mut s.long_cover, kept, &mut s.spans_tmp, |c| c.edge);
        self.long_last = s
            .long_cover
            .iter()
            .map(|c| i64::from(c.last))
            .max()
            .unwrap_or(i64::MIN);
        s.covering.clear();
        s.covering.extend(s.long_cover.iter().map(|c| Hit {
            tx,
            edge: c.edge,
            ring: c.ring,
        }));
        let kept = s.covering.len();
        s.covering.extend_from_slice(&s.hits[start..self.h]);
        merge_by_edge(&mut s.covering, kept, &mut s.hits_tmp, |c| c.edge);
        self.cover = None;
    }

    /// The covered span not yet yielded, as a run (ending it).
    fn take_pending(&mut self) -> Option<FillRun> {
        let (start, end) = self.pending.take()?;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "both ends were checked to fit i32 when the span was recorded"
        )]
        Some(FillRun {
            y: self.ty,
            x: start as i32..end as i32,
            orient: self.orient.unwrap_or(Ordering::Equal),
        })
    }

    /// Record tiles `start..end` of the current row as covered, if the polygon covers them,
    /// extending the span not yet yielded when they abut.
    fn fill<V: PolyVertex, R: AsRef<[V]>>(
        &mut self,
        cx: &mut Cx<'_, V, R>,
        start: i64,
        end: i64,
    ) -> Result<(), TileError> {
        let covered = cx.s.inside.first().copied().unwrap_or(false) && self.holes_in == 0;
        if start >= end || !covered {
            return Ok(());
        }
        i32::try_from(start).map_err(|_| TileError::Overflow)?;
        i32::try_from(end).map_err(|_| TileError::Overflow)?;
        if self.orient.is_none() {
            let ext = cx.s.rings[0].pts(cx.rings, &cx.s.copies);
            self.orient = Some(ring_orientation(ext));
        }
        match &mut self.pending {
            Some((_, e)) if *e == start => *e = end,
            pending => {
                debug_assert!(
                    pending.is_none(),
                    "an edge tile or row end yields a span first"
                );
                *pending = Some((start, end));
            }
        }
        Ok(())
    }

    /// Gather row `ty`'s hits and crossings from the chunks reaching it.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    // (The `hotpath` wrapper hides the body from this lint.)
    #[cfg_attr(
        not(feature = "hotpath"),
        expect(
            clippy::many_single_char_names,
            reason = "edge endpoints and the scratch"
        )
    )]
    fn load_row<V: PolyVertex, R: AsRef<[V]>>(
        &mut self,
        cx: &mut Cx<'_, V, R>,
    ) -> Result<(), TileError> {
        let ty = self.row;
        let (extent, buffer) = (i64::from(cx.grid.extent()), i64::from(cx.grid.buffer()));
        let s = &mut *cx.s;
        let rings = cx.rings;
        while let Some(&c) = s.order.get(self.next_chunk)
            && s.chunks[c as usize].rows.0 <= ty
        {
            let at = s.active.partition_point(|&a| a < c);
            s.active.insert(at, c);
            self.next_chunk += 1;
        }
        let chunks = &s.chunks;
        s.active.retain(|&c| chunks[c as usize].rows.1 >= ty);

        s.hits.clear();
        s.long.clear();
        s.long_cover.clear();
        s.covering.clear();
        s.crossings.clear();
        let (slab_lo, slab_hi) = (ty * extent - buffer, ty * extent + extent - 1 + buffer);
        let cy = row_center(ty, extent);
        for &c in &s.active {
            let chunk = s.chunks[c as usize];
            let info = s.rings[chunk.ring as usize];
            let pts = info.pts(rings, &s.copies);
            let m = pts.len();
            for i in chunk.first as usize..(chunk.first + chunk.count) as usize {
                let (a, b) = (pts[i].position(), pts[(i + 1) % m].position());
                let (lo, hi) = if a.y <= b.y { (a, b) } else { (b, a) };
                if i64::from(hi.y) < slab_lo || i64::from(lo.y) > slab_hi {
                    continue;
                }
                #[expect(clippy::cast_possible_truncation, reason = "edge ids fit u32")]
                let edge = info.edge0 + i as u32;
                let wide = |p: Coord<i32>| Coord {
                    x: i64::from(p.x),
                    y: i64::from(p.y),
                };
                let (first, last) = Seg::new(wide(lo), wide(hi)).row_span(ty, extent, buffer);
                let fits = |v: i64| i32::try_from(v).map_err(|_| TileError::Overflow);
                if last - first + 1 >= LONG_EDGE {
                    s.long.push(Span {
                        first: fits(first)?,
                        last: fits(last)?,
                        edge,
                        ring: chunk.ring,
                    });
                } else {
                    for tx in first..=last {
                        s.hits.push(Hit {
                            tx: fits(tx)?,
                            edge,
                            ring: chunk.ring,
                        });
                    }
                }
                // Crossings of the center line, half-open in `y`.
                if i64::from(lo.y) <= cy && cy < i64::from(hi.y) {
                    let x =
                        crossing_x_ceil(a, b, i32::try_from(cy).map_err(|_| TileError::Overflow)?);
                    s.crossings.push(Crossing {
                        edge,
                        ring: chunk.ring,
                        up: b.y > a.y,
                        // Right of tile `tx`'s box iff x > tx·extent + extent − 1 + buffer.
                        right_of: (x - extent - buffer).div_euclid(extent),
                    });
                }
            }
        }
        sort_hits(&mut s.hits, &mut s.hits_tmp, &mut s.counts);
        s.long.sort_unstable_by_key(|c| (c.first, c.edge));
        debug_assert!(s.crossings.is_sorted_by_key(|c| c.edge));
        s.by_x.clear();
        #[expect(clippy::cast_possible_truncation, reason = "one crossing per edge")]
        s.by_x.extend(0..s.crossings.len() as u32);
        let crossings = &s.crossings;
        s.by_x
            .sort_unstable_by_key(|&i| crossings[i as usize].right_of);
        fenwick_build(&mut s.fenwick, s.crossings.iter().map(|c| c.sign()));
        Ok(())
    }

    /// The spans covering the current edge tile, if the polygon leaves anything there. Only the
    /// touching rings are visited: the untouched holes are settled at once by how many of them
    /// contain the tile. Takes the scratch's row fields, `hits`, `covering` and `inside`, rather than
    /// the scratch, so the caller can still write the piece buffer.
    #[expect(
        clippy::inline_always,
        reason = "run per edge tile; out of line it costs the polygon benchmarks 2%"
    )]
    #[inline(always)]
    fn tile_hits<'x>(
        &self,
        hits: &'x [Hit],
        covering: &'x [Hit],
        inside: &[bool],
    ) -> Option<&'x [Hit]> {
        let hits = match &self.cover {
            Some(range) => &hits[range.clone()],
            None => covering,
        };
        // Hits come in edge order, so the exterior's (if any) come first, then each hole's.
        let exterior_touched = hits.first().is_some_and(|x| x.ring == 0);
        if !exterior_touched && !inside[0] {
            return None; // the exterior misses the tile
        }
        // `holes_in` counts every hole whose ray parity is odd, but a touched hole's parity says
        // nothing about the tile: what remains are untouched holes containing it.
        let holes_in = self.holes_in as usize;
        if holes_in > 0
            && holes_in
                > ring_groups(hits)
                    .filter(|g| g[0].ring != 0 && inside[g[0].ring as usize])
                    .count()
        {
            return None;
        }
        Some(hits)
    }

    /// Unclipped, edge tile `tile` gets the whole polygon if it would get a piece.
    fn whole_tile<V>(&self, grid: Grid, s: &PolyScratch<V>, tile: TileId) -> Option<Event> {
        self.tile_hits(&s.hits, &s.covering, &s.inside)?;
        let origin = grid.tile_origin(tile);
        Some(Event::Whole { tile, origin })
    }

    /// Build the polygon's piece in edge tile `tile` from [the spans covering it](Self::tile_hits)
    /// into the piece buffer, returning the tile origin; `None` if it leaves nothing there. Every
    /// vertex fits the tile's local frame: the polygon's bounds were checked when it was loaded.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn build_tile<V: PolyVertex, R: AsRef<[V]>>(
        &mut self,
        cx: &mut Cx<'_, V, R>,
        tile: TileId,
    ) -> Result<Option<Coord<i32>>, TileError> {
        let (min, max) = cx.grid.tile_buffered_bounds(tile)?;
        let origin = cx.grid.origin_of(min);
        let s = &mut *cx.s;
        let rings = cx.rings;
        s.verts.clear();
        s.ring_ends.clear();
        let Some(hits) = self.tile_hits(&s.hits, &s.covering, &s.inside) else {
            return Ok(None);
        };
        let exterior_touched = hits.first().is_some_and(|x| x.ring == 0);
        if !exterior_touched {
            let orient = *self
                .orient
                .get_or_insert_with(|| ring_orientation(s.rings[0].pts(rings, &s.copies)));
            push_fill_box(&mut s.verts, orient, min, max)?;
            s.ring_ends.push(offset(s.verts.len())?);
        }
        for ring_hits in ring_groups(hits) {
            let info = s.rings[ring_hits[0].ring as usize];
            s.arcs.clear();
            for x in ring_hits {
                keep_edge(&mut s.arcs, (x.edge - info.edge0) as usize);
            }
            let (cs, fenwick) = (&s.crossings, &s.fenwick);
            let winding =
                |first: usize, count: usize| excursion_winding(cs, fenwick, info, first, count);
            close_ring(
                info.pts(rings, &s.copies),
                &mut s.arcs,
                min,
                max,
                winding,
                &mut s.verts,
            )?;
            s.ring_ends.push(offset(s.verts.len())?);
        }
        Ok(Some(origin))
    }
}

/// A kept ring and its bounding box `(lo, hi)`.
type LoadedRing = (RingInfo, Coord<i32>, Coord<i32>);

/// Load one ring of the polygon in `s` (its `index` among the input rings, its first edge `edge0`):
/// borrowed when it is distinct as given — no consecutive duplicates once its repeated closing
/// vertices are dropped — else its distinct vertices copied, with its bounding box. `None` for a
/// degenerate ring (fewer than 3 distinct vertices).
#[expect(
    clippy::inline_always,
    reason = "run per ring of every polygon; out of line it costs the small-polygon benchmarks 3-5%"
)]
#[inline(always)]
fn load_ring<V: Vertex>(
    s: &mut PolyScratch<V>,
    index: u32,
    ring: &[V],
    edge0: u32,
) -> Result<Option<LoadedRing>, TileError> {
    let Some(first) = ring.first().map(Vertex::position) else {
        return Ok(None);
    };
    let mut n = ring.len();
    while n >= 2 && ring[n - 1].position() == first {
        n -= 1;
    }
    // One pass: the bounding box, and whether the ring is distinct as given.
    let (mut lo, mut hi, mut prev, mut distinct) = (first, first, first, true);
    for v in &ring[1..n] {
        let p = v.position();
        (lo.x, lo.y, hi.x, hi.y) = (lo.x.min(p.x), lo.y.min(p.y), hi.x.max(p.x), hi.y.max(p.y));
        distinct &= p != prev;
        prev = p;
    }
    let (src, len) = if distinct {
        (Src::Input, n)
    } else {
        let start = s.copies.len();
        (Src::Copy(start), push_distinct(ring, &mut s.copies))
    };
    if len < 3 {
        if let Src::Copy(start) = src {
            s.copies.truncate(start);
        }
        return Ok(None);
    }
    let info = RingInfo {
        input: index,
        src,
        len: u32::try_from(len).map_err(|_| TileError::GeometryTooLarge)?,
        edge0,
    };
    Ok(Some((info, lo, hi)))
}

/// The row index of the polygon in `s.rings`: every run of [`CHUNK`] edges, with the rows its buffered
/// `y`-range reaches.
fn index_chunks<V: Vertex, R: AsRef<[V]>>(
    s: &mut PolyScratch<V>,
    rings: &[R],
    extent: i64,
    buffer: i64,
) {
    for (ring_id, info) in (0_u32..).zip(&s.rings) {
        let pts = info.pts(rings, &s.copies);
        for first in (0..pts.len()).step_by(CHUNK) {
            let count = CHUNK.min(pts.len() - first);
            let (mut y0, mut y1) = (i32::MAX, i32::MIN);
            for v in pts[first..first + count]
                .iter()
                .chain(core::iter::once(&pts[(first + count) % pts.len()]))
            {
                let y = v.position().y;
                (y0, y1) = (y0.min(y), y1.max(y));
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "within the ring's u32 length"
            )]
            s.chunks.push(Chunk {
                ring: ring_id,
                first: first as u32,
                count: count as u32,
                rows: (
                    (i64::from(y0) - buffer).div_euclid(extent),
                    (i64::from(y1) + buffer).div_euclid(extent),
                ),
            });
        }
    }
}

/// `len` as a `u32` offset into the piece buffer.
fn offset(len: usize) -> Result<u32, TileError> {
    u32::try_from(len).map_err(|_| TileError::GeometryTooLarge)
}

/// Clip a polygon's `rings` to one `tile` into the scratch's piece buffer, and lend the piece; `None`
/// if the polygon leaves nothing in the tile. See [`Slicer::tile_polygon`](crate::Slicer::tile_polygon).
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub(crate) fn clip_polygon_to_tile<'s, V: PolyVertex, R: AsRef<[V]>>(
    grid: Grid,
    tile: TileId,
    rings: &[R],
    s: &'s mut PolyScratch<V>,
) -> Result<Option<PolygonPiece<'s, V, R>>, TileError> {
    let (min, max) = grid.tile_buffered_bounds(tile)?;
    let origin = grid.origin_of(min);
    s.verts.clear();
    s.ring_ends.clear();
    let Some((exterior, holes)) = rings.split_first() else {
        return Ok(None);
    };
    // If the exterior misses the tile entirely, the whole polygon is absent here.
    if clip_ring(exterior.as_ref(), min, max, &mut s.clip, &mut s.verts)? == RingClip::Outside {
        return Ok(None);
    }
    check_local(&s.verts, origin)?;
    s.ring_ends.push(offset(s.verts.len())?);
    for hole in holes {
        let start = s.verts.len();
        match clip_ring(hole.as_ref(), min, max, &mut s.clip, &mut s.verts)? {
            // A hole covering the whole tile leaves nothing to draw here.
            RingClip::Covers => return Ok(None),
            RingClip::Clipped => {
                check_local(&s.verts[start..], origin)?;
                s.ring_ends.push(offset(s.verts.len())?);
            }
            RingClip::Outside => {}
        }
    }
    Ok(Some(PolygonPiece {
        tile,
        rings: PieceRings::Built {
            verts: &s.verts,
            ends: &s.ring_ends,
            origin,
        },
    }))
}

/// A polygon's `rings` whole, lent in one `tile`'s local frame; `None` if the polygon leaves nothing
/// in the tile — the test [`clip_polygon_to_tile`] makes, without building the piece. See
/// [`Slicer::tile_polygon`](crate::Slicer::tile_polygon) in [`ClipMode::Unclipped`].
pub(crate) fn whole_polygon_in_tile<'s, V: PolyVertex, R: AsRef<[V]>>(
    grid: Grid,
    tile: TileId,
    rings: &'s [R],
    s: &'s mut PolyScratch<V>,
) -> Result<Option<PolygonPiece<'s, V, R>>, TileError> {
    let (min, max) = grid.tile_buffered_bounds(tile)?;
    let origin = grid.origin_of(min);
    s.rings.clear();
    s.copies.clear();
    for (index, ring) in (0_u32..).zip(rings) {
        // (Edge ids only index the sweep's rows.)
        let Some((info, lo, hi)) = load_ring(s, index, ring.as_ref(), 0)? else {
            if index == 0 {
                return Ok(None); // a degenerate exterior drops the polygon
            }
            continue;
        };
        match ring_meets_box(info.pts(rings, &s.copies), min, max) {
            // An exterior missing the tile, or a hole covering it, leaves nothing to draw here.
            RingClip::Outside if index == 0 => return Ok(None),
            RingClip::Covers if index > 0 => return Ok(None),
            _ => {}
        }
        check_local(&[lo, hi], origin)?;
        s.rings.push(info);
    }
    if s.rings.is_empty() {
        return Ok(None); // no rings at all
    }
    Ok(Some(PolygonPiece {
        tile,
        rings: PieceRings::Whole {
            rings: &s.rings,
            input: rings,
            copies: &s.copies,
            origin,
        },
    }))
}
