//! [`PolygonSlicerAll`]: slices whole **multipolygon** features into every tile they reach, producing
//! per tile exactly the rings [`PolygonSlicerOne`](crate::PolygonSlicerOne) produces for that tile.
//!
//! One pass per feature, in three steps (see `docs/polygon-slicer.md` §6/§8):
//!
//! 1. **Route** every ring through [`Grid::route_within`] (one shared tile budget), recording which
//!    edges touch which tile's buffered box — the keep-rule's whole input. Tiles no edge touches are
//!    never visited.
//! 2. **Index** where each edge crosses each tile row's center line `y = cy`. That line carries the
//!    winding ray of every tile in the row, so one sorted list per row answers both remaining
//!    questions: how far an excursion outside a tile winds around it (the detour's shape) and whether
//!    a ring with no edge in a tile contains it (ray parity).
//! 3. **Sweep** each row left to right over its edge tiles, closing every touching ring's arcs with
//!    the shared [`close_ring`] core. A Fenwick tree over the row's crossings (in edge order) answers
//!    "signed crossings right of this tile among edges `i..j`" in `O(log n)`, so the cost is
//!    proportional to the routing work plus the crossings, never to ring length × tiles.

use core::cmp::Ordering;
use core::ops::Range;

use geo_types::Coord;

use crate::TileError;
use crate::clip_polygon::{close_ring, push_distinct, push_fill_box};
use crate::clip_polyline::to_local;
use crate::geom::{crossing_x_ceil, ring_orientation};
use crate::grid::{Grid, MAX_TILE_VISITS, RouteSink};
use crate::polygon_view::{PolygonView, copy_view, offset, span};
use crate::tile::TileId;
use crate::vertex::PolyVertex;

/// The most vertices one feature may add to the output. Each detour has fewer corners than the
/// vertices it replaces, so a tile's output stays within its rings' size, but a ring winding many times
/// around many tiles (or many overlapping polygons) can still multiply that across tiles; valid
/// geometry stays far below this.
const MAX_FEATURE_VERTICES: usize = 1 << 28;

/// One ring of the feature being added. Its distinct vertices are `pts[start .. start + len]`, followed
/// by a copy of the first so it routes as a closed polyline; edge `i` is global edge `start + i`.
#[derive(Debug, Clone, Copy)]
struct RingInfo {
    start: u32,
    len: u32,
    poly: u32,
    hole: bool,
}

/// One polygon of the feature (only those with a non-degenerate exterior): its exterior ring `first`
/// (its holes follow it) and the exterior's winding (for a fill box).
#[derive(Debug, Clone, Copy)]
struct PolyInfo {
    first: u32,
    orient: Ordering,
}

/// A ring edge touching a tile's buffered box (from routing). Sorted by `(ty, tx, edge)`: rows in
/// order for the sweep, and within a tile edges in ring (hence polygon) order.
#[derive(Debug, Clone, Copy)]
struct Hit {
    ty: i32,
    tx: i32,
    edge: u32,
    ring: u32,
}

/// A ring edge crossing row `ty`'s center line, the line of every winding ray in that row (the
/// half-open rule of [`ray_crossing`](crate::clip_polygon) — each crossing counted once). The crossing
/// lies right of tile `tx`'s buffered box — on its winding ray — iff `tx <= right_of`.
#[derive(Debug, Clone, Copy)]
struct Crossing {
    ty: i32,
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

/// Collects routing hits for one ring, numbering its edges globally.
struct HitSink<'a> {
    hits: &'a mut Vec<Hit>,
    ring: u32,
    next: u32,
    edge: u32,
}

impl<V: PolyVertex> RouteSink<V> for HitSink<'_> {
    fn begin_polyline(&mut self) {}

    fn begin_segment(&mut self) {
        // The routed ring has no consecutive duplicates, so every edge is one segment, in order.
        self.edge = self.next;
        self.next += 1;
    }

    fn emit(&mut self, tile: TileId, _: Coord<i32>, _: V, _: V) -> Result<(), TileError> {
        self.hits.push(Hit {
            ty: tile.y,
            tx: tile.x,
            edge: self.edge,
            ring: self.ring,
        });
        Ok(())
    }
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

/// Re-express freshly written vertices in the tile-local frame.
fn localize<V: PolyVertex>(verts: &mut [V], origin: Coord<i32>) -> Result<(), TileError> {
    for v in verts {
        *v = to_local(*v, origin)?;
    }
    Ok(())
}

/// The feature being added, copied once: distinct ring vertices plus ring/polygon structure.
#[derive(Debug, Clone)]
struct Input<V> {
    pts: Vec<V>,
    rings: Vec<RingInfo>,
    polys: Vec<PolyInfo>,
}

