//! Stateful reassembly of sliced tiles back into whole features — the accumulating inverse of
//! [`SlicerAll`](crate::SlicerAll).
//!
//! Feed [`Mosaic::add`] each tile's runs in its local frame; the mosaic rebases them into the global
//! frame (`local + tile·extent`) and links geometry across borders by shared **directed edges**.
//! [`add`](Mosaic::add) rejects a tile that is inconsistent with those already present — naming the
//! ones it conflicts with, and leaving the mosaic unchanged — in two ways:
//!
//! - **Payload:** slicing duplicates a border-crossing segment identically (position **and** payload)
//!   in every tile that carries it, so a shared edge whose vertices *disagree* is a conflict. (A single
//!   position may be shared by several features — edges only collide when their whole segment does — so
//!   unrelated features meeting at a point never conflict.)
//! - **Membership:** every tile is complete in its own *core* (the cell it owns), so an edge with an
//!   endpoint in a present tile's core must be carried by that tile. A present tile that owns an
//!   endpoint but lacks the edge — a line spanning into a neighbor the neighbor never corroborates,
//!   or two tiles disagreeing at a shared junction — is a conflict. A buffer only widens the overlap
//!   two tiles share, and the whole overlap must match; any *disagreement* there surfaces here too,
//!   because the differing vertex lands in some tile's core. Only a pure *omission* of redundant
//!   overlap geometry goes unflagged — and that is harmless, since the core tile still supplies the
//!   edge and duplicates collapse on reassembly. So the mosaic needs only the `extent`, never the
//!   buffer.
//!
//! [`iter_features`](Mosaic::iter_features) re-chains the distinct edges into maximal polylines
//! (`stitch`) — the same reconstruction a from-scratch merge of all tiles would produce.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use geo_types::Coord;

use crate::TileError;
use crate::tile::{TileId, tile_of};
use crate::vertex::Vertex;

/// A global directed edge's two vertices (identical across every tile that carries it) and the set of
/// tiles that do.
#[derive(Debug, Clone)]
struct Edge<V> {
    a: V,
    b: V,
    tiles: BTreeSet<TileId>,
}

/// A directed edge keyed by its endpoint positions.
type EdgeKey = (Coord<i32>, Coord<i32>);

/// Reassembles tiled features back into whole features as tiles are [added](Self::add).
///
/// Generic over the [`Vertex`] type `V` (defaults to [`Coord<i32>`]). Needs only the `extent` the
/// tiles were sliced at — never the buffer (see the module docs). A payload-carrying vertex (e.g.
/// [`Measured`](crate::Measured)) additionally makes payload [`Conflict`](TileError::Conflict)s
/// meaningful.
#[derive(Debug, Clone)]
pub struct Mosaic<V: Vertex = Coord<i32>> {
    extent: u32,
    /// Global directed-edge index, keyed by endpoint positions. Every tile at a key holds the same
    /// vertices (conflicts are rejected on `add`).
    edges: HashMap<EdgeKey, Edge<V>>,
    /// The edge keys each tile contributed, for `O(tile)` purge and self-exclusion when re-adding.
    tile_edges: BTreeMap<TileId, Vec<EdgeKey>>,
}

impl<V: Vertex> Mosaic<V> {
    /// Create an empty mosaic for tiles sliced at `extent`.
    ///
    /// # Errors
    ///
    /// [`TileError::InvalidExtent`] if `extent` is `0` or greater than `i32::MAX`.
    pub fn new(extent: u32) -> Result<Self, TileError> {
        if extent == 0 || extent > i32::MAX.cast_unsigned() {
            return Err(TileError::InvalidExtent);
        }
        Ok(Self {
            extent,
            edges: HashMap::new(),
            tile_edges: BTreeMap::new(),
        })
    }

    /// The extent every tile is sliced at.
    #[must_use]
    pub fn extent(&self) -> u32 {
        self.extent
    }

