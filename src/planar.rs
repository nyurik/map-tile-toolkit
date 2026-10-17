//! Whether a tile's piece is **planar** where it matters: its segments with an endpoint outside the
//! tile's buffered box (the spokes of the edges that cross out, and the connectors that close the
//! rings) neither cross nor touch one another. Segments inside the box are the input's own edges,
//! so a valid input keeps them valid; and every connector lies outside the box, so it can't meet
//! them. That makes the check proportional to the number of excursions, not to the piece's size.
//!
//! Rings may meet at a single shared vertex (OGC allows a hole touching its exterior, or another
//! hole, at one point), but not at two, and no ring may touch itself.

use core::cmp::Ordering;

use geo_types::Coord;

use crate::geom::orient;
use crate::vertex::Vertex;

/// One outside segment of a piece, with where it sits in its ring.
#[derive(Clone, Copy)]
pub(crate) struct Seg {
    a: Coord<i32>,
    b: Coord<i32>,
    ring: u32,
    /// The segment's index among the ring's `edges` edges.
    edge: u32,
    edges: u32,
    /// Whether this segment is a connector or fill-box side, not an input edge.
    synthetic: bool,
}

/// Reusable working memory of [`is_planar`]: it holds nothing between calls, only its capacity.
pub(crate) struct PlanarScratch {
    segs: Vec<Seg>,
    /// The indices of the synthetic segments among `segs`.
    synthetic: Vec<u32>,
    /// Distinct shared vertices found so far between two different rings.
    shared: Vec<(u32, u32, Coord<i32>)>,
}

impl PlanarScratch {
    pub(crate) const fn new() -> Self {
        Self {
            segs: Vec::new(),
            synthetic: Vec::new(),
            shared: Vec::new(),
        }
    }
}

/// How two segments meet.
enum Meet {
    Apart,
    /// Exactly at one endpoint they share.
    Vertex(Coord<i32>),
    /// Crossing, an endpoint inside the other, or overlapping along a line.
    Bad,
}

fn sign(o: Ordering) -> i8 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `p` lies on segment `a`–`b`, given that it is collinear with it.
fn within(p: Coord<i32>, a: Coord<i32>, b: Coord<i32>) -> bool {
    p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)
}

fn meet(p1: Coord<i32>, p2: Coord<i32>, q1: Coord<i32>, q2: Coord<i32>) -> Meet {
    if p1.x.max(p2.x) < q1.x.min(q2.x)
        || q1.x.max(q2.x) < p1.x.min(p2.x)
        || p1.y.max(p2.y) < q1.y.min(q2.y)
        || q1.y.max(q2.y) < p1.y.min(p2.y)
    {
        return Meet::Apart;
    }
    let (s1, s2) = (sign(orient(p1, p2, q1)), sign(orient(p1, p2, q2)));
    let (s3, s4) = (sign(orient(q1, q2, p1)), sign(orient(q1, q2, p2)));
    if s1 * s2 < 0 && s3 * s4 < 0 {
        return Meet::Bad;
    }
    if s1 == 0 && s2 == 0 {
        // Collinear: compare the segments' extents along their common line.
        let key = |c: Coord<i32>| if p1.x == p2.x { c.y } else { c.x };
        let (plo, phi) = (key(p1).min(key(p2)), key(p1).max(key(p2)));
        let (qlo, qhi) = (key(q1).min(key(q2)), key(q1).max(key(q2)));
        let (lo, hi) = (plo.max(qlo), phi.min(qhi));
        return match lo.cmp(&hi) {
            Ordering::Less => Meet::Bad,
            Ordering::Greater => Meet::Apart,
            Ordering::Equal => {
                // A single common point: always an endpoint of both.
                let at = [p1, p2].into_iter().find(|&c| key(c) == lo).unwrap_or(p1);
                Meet::Vertex(at)
            }
        };
    }
    // At most one common point; each zero orientation puts an endpoint on the other line.
    let mut touch = None;
    for (z, c, a, b) in [
        (s1, q1, p1, p2),
        (s2, q2, p1, p2),
        (s3, p1, q1, q2),
        (s4, p2, q1, q2),
    ] {
        if z == 0 && within(c, a, b) {
            touch = Some(c);
        }
    }
    match touch {
        None => Meet::Apart,
        Some(c) if (c == p1 || c == p2) && (c == q1 || c == q2) => Meet::Vertex(c),
        Some(_) => Meet::Bad,
    }
}