impl<V: PolyVertex> Input<V> {
    /// Copy one feature, dropping degenerate rings (fewer than 3 distinct vertices) — and a polygon
    /// whose exterior is degenerate — exactly as the single-tile clip treats them.
    fn load<P>(&mut self, polygons: P) -> Result<(), TileError>
    where
        P: IntoIterator,
        P::Item: IntoIterator,
        <P::Item as IntoIterator>::Item: AsRef<[V]>,
    {
        self.pts.clear();
        self.rings.clear();
        self.polys.clear();
        for polygon in polygons {
            let mut rings = polygon.into_iter();
            let Some(exterior) = rings.next() else {
                continue;
            };
            let first = offset(self.rings.len())?;
            let poly = offset(self.polys.len())?;
            if !self.push_ring(exterior.as_ref(), poly, false)? {
                continue;
            }
            for hole in rings {
                self.push_ring(hole.as_ref(), poly, true)?;
            }
            let ext = self.rings[first as usize];
            let ext_pts = &self.pts[ext.start as usize..(ext.start + ext.len) as usize];
            self.polys.push(PolyInfo {
                first,
                orient: ring_orientation(ext_pts),
            });
        }
        Ok(())
    }

    /// Append one ring's distinct vertices (closed with a copy of the first), if it has at least 3.
    fn push_ring(&mut self, ring: &[V], poly: u32, hole: bool) -> Result<bool, TileError> {
        let start = self.pts.len();
        let len = push_distinct(ring, &mut self.pts);
        if len < 3 {
            self.pts.truncate(start);
            return Ok(false);
        }
        self.pts.push(self.pts[start]);
        // The closing copy's index must fit too: it is the head of the last edge.
        offset(self.pts.len())?;
        self.rings.push(RingInfo {
            start: offset(start)?,
            len: offset(len)?,
            poly,
            hole,
        });
        Ok(true)
    }

    /// The ring's distinct vertices (without the closing copy).
    fn ring_pts(&self, ring: RingInfo) -> &[V] {
        &self.pts[ring.start as usize..(ring.start + ring.len) as usize]
    }
}

/// A horizontal run of tiles lying **entirely inside** a feature: row `y`, columns `x.start..x.end`.
///
/// No ring edge touches these tiles' buffered boxes, so each is uniformly covered — render it as a
/// solid fill of its buffered box (where [`PolygonSlicerOne`](crate::PolygonSlicerOne) emits one
/// all-synthetic box ring per covering polygon). Runs are maximal, never overlap the feature's edge
/// tiles, and come in row-major order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct FillRun {
    /// The tile row.
    pub y: i32,
    /// The tile columns, end-exclusive.
    pub x: Range<i32>,
}

/// Where each feature's geometry lands, in flat arenas (no per-tile allocations): tile-local ring
/// vertices, ring/polygon/tile end offsets, and fill runs.
#[derive(Debug, Clone)]
struct Pieces<V> {
    verts: Vec<V>,
    ring_ends: Vec<u32>,
    poly_ends: Vec<u32>,
    tiles: Vec<TileEntry>,
    runs: Vec<FillRun>,
}

/// One edge tile of one feature: its id and the end of its polygons in `poly_ends`.
#[derive(Debug, Clone, Copy)]
struct TileEntry {
    tile: TileId,
    poly_end: u32,
}

/// Arena lengths before a feature, to roll back to on error.
#[derive(Clone, Copy, Default)]
struct Savepoint {
    verts: usize,
    ring_ends: usize,
    poly_ends: usize,
    tiles: usize,
    runs: usize,
}

impl<V> Pieces<V> {
    fn savepoint(&self) -> Savepoint {
        Savepoint {
            verts: self.verts.len(),
            ring_ends: self.ring_ends.len(),
            poly_ends: self.poly_ends.len(),
            tiles: self.tiles.len(),
            runs: self.runs.len(),
        }
    }

    fn rollback(&mut self, s: Savepoint) {
        self.verts.truncate(s.verts);
        self.ring_ends.truncate(s.ring_ends);
        self.poly_ends.truncate(s.poly_ends);
        self.tiles.truncate(s.tiles);
        self.runs.truncate(s.runs);
    }

    fn clear(&mut self) {
        self.rollback(Savepoint::default());
    }

    fn shrink_to_fit(&mut self) {
        self.verts.shrink_to_fit();
        self.ring_ends.shrink_to_fit();
        self.poly_ends.shrink_to_fit();
        self.tiles.shrink_to_fit();
        self.runs.shrink_to_fit();
    }

    /// Record covered tiles `start..end` of row `y`, extending the row's previous run when they abut.
    /// `row_runs` is where this row's runs begin, so runs never merge across rows or features.
    fn fill(&mut self, y: i32, start: i64, end: i64, row_runs: usize) -> Result<(), TileError> {
        if start >= end {
            return Ok(());
        }
        let start = i32::try_from(start).map_err(|_| TileError::Overflow)?;
        let end = i32::try_from(end).map_err(|_| TileError::Overflow)?;
        let row_len = self.runs.len() - row_runs;
        match self.runs.last_mut() {
            Some(run) if row_len > 0 && run.x.end == start => run.x.end = end,
            _ => self.runs.push(FillRun { y, x: start..end }),
        }
        Ok(())
    }
}