    /// Add `tile`'s runs (in its local frame) to the mosaic. `runs` is anything sliceable to `[V]`
    /// (e.g. `&[&[V]]` collected from a tile's features). Re-adding a tile replaces its contents. A
    /// tile with no ≥2-vertex run contributes nothing.
    ///
    /// # Errors
    ///
    /// - [`TileError::Conflict`] if the tile disagrees with already-added tiles on a shared border
    ///   segment. The mosaic is unchanged.
    /// - [`TileError::Overflow`] if a vertex overflows `i32` when rebased into the global frame.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn add<L: AsRef<[V]>>(&mut self, tile: TileId, runs: &[L]) -> Result<(), TileError> {
        // `tile · extent` fits `i64`: both factors are within `i32`, so the product is below `2^62`.
        let e = i64::from(self.extent);
        let off_x = i64::from(tile.x) * e;
        let off_y = i64::from(tile.y) * e;
        // Rebase to the global frame and collect this tile's distinct directed edges. Nothing is
        // mutated yet, so an overflow (or a later conflict) leaves the mosaic untouched.
        let mut new_edges: HashMap<EdgeKey, (V, V)> = HashMap::new();
        for run in runs {
            let g = rebase(run.as_ref(), off_x, off_y)?;
            for w in g.windows(2) {
                let key = (w[0].position(), w[1].position());
                if key.0 != key.1 {
                    new_edges.entry(key).or_insert((w[0], w[1]));
                }
            }
        }
        // Conflict scan against *other* tiles (this tile's own prior data is replaced, not compared).
        let mut conflicts = BTreeSet::new();
        // (a) Payload conflict: another tile carries the same edge (identical positions) but different
        // vertices — only a payload (e.g. an M value) can differ, since the key is the positions.
        for (key, (a, b)) in &new_edges {
            if let Some(existing) = self.edges.get(key)
                && !(existing.a == *a && existing.b == *b)
            {
                conflicts.extend(existing.tiles.iter().copied().filter(|&t| t != tile));
            }
        }
        // (b) Membership conflict: a tile is complete in its own core, so every edge with an endpoint
        // in a present tile's core must be carried by that tile. A present endpoint-tile that lacks the
        // edge means the tiles came from inconsistent data. Checked both ways so detection is
        // order-independent. (Any *disagreement* in a buffer overlap surfaces here too — the differing
        // vertex lands in some core — so only a harmless omission of a redundant overlap edge slips.)
        let ext = self.extent.cast_signed();
        // This tile must carry every already-present edge that names it as an endpoint-tile.
        for (key, existing) in &self.edges {
            if (tile_of(key.0, ext) == tile || tile_of(key.1, ext) == tile)
                && !new_edges.contains_key(key)
            {
                conflicts.extend(existing.tiles.iter().copied().filter(|&t| t != tile));
            }
        }
        // Every already-present endpoint-tile of one of this tile's edges must carry that edge.
        for key in new_edges.keys() {
            for endpoint in [tile_of(key.0, ext), tile_of(key.1, ext)] {
                if endpoint != tile
                    && self.tile_edges.contains_key(&endpoint)
                    && !self
                        .edges
                        .get(key)
                        .is_some_and(|e| e.tiles.contains(&endpoint))
                {
                    conflicts.insert(endpoint);
                }
            }
        }
        if !conflicts.is_empty() {
            return Err(TileError::Conflict(conflicts.into_iter().collect()));
        }
        // Commit: replace any prior contents for this tile, then index the new edges.
        self.purge(tile);
        let mut keys = Vec::with_capacity(new_edges.len());
        for (key, (a, b)) in new_edges {
            self.edges
                .entry(key)
                .or_insert_with(|| Edge {
                    a,
                    b,
                    tiles: BTreeSet::new(),
                })
                .tiles
                .insert(tile);
            keys.push(key);
        }
        if !keys.is_empty() {
            self.tile_edges.insert(tile, keys);
        }
        Ok(())
    }

    /// Remove `tile` and all geometry it contributed. Returns whether the tile was present.
    pub fn purge(&mut self, tile: TileId) -> bool {
        let Some(keys) = self.tile_edges.remove(&tile) else {
            return false;
        };
        for key in keys {
            if let Some(edge) = self.edges.get_mut(&key) {
                edge.tiles.remove(&tile);
                if edge.tiles.is_empty() {
                    self.edges.remove(&key);
                }
            }
        }
        true
    }

    /// Iterate every reassembled feature — geometry stitched across tile borders into maximal
    /// polylines, in the global frame. A single iterator over the whole mosaic; deterministic order.
    pub fn iter_features(&self) -> impl Iterator<Item = Vec<V>> {
        let mut ordered: Vec<&Edge<V>> = self.edges.values().collect();
        ordered.sort_unstable_by_key(|edge| {
            let (a, b) = (edge.a.position(), edge.b.position());
            (a.x, a.y, b.x, b.y)
        });
        let runs: Vec<Vec<V>> = ordered.iter().map(|edge| vec![edge.a, edge.b]).collect();
        stitch(&runs).into_iter()
    }

    /// Whether `tile` is currently in the mosaic.
    #[must_use]
    pub fn contains(&self, tile: TileId) -> bool {
        self.tile_edges.contains_key(&tile)
    }

    /// The number of tiles currently in the mosaic.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tile_edges.len()
    }

    /// Whether no tile has been added yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tile_edges.is_empty()
    }

    /// Discard every tile, keeping the extent.
    pub fn clear(&mut self) {
        self.edges.clear();
        self.tile_edges.clear();
    }
}