/// What [`is_planar`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No segments meet but where they should.
    Planar,
    /// Some segments cross, overlap or touch.
    Crossing,
    /// Checking would cost more than the budget has left: nothing is known.
    OverBudget,
}

/// The most segment pairs a tile's checks may test in all, repairs included. Real pieces need a few
/// hundred; a garbage input (self-intersecting rings of thousands of spikes) must not cost a tile more.
pub(crate) const PAIR_BUDGET: usize = 1 << 16;

/// Up to this many vertices, a single ring is checked without collecting its segments.
const SMALL: usize = 24;

/// Whether the piece in `verts` (rings closed, ending at `ring_ends`) is planar outside the box
/// `[min, max]`. With `connectors_only`, only the synthetic segments are checked against one another:
/// for a frame, whose connectors lie outside the hull of everything else, and so can't meet it. The
/// pairs tested are taken off `budget`, and the check gives up when it would not cover them.
#[expect(
    clippy::too_many_arguments,
    reason = "the piece, what is synthetic in it, the box, and the check's scratch and budget"
)]
pub(crate) fn is_planar<V: Vertex>(
    verts: &[V],
    ring_ends: &[u32],
    synthetic: &[u32],
    connectors_only: bool,
    min: Coord<i32>,
    max: Coord<i32>,
    scratch: &mut PlanarScratch,
    budget: &mut usize,
) -> Verdict {
    if let [end] = *ring_ends
        && !connectors_only
        && end as usize <= SMALL + 1
    {
        return small_ring(&verts[..end as usize], synthetic, min, max);
    }
    let PlanarScratch {
        segs,
        synthetic: syn,
        shared,
    } = scratch;
    segs.clear();
    syn.clear();
    shared.clear();
    let outside = |c: Coord<i32>| c.x < min.x || c.x > max.x || c.y < min.y || c.y > max.y;
    let mut start = 0;
    for (ring, &end) in (0..).zip(ring_ends) {
        let end = end as usize;
        let ring_verts = &verts[start..end];
        let edges = u32::try_from(ring_verts.len().saturating_sub(1)).unwrap_or(u32::MAX);
        for (edge, w) in (0..).zip(ring_verts.windows(2)) {
            let (a, b) = (w[0].position(), w[1].position());
            if a != b && (outside(a) || outside(b)) {
                let at = u32::try_from(start)
                    .unwrap_or(u32::MAX)
                    .saturating_add(edge);
                let synthetic = synthetic.binary_search(&at).is_ok();
                if synthetic {
                    syn.push(u32::try_from(segs.len()).unwrap_or(u32::MAX));
                }
                segs.push(Seg {
                    a,
                    b,
                    ring,
                    edge,
                    edges,
                    synthetic,
                });
            }
        }
        start = end;
    }
    // Only the fill box of a covered tile (a ring of nothing but synthetic edges) lies outside the
    // box: with no other outside segment, it has nothing to meet.
    if segs.len() == syn.len() && segs.iter().all(|s| s.ring == 0 && s.edges == 4) {
        return Verdict::Planar;
    }
    let pairs = syn.len().saturating_mul(segs.len());
    if pairs > *budget {
        *budget = 0;
        return Verdict::OverBudget;
    }
    *budget -= pairs;
    // Input edges never meet one another improperly (the input is valid), so every pair with at
    // least one synthetic segment is checked, once.
    for &i in syn.iter() {
        let p = segs[i as usize];
        for (j, q) in segs.iter().enumerate() {
            if j == i as usize
                || (q.synthetic && j < i as usize)
                || (connectors_only && !q.synthetic)
            {
                continue;
            }
            let adjacent = p.ring == q.ring
                && ((p.edge + 1) % p.edges == q.edge || (q.edge + 1) % q.edges == p.edge);
            match meet(p.a, p.b, q.a, q.b) {
                Meet::Apart => {}
                Meet::Bad => return Verdict::Crossing,
                Meet::Vertex(c) => {
                    if p.ring == q.ring {
                        if !adjacent {
                            return Verdict::Crossing;
                        }
                    } else {
                        let key = (p.ring.min(q.ring), p.ring.max(q.ring), c);
                        if !shared.contains(&key) {
                            if shared.iter().any(|s| s.0 == key.0 && s.1 == key.1) {
                                return Verdict::Crossing;
                            }
                            shared.push(key);
                        }
                    }
                }
            }
        }
    }
    Verdict::Planar
}