/// Reusable working memory for one feature's routing and row sweep (kept across features).
#[derive(Debug, Clone)]
struct Scratch {
    hits: Vec<Hit>,
    crossings: Vec<Crossing>,
    /// The current row's crossings (indices into its edge-ordered slice), by `right_of`.
    by_x: Vec<u32>,
    /// Signs of the current row's crossings still right of the sweep, in edge order.
    fenwick: Vec<i64>,
    /// Per ring: odd crossings right of the current tile (= the ring contains the tile, when no edge
    /// of it touches the tile).
    inside: Vec<bool>,
    /// Per polygon: how many of its holes contain the current tile (by the same parity).
    holes_in: Vec<u32>,
    /// Polygons containing the current tile (exterior inside, no hole inside), as a swap-remove set.
    covering: Vec<u32>,
    cover_pos: Vec<usize>,
    /// The output length the current feature must not pass ([`MAX_FEATURE_VERTICES`] past its start).
    vert_limit: usize,
    /// Per-tile temporaries.
    edges: Vec<u32>,
    arcs: Vec<(usize, usize)>,
    touched: Vec<u32>,
    extra: Vec<u32>,
}

impl Scratch {
    const fn new() -> Self {
        Self {
            hits: Vec::new(),
            crossings: Vec::new(),
            by_x: Vec::new(),
            fenwick: Vec::new(),
            inside: Vec::new(),
            holes_in: Vec::new(),
            covering: Vec::new(),
            cover_pos: Vec::new(),
            vert_limit: 0,
            edges: Vec::new(),
            arcs: Vec::new(),
            touched: Vec::new(),
            extra: Vec::new(),
        }
    }

    /// Route every ring, recording its tile hits, all sharing one candidate-tile budget.
    fn route<V: PolyVertex>(&mut self, grid: Grid, input: &Input<V>) -> Result<(), TileError> {
        self.hits.clear();
        let mut budget = MAX_TILE_VISITS;
        for (ring, info) in (0..).zip(&input.rings) {
            let start = info.start as usize;
            let closed = &input.pts[start..=start + info.len as usize];
            let mut sink = HitSink {
                hits: &mut self.hits,
                ring,
                next: info.start,
                edge: 0,
            };
            grid.route_within(closed, &mut sink, &mut budget)?;
        }
        self.hits.sort_unstable_by_key(|h| (h.ty, h.tx, h.edge));
        Ok(())
    }

    /// Index every edge's crossings of the tile-row center lines it spans. An edge spans no more rows
    /// than routing charged it, so this is bounded by the routing budget.
    fn index_crossings<V: PolyVertex>(
        &mut self,
        grid: Grid,
        input: &Input<V>,
    ) -> Result<(), TileError> {
        self.crossings.clear();
        let extent = i64::from(grid.extent());
        let buffer = i64::from(grid.buffer());
        // Row `ty`'s center line: the midpoint of its buffered box, as the clip computes it.
        let center =
            |ty: i64| i64::midpoint(ty * extent - buffer, ty * extent + extent - 1 + buffer);
        for (ring, info) in (0..).zip(&input.rings) {
            for edge in info.start..info.start + info.len {
                let a = input.pts[edge as usize].position();
                let b = input.pts[edge as usize + 1].position();
                let (lo, hi) = (i64::from(a.y.min(b.y)), i64::from(a.y.max(b.y)));
                // Rows whose center `cy` satisfies `lo <= cy < hi` (the half-open crossing rule).
                let mut ty = lo.div_euclid(extent) - 1;
                while center(ty) < lo {
                    ty += 1;
                }
                while center(ty) < hi {
                    let cy = i32::try_from(center(ty)).map_err(|_| TileError::Overflow)?;
                    // Right of tile `tx`'s box iff x > tx·extent + extent − 1 + buffer.
                    let x = crossing_x_ceil(a, b, cy);
                    self.crossings.push(Crossing {
                        ty: i32::try_from(ty).map_err(|_| TileError::Overflow)?,
                        edge,
                        ring,
                        up: b.y > a.y,
                        right_of: (x - extent - buffer).div_euclid(extent),
                    });
                    ty += 1;
                }
            }
        }
        self.crossings.sort_unstable_by_key(|c| (c.ty, c.edge));
        Ok(())
    }

    /// Emit every edge tile of the feature into `out`, row by row, adding at most `max_verts` vertices.
    fn sweep<V: PolyVertex>(
        &mut self,
        grid: Grid,
        input: &Input<V>,
        out: &mut Pieces<V>,
        max_verts: usize,
    ) -> Result<(), TileError> {
        self.vert_limit = out.verts.len().saturating_add(max_verts);
        // Containment state starts (and, since every ring crosses a line an even number of times, ends
        // each row) empty.
        self.inside.clear();
        self.inside.resize(input.rings.len(), false);
        self.holes_in.clear();
        self.holes_in.resize(input.polys.len(), 0);
        self.cover_pos.clear();
        self.cover_pos.resize(input.polys.len(), 0);
        self.covering.clear();

        let hits = core::mem::take(&mut self.hits);
        let crossings = core::mem::take(&mut self.crossings);
        let result = self.sweep_rows(grid, input, &hits, &crossings, out);
        self.hits = hits;
        self.crossings = crossings;
        result
    }