/// Rebase every vertex of `run` by `(off_x, off_y)` in `i64` (the offset is `tile · extent`, below
/// `2^62`, and a vertex adds at most another `i32`, so the sum stays within `i64`), range-checking
/// each result back into `i32`.
fn rebase<V: Vertex>(run: &[V], off_x: i64, off_y: i64) -> Result<Vec<V>, TileError> {
    run.iter()
        .map(|&v| {
            let p = v.position();
            Ok(v.with_position(Coord {
                x: i32::try_from(i64::from(p.x) + off_x).map_err(|_| TileError::Overflow)?,
                y: i32::try_from(i64::from(p.y) + off_y).map_err(|_| TileError::Overflow)?,
            }))
        })
        .collect()
}

/// Rejoin overlapping runs into maximal polylines via their **directed edges** (consecutive vertex
/// pairs, keyed by position): a border-crossing segment is the same edge in both tiles, so keeping the
/// distinct edges drops every duplicate, and following each vertex to its outgoing edge re-chains them.
///
/// Order-independent and deterministic (edges/positions keep first-seen order). A simple polyline
/// reconstructs exactly; where the geometry revisits a position, some covering chain is produced
/// (deterministic but arbitrary).
fn stitch<V: Vertex>(runs: &[Vec<V>]) -> Vec<Vec<V>> {
    // Distinct directed edges, in first-seen order (dedup by endpoint positions).
    let mut seen: HashSet<(Coord<i32>, Coord<i32>)> = HashSet::new();
    let mut edges: Vec<(V, V)> = Vec::new();
    for run in runs {
        for w in run.windows(2) {
            if seen.insert((w[0].position(), w[1].position())) {
                edges.push((w[0], w[1]));
            }
        }
    }
    if edges.is_empty() {
        return Vec::new();
    }

    // Adjacency: each start position → its outgoing edge indices (FIFO = first-seen). Track in-degree
    // and first-seen position order so chain starts are found deterministically.
    let mut out: HashMap<Coord<i32>, VecDeque<usize>> = HashMap::new();
    let mut indeg: HashMap<Coord<i32>, usize> = HashMap::new();
    let mut points: Vec<Coord<i32>> = Vec::new();
    let mut known: HashSet<Coord<i32>> = HashSet::new();
    for (i, (p, q)) in edges.iter().enumerate() {
        let (pp, qp) = (p.position(), q.position());
        out.entry(pp).or_default().push_back(i);
        *indeg.entry(qp).or_insert(0) += 1;
        indeg.entry(pp).or_insert(0);
        for pt in [pp, qp] {
            if known.insert(pt) {
                points.push(pt);
            }
        }
    }

    let mut used = vec![false; edges.len()];
    let mut chains: Vec<Vec<V>> = Vec::new();

    // Phase 1: start a chain at each path source — a position with more outgoing than incoming edges
    // accounts for `outdeg − indeg` chain starts (the endpoints of open polylines).
    for &p in &points {
        let outdeg = out.get(&p).map_or(0, VecDeque::len);
        let ind = indeg.get(&p).copied().unwrap_or(0);
        for _ in ind..outdeg {
            if let Some(first) = next_unused(&mut out, p, &used) {
                chains.push(build_chain(first, &edges, &mut used, &mut out));
            }
        }
    }
    // Phase 2: whatever edges remain form closed loops; start each at its first unused edge.
    for i in 0..edges.len() {
        if !used[i] {
            chains.push(build_chain(i, &edges, &mut used, &mut out));
        }
    }
    chains
}

/// Pop and return the next not-yet-used outgoing edge index from position `p`, discarding used ones
/// from the front. `None` when `p` has no remaining unused outgoing edge. Used by [`stitch`].
fn next_unused(
    out: &mut HashMap<Coord<i32>, VecDeque<usize>>,
    p: Coord<i32>,
    used: &[bool],
) -> Option<usize> {
    let dq = out.get_mut(&p)?;
    while let Some(&i) = dq.front() {
        if used[i] {
            dq.pop_front();
        } else {
            return dq.pop_front();
        }
    }
    None
}

/// Follow outgoing edges from edge `first`, consuming each, until a position has no unused outgoing
/// edge; return the vertex chain `[start, …]`. Used by [`stitch`].
fn build_chain<V: Vertex>(
    first: usize,
    edges: &[(V, V)],
    used: &mut [bool],
    out: &mut HashMap<Coord<i32>, VecDeque<usize>>,
) -> Vec<V> {
    let mut chain = vec![edges[first].0];
    let mut cur = first;
    loop {
        used[cur] = true;
        let q = edges[cur].1;
        chain.push(q);
        match next_unused(out, q.position(), used) {
            Some(next) => cur = next,
            None => break,
        }
    }
    chain
}