/// [`is_planar`] for one ring of at most [`SMALL`] vertices (closed, in `verts`), tested in place: a
/// small ring, a building across a tile edge say, costs a few segment tests and no bookkeeping.
#[expect(
    clippy::many_single_char_names,
    reason = "a, b and c, d are the two segments' endpoints, as in `meet`"
)]
fn small_ring<V: Vertex>(
    verts: &[V],
    synthetic: &[u32],
    min: Coord<i32>,
    max: Coord<i32>,
) -> Verdict {
    let outside = |c: Coord<i32>| c.x < min.x || c.x > max.x || c.y < min.y || c.y > max.y;
    let n = verts.len().saturating_sub(1);
    let edge = |i: usize| (verts[i].position(), verts[i + 1].position());
    let synthetic_at =
        |i: usize| u32::try_from(i).is_ok_and(|i| synthetic.binary_search(&i).is_ok());
    for &i in synthetic {
        let i = i as usize;
        let (a, b) = edge(i);
        if a == b {
            continue;
        }
        for j in 0..n {
            if j == i || (j < i && synthetic_at(j)) {
                continue;
            }
            let (c, d) = edge(j);
            if c == d || !(outside(c) || outside(d)) {
                continue;
            }
            match meet(a, b, c, d) {
                Meet::Apart => {}
                Meet::Bad => return Verdict::Crossing,
                // One ring may touch itself only where consecutive edges meet.
                Meet::Vertex(_) => {
                    if (i + 1) % n != j && (j + 1) % n != i {
                        return Verdict::Crossing;
                    }
                }
            }
        }
    }
    Verdict::Planar
}

#[cfg(test)]
mod tests {
    use geo_types::Coord;

    use super::*;

    fn c(x: i32, y: i32) -> Coord<i32> {
        Coord { x, y }
    }

    fn meets(a: (i32, i32), b: (i32, i32), p: (i32, i32), q: (i32, i32)) -> Meet {
        meet(c(a.0, a.1), c(b.0, b.1), c(p.0, p.1), c(q.0, q.1))
    }

    #[test]
    fn segments_meet_in_every_way() {
        // Crossing, and an endpoint on the other's interior, are bad.
        assert!(matches!(meets((0, 0), (4, 4), (0, 4), (4, 0)), Meet::Bad));
        assert!(matches!(meets((0, 0), (4, 0), (2, 0), (2, 3)), Meet::Bad));
        // Only a shared endpoint is a vertex touch, collinear or not.
        assert!(matches!(meets((0, 0), (4, 0), (4, 0), (4, 3)), Meet::Vertex(v) if v == c(4, 0)));
        assert!(matches!(meets((0, 0), (4, 0), (4, 0), (9, 0)), Meet::Vertex(v) if v == c(4, 0)));
        // Collinear overlap is bad; a gap, or bounding boxes apart, is not a meeting.
        assert!(matches!(meets((0, 0), (4, 0), (2, 0), (9, 0)), Meet::Bad));
        assert!(matches!(meets((0, 0), (4, 0), (5, 0), (9, 0)), Meet::Apart));
        assert!(matches!(meets((0, 0), (4, 0), (0, 1), (4, 1)), Meet::Apart));
        assert!(matches!(meets((0, 0), (1, 1), (5, 5), (6, 6)), Meet::Apart));
    }

    /// A closed ring of `pts` (its first vertex repeated) and the edges marked synthetic.
    fn planar(pts: &[(i32, i32)], synthetic: &[u32], only_connectors: bool) -> bool {
        let mut verts: Vec<_> = pts.iter().map(|&(x, y)| c(x, y)).collect();
        verts.push(verts[0]);
        let end = u32::try_from(verts.len()).unwrap();
        let mut scratch = PlanarScratch::new();
        // The box is the single point (0, 0): every other vertex is outside.
        let mut budget = PAIR_BUDGET;
        is_planar(
            &verts,
            &[end],
            synthetic,
            only_connectors,
            c(0, 0),
            c(0, 0),
            &mut scratch,
            &mut budget,
        ) != Verdict::Crossing
    }

    #[test]
    fn only_edges_with_a_synthetic_side_are_checked() {
        let bowtie = [(1, 1), (5, 5), (5, 1), (1, 5)];
        // Nothing synthetic: the input's own edges are trusted, even when they cross.
        assert!(planar(&bowtie, &[], false));
        // A synthetic edge crossing an input edge is not planar.
        assert!(!planar(&bowtie, &[0], false));
        // Frames check connectors against each other only.
        assert!(planar(&bowtie, &[0], true));
        let square = [(1, 1), (5, 1), (5, 5), (1, 5)];
        assert!(planar(&square, &[0, 1, 2, 3], false));
    }
}