    fn sweep_rows<V: PolyVertex>(
        &mut self,
        grid: Grid,
        input: &Input<V>,
        hits: &[Hit],
        crossings: &[Crossing],
        out: &mut Pieces<V>,
    ) -> Result<(), TileError> {
        let (mut h, mut c) = (0, 0);
        loop {
            let ty = match (hits.get(h), crossings.get(c)) {
                (Some(a), Some(b)) => a.ty.min(b.ty),
                (Some(a), None) => a.ty,
                (None, Some(b)) => b.ty,
                (None, None) => return Ok(()),
            };
            let h_end = h + hits[h..].partition_point(|x| x.ty == ty);
            let c_end = c + crossings[c..].partition_point(|x| x.ty == ty);
            self.sweep_row(grid, input, ty, &hits[h..h_end], &crossings[c..c_end], out)?;
            (h, c) = (h_end, c_end);
        }
    }

    /// One tile row: walk its edge tiles left to right, retiring each crossing from the winding rays
    /// (and toggling its ring's containment) once the sweep passes it. Between consecutive crossings
    /// the containment state is constant, so while some polygon covers it, every tile there that is
    /// not an edge tile is filled — one run, however many tiles.
    fn sweep_row<V: PolyVertex>(
        &mut self,
        grid: Grid,
        input: &Input<V>,
        ty: i32,
        hits: &[Hit],
        cs: &[Crossing],
        out: &mut Pieces<V>,
    ) -> Result<(), TileError> {
        self.by_x.clear();
        self.by_x.extend(0..offset(cs.len())?);
        self.by_x.sort_unstable_by_key(|&i| cs[i as usize].right_of);
        fenwick_build(&mut self.fenwick, cs.iter().map(|c| c.sign()));

        let row_runs = out.runs.len();
        let (mut k, mut h) = (0, 0);
        // The first tile not yet emitted or filled.
        let mut next = i64::MIN;
        loop {
            // Tiles `next..=bound` all see the same crossings on their winding rays.
            let bound = self
                .by_x
                .get(k)
                .map_or(i64::MAX, |&i| cs[i as usize].right_of);
            let covered = !self.covering.is_empty();
            while let Some(hit) = hits.get(h)
                && i64::from(hit.tx) <= bound
            {
                let tx = i64::from(hit.tx);
                if covered {
                    out.fill(ty, next, tx, row_runs)?;
                }
                let end = h + hits[h..].partition_point(|x| x.tx == hit.tx);
                self.tile(grid, input, TileId::new(hit.tx, ty), &hits[h..end], cs, out)?;
                // One tile's output is bounded by the feature's size; only the sum over tiles is not.
                if out.verts.len() > self.vert_limit {
                    return Err(TileError::OutputTooLarge);
                }
                (h, next) = (end, tx + 1);
            }
            if k == self.by_x.len() {
                // Past every crossing each ring's parity is even again: nothing covers.
                return Ok(());
            }
            // `bound` is a crossing's `right_of` (|·| ≤ 2^32), so `+ 1` cannot overflow.
            if covered {
                out.fill(ty, next, bound + 1, row_runs)?;
            }
            next = bound + 1;
            // Tiles past `bound` have these crossings on their left.
            while let Some(&i) = self.by_x.get(k)
                && cs[i as usize].right_of == bound
            {
                let crossing = cs[i as usize];
                self.toggle(input, crossing);
                fenwick_add(&mut self.fenwick, i as usize, -crossing.sign());
                k += 1;
            }
        }
    }

    /// The sweep passed `crossing`: flip its ring's containment and update which polygons cover.
    fn toggle<V>(&mut self, input: &Input<V>, crossing: Crossing) {
        let ring = input.rings[crossing.ring as usize];
        let p = ring.poly as usize;
        let ext = input.polys[p].first as usize;
        let covers = |s: &Self| s.inside[ext] && s.holes_in[p] == 0;
        let was = covers(self);
        let r = crossing.ring as usize;
        self.inside[r] = !self.inside[r];
        if ring.hole {
            if self.inside[r] {
                self.holes_in[p] += 1;
            } else {
                // Parity alternates from an empty row start, so a hole only leaves after entering.
                self.holes_in[p] = self.holes_in[p].saturating_sub(1);
            }
        }
        match (was, covers(self)) {
            (false, true) => {
                self.cover_pos[p] = self.covering.len();
                self.covering.push(ring.poly);
            }
            (true, false) => {
                let i = self.cover_pos[p];
                self.covering.swap_remove(i);
                if let Some(&moved) = self.covering.get(i) {
                    self.cover_pos[moved as usize] = i;
                }
            }
            _ => {}
        }
    }

