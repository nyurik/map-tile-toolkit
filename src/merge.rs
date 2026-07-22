use std::collections::{HashMap, HashSet, VecDeque};

use geo_types::{Coord, Geometry};

use crate::clip_polyline::{assemble, each_line};
use crate::vertex::Vertex;
use crate::{Error, Slicer, TileId};

impl Slicer {
    /// Reconstruct the combined geometry of two tiles from their tile-local pieces — the inverse of
    /// slicing. The tiles need not be adjacent, so this folds: merge two tiles, then merge the
    /// result (treated as a piece at the lower-left tile) with a third, and so on.
    ///
    /// `a` and `b` are `(tile, piece)` pairs as produced by [`slice`](Self::slice) /
    /// [`slice_all`](Self::slice_all), each `piece` in its own tile-local frame. The result is
    /// expressed in a **shared tile-local frame** anchored at the lower-left of the two tiles (its
    /// origin is the component-wise-minimum tile's `[0, 0]` corner), so adding that origin
    /// (`min(a.tile, b.tile) · divider`) recovers global coordinates. Fold with that min tile as the
    /// running anchor.
    ///
    /// Because slicing keeps original vertices, a segment crossing a shared border is present in
    /// *both* tiles. Merging rebases both pieces into the shared frame, collects their **distinct**
    /// directed edges (so every duplicated border segment collapses to one), and re-chains those
    /// edges into maximal runs. Parts that don't (yet) connect stay separate lines. Returns `None`
    /// only if neither piece has a ≥2-vertex run.
    ///
    /// # Errors
    ///
    /// - [`Error::UnsupportedGeometry`] — either piece is not a polyline.
    /// - [`Error::Overflow`] — rebasing a coordinate into the shared frame overflows `i32` (the two
    ///   tiles lie too far apart to share one `i32` local frame).
    pub fn merge(
        self,
        a: (TileId, &Geometry<i32>),
        b: (TileId, &Geometry<i32>),
    ) -> Result<Option<Geometry<i32>>, Error> {
        let (ta, ga) = a;
        let (tb, gb) = b;
        let (la, lb) = (each_line(ga)?, each_line(gb)?);
        let ra: Vec<&[Coord<i32>]> = la.iter().map(|l| l.0.as_slice()).collect();
        let rb: Vec<&[Coord<i32>]> = lb.iter().map(|l| l.0.as_slice()).collect();
        Ok(assemble(self.merge_lines((ta, &ra), (tb, &rb))?))
    }

    /// Merge two tiles' generic [`Vertex`] pieces; the analogue of [`merge`](Self::merge) for
    /// vertices carrying a payload (e.g. an M value).
    ///
    /// Each piece is that tile's runs (`&[Vec<V>]`, `&[&[V]]`, …). Returns the reconstructed runs in
    /// the shared (lower-left-anchored) frame; empty if neither piece has a ≥2-vertex run. Payloads
    /// ride through unchanged; the shared crossing segment is de-duplicated by position, so both
    /// copies must carry the same payload (they are the same original vertex).
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if rebasing a coordinate into the shared frame overflows `i32`.
    pub fn merge_lines<V: Vertex, L: AsRef<[V]>>(
        self,
        a: (TileId, &[L]),
        b: (TileId, &[L]),
    ) -> Result<Vec<Vec<V>>, Error> {
        let (ta, runs_a) = a;
        let (tb, runs_b) = b;
        // Shared frame anchored at the lower-left tile.
        let (sx, sy) = (ta.x.min(tb.x), ta.y.min(tb.y));
        let mut runs: Vec<Vec<V>> = Vec::new();
        for (tile, tile_runs) in [(ta, runs_a), (tb, runs_b)] {
            // Offset from this tile's local frame into the shared frame: `(tile − shared) · divider`.
            // Done in i128 so distant tiles (allowed) can't overflow the offset itself — only the
            // final per-vertex position is range-checked back into `i32`.
            let d = i128::from(self.divider);
            let off_x = (i128::from(tile.x) - i128::from(sx)) * d;
            let off_y = (i128::from(tile.y) - i128::from(sy)) * d;
            for run in tile_runs {
                let run = run.as_ref();
                if run.len() < 2 {
                    continue; // a <2-vertex run cannot stitch and is not a valid piece
                }
                let mut rebased = Vec::with_capacity(run.len());
                for &v in run {
                    let p = v.position();
                    rebased.push(v.with_position(Coord {
                        x: i32::try_from(i128::from(p.x) + off_x).map_err(|_| Error::Overflow)?,
                        y: i32::try_from(i128::from(p.y) + off_y).map_err(|_| Error::Overflow)?,
                    }));
                }
                runs.push(rebased);
            }
        }
        Ok(Self::stitch(&runs))
    }

    /// Rejoin overlapping runs into maximal polylines by working on their **directed edges**
    /// (consecutive vertex pairs, keyed by position). A segment crossing a shared border is the
    /// *same* directed edge in both tiles, so collecting the **distinct** edges drops every such
    /// duplicate — whether the overlap is a shared endpoint, a shared segment, or one tile's whole
    /// run sitting inside the other's. The distinct edges are then chained back into maximal runs by
    /// following each vertex to its outgoing edge.
    ///
    /// Order-independent and deterministic: edges and positions keep first-seen order, and outgoing
    /// edges are followed first-seen first. For a simple (non-self-touching) polyline every interior
    /// position has one in- and one out-edge, so the reconstruction is exact; where the geometry
    /// genuinely revisits a position, any covering chain is produced (deterministic but arbitrary).
    /// Every input run has ≥2 vertices, so each contributes at least one edge.
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

        // Adjacency: each start position → its outgoing edge indices (FIFO = first-seen). Track
        // in-degree and first-seen position order so chain starts are found deterministically.
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

        // Phase 1: start a chain at each path source — a position with more outgoing than incoming
        // edges accounts for `outdeg − indeg` chain starts (the endpoints of open polylines).
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
}

/// Pop and return the next not-yet-used outgoing edge index from position `p`, discarding used ones
/// from the front. `None` when `p` has no remaining unused outgoing edge. Used by [`Slicer::stitch`].
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
/// edge; return the vertex chain `[start, …]`. Used by [`Slicer::stitch`].
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