    /// Emit one edge tile: every polygon in input order, exactly as the single-tile clip would.
    fn tile<V: PolyVertex>(
        &mut self,
        grid: Grid,
        input: &Input<V>,
        tile: TileId,
        hits: &[Hit],
        cs: &[Crossing],
        out: &mut Pieces<V>,
    ) -> Result<(), TileError> {
        let (min, max) = grid.tile_buffered_bounds(tile)?;
        let origin = tile.origin(grid.extent()).ok_or(TileError::Overflow)?;
        // Polygons with an edge here, ascending (hits are in edge order; rings and polygons in input
        // order).
        self.touched.clear();
        for hit in hits {
            let p = input.rings[hit.ring as usize].poly;
            if self.touched.last() != Some(&p) {
                self.touched.push(p);
            }
        }
        // Polygons covering this tile with no edge here. Only overlapping parts of an (invalid)
        // multipolygon can do that; each contributes a fill box, as in the single-tile clip.
        self.extra.clear();
        let touched = &self.touched;
        self.extra.extend(
            self.covering
                .iter()
                .filter(|p| touched.binary_search(p).is_err()),
        );
        self.extra.sort_unstable();

        let polys_before = out.poly_ends.len();
        // Merge both (ascending, disjoint) polygon lists so the output follows input order.
        let (mut next_touched, mut next_extra, mut hit) = (0, 0, 0);
        loop {
            let touched = self.touched.get(next_touched).copied();
            let extra = self.extra.get(next_extra).copied();
            match (touched, extra) {
                (Some(poly), fill) if fill.is_none_or(|fill| poly < fill) => {
                    let n =
                        hits[hit..].partition_point(|x| input.rings[x.ring as usize].poly == poly);
                    let poly_hits = &hits[hit..hit + n];
                    self.polygon(input, poly, poly_hits, cs, min, max, origin, out)?;
                    next_touched += 1;
                    hit += n;
                }
                (_, Some(fill)) => {
                    let ring_start = out.verts.len();
                    push_fill_box(&mut out.verts, input.polys[fill as usize].orient, min, max)?;
                    localize(&mut out.verts[ring_start..], origin)?;
                    out.ring_ends.push(offset(out.verts.len())?);
                    out.poly_ends.push(offset(out.ring_ends.len())?);
                    next_extra += 1;
                }
                _ => break,
            }
        }
        if out.poly_ends.len() > polys_before {
            out.tiles.push(TileEntry {
                tile,
                poly_end: offset(out.poly_ends.len())?,
            });
        }
        Ok(())
    }

    /// Emit polygon `p` into the current tile from its rings' `hits` here: touching rings are clipped,
    /// the rest decided by containment (an exterior missing the tile or a hole covering it drops the
    /// polygon).
    ///
    /// Only the touching rings are visited, so a polygon with many holes costs its hits here, not its
    /// ring count: the untouched holes are settled at once by how many of them contain the tile.
    #[expect(
        clippy::too_many_arguments,
        reason = "the tile context, passed down once per polygon"
    )]
    fn polygon<V: PolyVertex>(
        &mut self,
        input: &Input<V>,
        p: u32,
        hits: &[Hit],
        cs: &[Crossing],
        min: Coord<i32>,
        max: Coord<i32>,
        origin: Coord<i32>,
        out: &mut Pieces<V>,
    ) -> Result<(), TileError> {
        let info = input.polys[p as usize];
        // Hits come in edge order, so the exterior's (if any) come first, then each hole's.
        let exterior_touched = hits.first().is_some_and(|x| x.ring == info.first);
        if !exterior_touched && !self.inside[info.first as usize] {
            return Ok(()); // the exterior misses the tile
        }
        // `holes_in` counts every hole whose ray parity is odd, but a touched hole's parity says
        // nothing about the tile: what remains are untouched holes containing it, which leave nothing
        // to draw.
        let holes_in = self.holes_in[p as usize] as usize;
        if holes_in > 0
            && holes_in
                > ring_groups(hits)
                    .filter(|g| g[0].ring != info.first && self.inside[g[0].ring as usize])
                    .count()
        {
            return Ok(());
        }
        if !exterior_touched {
            let ring_start = out.verts.len();
            push_fill_box(&mut out.verts, info.orient, min, max)?;
            localize(&mut out.verts[ring_start..], origin)?;
            out.ring_ends.push(offset(out.verts.len())?);
        }
        for ring_hits in ring_groups(hits) {
            let ring = input.rings[ring_hits[0].ring as usize];
            let ring_start = out.verts.len();
            self.edges.clear();
            self.edges
                .extend(ring_hits.iter().map(|x| x.edge - ring.start));
            let fenwick = &self.fenwick;
            let winding =
                |first: usize, count: usize| excursion_winding(cs, fenwick, ring, first, count);
            close_ring(
                input.ring_pts(ring),
                &self.edges,
                min,
                max,
                winding,
                &mut self.arcs,
                &mut out.verts,
            )?;
            localize(&mut out.verts[ring_start..], origin)?;
            out.ring_ends.push(offset(out.verts.len())?);
        }
        out.poly_ends.push(offset(out.ring_ends.len())?);
        Ok(())
    }
}

/// `hits` (one tile's, in edge order, so each ring's are contiguous) split into runs of the same ring,
/// with one binary search per ring rather than a comparison per hit.
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
    let (start, len) = (ring.start as usize, ring.len as usize);
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

/// One recorded feature: the ends of its edge tiles and fill runs in [`Pieces`], and its attribute.
#[derive(Debug, Clone)]
struct FeatureEntry<A> {
    tiles_end: u32,
    runs_end: u32,
    attr: A,
}

/// Slices integer **multipolygon** features into every tile they reach, keeping original vertices.
///
/// The polygon counterpart to [`SlicerAll`](crate::SlicerAll), and the all-tiles counterpart to
/// [`PolygonSlicerOne`](crate::PolygonSlicerOne): for every tile a feature's ring edges touch (its
/// **edge tiles**), the pieces are exactly what a `PolygonSlicerOne` bound to that tile yields for each
/// of the feature's polygons, in order. Generic over the [`PolyVertex`] type `V` (default
/// [`Coord<i32>`]) and the per-feature attribute `A` (default `()`).
///
/// Reading back is per feature ([`iter_features`](Self::iter_features)), then per edge tile, polygon,
/// and ring. Output rings keep the winding of the input rings they come from (see
/// [`signed_area_2x`](crate::signed_area_2x) to normalize it first).
///
/// The cost is proportional to the routing work (the edge tiles) plus the ring edges' crossings of
/// tile-row center lines — independent of ring length per tile. All storage is flat and reused:
/// [`clear`](Self::clear) keeps every buffer's capacity, so one slicer per worker, cleared between
/// features, stops allocating once warmed up.
#[derive(Debug, Clone)]
pub struct PolygonSlicerAll<V: PolyVertex = Coord<i32>, A = ()> {
    grid: Grid,
    pieces: Pieces<V>,
    /// Per recorded feature: where its tiles and fill runs end, and its attribute.
    features: Vec<FeatureEntry<A>>,
    input: Input<V>,
    scratch: Scratch,
    /// [`MAX_FEATURE_VERTICES`], lowered by tests.
    max_feature_verts: usize,
}

impl<V: PolyVertex, A> PolygonSlicerAll<V, A> {
    /// Create a slicer with the given tile side / per-tile output resolution `extent` and `buffer`
    /// (same coordinate model as [`SlicerAll`](crate::SlicerAll)).
    ///
    /// # Errors
    ///
    /// - [`TileError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    /// - [`TileError::BufferTooLarge`] if `buffer` is not strictly less than half the `extent`.
    pub fn new(extent: u32, buffer: u16) -> Result<Self, TileError> {
        Ok(Self {
            grid: Grid::new(extent, buffer)?,
            pieces: Pieces {
                verts: Vec::new(),
                ring_ends: Vec::new(),
                poly_ends: Vec::new(),
                tiles: Vec::new(),
                runs: Vec::new(),
            },
            features: Vec::new(),
            input: Input {
                pts: Vec::new(),
                rings: Vec::new(),
                polys: Vec::new(),
            },
            scratch: Scratch::new(),
            max_feature_verts: MAX_FEATURE_VERTICES,
        })
    }

    /// The tile side / per-tile output resolution.
    #[must_use]
    pub fn extent(&self) -> u32 {
        self.grid.extent()
    }

    /// The buffer kept around every tile, in tile-space units.
    #[must_use]
    pub fn buffer(&self) -> u16 {
        self.grid.buffer()
    }

    /// Add one multipolygon feature carrying `attr`, slicing it into every tile it reaches. Chainable.
    ///
    /// `polygons` yields each polygon as its rings — exterior first, then holes — each a vertex slice,
    /// open or closed (a repeated first/last vertex is fine), e.g. `[[&exterior[..], &hole[..]]]`.
    /// Rings with fewer than 3 distinct vertices are ignored, and so is a polygon whose exterior is.
    /// The feature (and `attr`) is recorded only if some tile receives geometry.
    ///
    /// When `A = ()`, prefer [`add_feature`](Self::add_feature).
    ///
    /// **Atomic:** on error nothing is recorded, so the slicer stays usable — skip the input and go on.
    ///
    /// # Errors
    ///
    /// - [`TileError::TooManyTiles`] if a ring spans more than `i16::MAX` tiles on an axis, or routing
    ///   the feature's rings would examine too many candidate tiles (one budget per feature).
    /// - [`TileError::Overflow`] if coordinate math overflows `i32` (geometry too near its limits).
    /// - [`TileError::OutputTooLarge`] if the feature's pieces would exceed 2^28 vertices (only rings
    ///   winding around tiles many times, or many overlapping polygons, get there).
    /// - [`TileError::GeometryTooLarge`] if the geometry exceeds the `u32` indexing of the storage.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn add_feature_with<P>(&mut self, polygons: P, attr: A) -> Result<&mut Self, TileError>
    where
        P: IntoIterator,
        P::Item: IntoIterator,
        <P::Item as IntoIterator>::Item: AsRef<[V]>,
    {
        self.input.load(polygons)?;
        self.scratch.route(self.grid, &self.input)?;
        self.scratch.index_crossings(self.grid, &self.input)?;
        let save = self.pieces.savepoint();
        let ends = self
            .scratch
            .sweep(
                self.grid,
                &self.input,
                &mut self.pieces,
                self.max_feature_verts,
            )
            .and_then(|()| {
                Ok((
                    offset(self.pieces.tiles.len())?,
                    offset(self.pieces.runs.len())?,
                ))
            });
        match ends {
            Ok((tiles_end, runs_end)) => {
                if self.pieces.tiles.len() > save.tiles || self.pieces.runs.len() > save.runs {
                    self.features.push(FeatureEntry {
                        tiles_end,
                        runs_end,
                        attr,
                    });
                }
                Ok(self)
            }
            Err(err) => {
                self.pieces.rollback(save);
                Err(err)
            }
        }
    }

    /// Iterate the recorded features, in the order added.
    pub fn iter_features(&self) -> impl Iterator<Item = PolygonFeatureView<'_, V, A>> {
        let pieces = &self.pieces;
        let features = &self.features;
        (0..features.len()).map(move |f| {
            let (tiles_start, runs_start) = f
                .checked_sub(1)
                .map_or((0, 0), |p| (features[p].tiles_end, features[p].runs_end));
            let feature = &features[f];
            PolygonFeatureView {
                pieces,
                tiles_start: tiles_start as usize,
                tiles_end: feature.tiles_end as usize,
                runs: &pieces.runs[runs_start as usize..feature.runs_end as usize],
                attr: &feature.attr,
            }
        })
    }

    /// Number of features recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether nothing has been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Discard everything recorded, keeping the extent/buffer config and every buffer's capacity.
    pub fn clear(&mut self) {
        self.pieces.clear();
        self.features.clear();
    }

    /// Release the memory not needed for what is recorded, including the working memory a large
    /// feature left behind: [`clear`](Self::clear) keeps every buffer's capacity, so after an outlier
    /// a long-lived slicer would otherwise hold its peak forever.
    pub fn shrink_to_fit(&mut self) {
        self.pieces.shrink_to_fit();
        self.features.shrink_to_fit();
        self.input = Input {
            pts: Vec::new(),
            rings: Vec::new(),
            polys: Vec::new(),
        };
        self.scratch = Scratch::new();
    }
}

impl<V: PolyVertex> PolygonSlicerAll<V, ()> {
    /// Add one multipolygon feature with no attribute — shorthand for
    /// [`add_feature_with`](Self::add_feature_with)`(polygons, ())`. Available only when `A = ()`.
    ///
    /// # Errors
    ///
    /// As in [`add_feature_with`](Self::add_feature_with).
    pub fn add_feature<P>(&mut self, polygons: P) -> Result<&mut Self, TileError>
    where
        P: IntoIterator,
        P::Item: IntoIterator,
        <P::Item as IntoIterator>::Item: AsRef<[V]>,
    {
        self.add_feature_with(polygons, ())
    }
}

/// A borrowed view of one feature added to a [`PolygonSlicerAll`].
pub struct PolygonFeatureView<'a, V: PolyVertex, A = ()> {
    pieces: &'a Pieces<V>,
    /// The feature's edge tiles, `tiles_start..tiles_end` in `pieces.tiles`.
    tiles_start: usize,
    tiles_end: usize,
    runs: &'a [FillRun],
    attr: &'a A,
}

impl<'a, V: PolyVertex, A> PolygonFeatureView<'a, V, A> {
    /// The feature's attribute, as passed to
    /// [`add_feature_with`](PolygonSlicerAll::add_feature_with).
    #[must_use]
    pub fn attr(&self) -> &'a A {
        self.attr
    }

    /// Iterate the feature's edge tiles — every tile a ring edge touches that received geometry — in
    /// row-major order (by `y`, then `x`).
    pub fn iter_tiles(&self) -> impl Iterator<Item = PolygonTileView<'a, V, A>> + use<'a, V, A> {
        let (pieces, attr) = (self.pieces, self.attr);
        (self.tiles_start..self.tiles_end).map(move |index| PolygonTileView {
            pieces,
            index,
            attr,
        })
    }

    /// Iterate the feature's fully covered tiles as [`FillRun`]s, in row-major order. Storage is per
    /// run, not per tile, so a feature covering millions of tiles costs one run per row of them.
    pub fn iter_fill_runs(&self) -> impl Iterator<Item = FillRun> + use<'a, V, A> {
        self.runs.iter().cloned()
    }
}

/// A borrowed view of one feature's pieces in one edge tile.
pub struct PolygonTileView<'a, V: PolyVertex, A = ()> {
    pieces: &'a Pieces<V>,
    index: usize,
    attr: &'a A,
}

impl<'a, V: PolyVertex, A> PolygonTileView<'a, V, A> {
    /// The tile this view is for.
    #[must_use]
    pub fn tile_id(&self) -> TileId {
        self.pieces.tiles[self.index].tile
    }

    /// Iterate the clipped polygons in this tile, in input-polygon order (at most one per input
    /// polygon), each carrying the feature's attribute.
    pub fn iter_polygons(&self) -> impl Iterator<Item = PolygonView<'a, V, A>> + use<'a, V, A> {
        let (pieces, attr) = (self.pieces, self.attr);
        let start = self
            .index
            .checked_sub(1)
            .map_or(0, |prev| pieces.tiles[prev].poly_end as usize);
        (start..pieces.tiles[self.index].poly_end as usize).map(move |p| {
            PolygonView::new(
                &pieces.verts,
                &pieces.ring_ends,
                span(&pieces.poly_ends, p),
                attr,
            )
        })
    }
}

copy_view!(PolygonFeatureView<A>: PolyVertex, PolygonTileView<A>: PolyVertex);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenwick_matches_brute_force() {
        let values = [3, -1, 4, 1, -5, 9, 2, -6, 5, 3, -5];
        let mut current = values.to_vec();
        let mut tree = Vec::new();
        fenwick_build(&mut tree, values.iter().copied());
        for (i, delta) in [(4, 5), (0, -3), (10, 7), (6, -2)] {
            fenwick_add(&mut tree, i, delta);
            current[i] += delta;
            for end in 0..=current.len() {
                assert_eq!(
                    fenwick_prefix(&tree, end),
                    current[..end].iter().sum::<i64>()
                );
            }
        }
    }

    #[test]
    fn output_cap_rejects_the_feature_atomically() {
        let small = [(5, 5), (20, 5), (20, 20), (5, 20)].map(|(x, y)| Coord { x, y });
        let big = [(5, 5), (190, 5), (190, 190), (5, 190)].map(|(x, y)| Coord { x, y });
        let mut s = PolygonSlicerAll::<Coord<i32>>::new(25, 2).expect("config");
        s.add_feature([[&small[..]]]).expect("slice");
        let before = format!("{:?}", s.pieces);
        s.max_feature_verts = 40;
        let err = s.add_feature([[&big[..]]]).err();
        assert_eq!(err, Some(TileError::OutputTooLarge));
        assert_eq!(s.len(), 1, "the rejected feature is not recorded");
        assert_eq!(format!("{:?}", s.pieces), before, "nor any of its pieces");
        s.max_feature_verts = MAX_FEATURE_VERTICES;
        s.add_feature([[&big[..]]]).expect("under the real cap");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn shrink_to_fit_releases_working_memory() {
        let ring = [(5, 5), (190, 5), (190, 190), (5, 190)].map(|(x, y)| Coord { x, y });
        let mut s = PolygonSlicerAll::<Coord<i32>>::new(25, 2).expect("config");
        s.add_feature([[&ring[..]]]).expect("slice");
        let output = format!("{:?}", s.pieces);
        s.clear();
        assert!(s.scratch.hits.capacity() > 0 && s.pieces.verts.capacity() > 0);
        s.shrink_to_fit();
        assert_eq!(s.scratch.hits.capacity(), 0);
        assert_eq!(s.input.pts.capacity(), 0);
        assert_eq!(s.pieces.verts.capacity(), 0);
        s.add_feature([[&ring[..]]]).expect("slice");
        assert_eq!(
            format!("{:?}", s.pieces),
            output,
            "a shrunk slicer works the same"
        );
    }

    #[test]
    fn clear_keeps_capacity() {
        let ring = [(5, 5), (190, 5), (190, 190), (5, 190)].map(|(x, y)| Coord { x, y });
        let hole = [(60, 60), (60, 120), (120, 120), (120, 60)].map(|(x, y)| Coord { x, y });
        let mut s = PolygonSlicerAll::<Coord<i32>>::new(25, 2).expect("config");
        s.add_feature([[&ring[..], &hole[..]]]).expect("slice");
        let capacities = |s: &PolygonSlicerAll<Coord<i32>>| {
            [
                s.pieces.verts.capacity(),
                s.pieces.ring_ends.capacity(),
                s.pieces.tiles.capacity(),
                s.pieces.runs.capacity(),
                s.features.capacity(),
                s.input.pts.capacity(),
                s.scratch.hits.capacity(),
                s.scratch.crossings.capacity(),
            ]
        };
        let warm = capacities(&s);
        let output = format!("{:?}", s.pieces);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(capacities(&s), warm, "clear keeps every buffer");
        s.add_feature([[&ring[..], &hole[..]]]).expect("slice");
        assert_eq!(
            capacities(&s),
            warm,
            "a warmed-up slicer does not grow for the same feature"
        );
        assert_eq!(format!("{:?}", s.pieces), output);
    }
}
