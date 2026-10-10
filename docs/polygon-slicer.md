# Polygon slicer — design

Status: **implemented** (all phases of §13). This captures the agreed algorithm for
`PolygonSlicerOne` / `PolygonSlicerAll` and the polygon path through `Mosaic`, so the code can be
reviewed against it; where the implementation refined the plan, the section says so.

## 1. Goal & constraints

Clip integer **polygons** (an exterior ring + zero or more holes) into per-tile pieces on the same
integer tile grid as the polyline slicers, and let `Mosaic` reassemble them. Same house rules as the
rest of the crate: edition 2024, MSRV 1.88, `unsafe_code = "forbid"`, **never panics** (bad input →
`TileError`), integer-only (no floats), and **keep original vertices** — the slicer never cuts a new
vertex at the tile edge.

Two axes carry through unchanged from the polyline slicers:
- **per-vertex payload** `V: Vertex` (default `Coord<i32>`; `Measured<M>` for M-values / ids),
- **per-feature attribute** `A` (default `()`, zero-cost; `add_feature_with`).

### The unavoidable tension

A clipped **polygon** must stay a closed, correctly-wound ring, but a valid clip's closing edge along
the tile boundary *requires* synthetic vertices. You cannot have both "zero new vertices" and "OGC-valid
output". We resolve it the way the crate resolves everything else — **keep every original vertex, and
accept that the output is invalid _only outside the visible/buffer box_**, where a renderer clips it
away. This is legitimate for MVT/raster consumers (nonzero-winding fill + raster clip to tile+buffer);
it is *not* suitable for a consumer that needs OGC-valid geometry.

We chose keep-original (over Sutherland–Hodgman, which is simpler but snaps new vertices onto the tile
boundary) **specifically to make polygon reassembly work** — see §7.

## 2. Coordinate model (unchanged)

Reuse `Grid`'s existing model verbatim: tile `t` owns cell `[t·extent, t·extent + extent − 1]`; the
**buffered box** is

```
B(t) = [base − buffer , base + extent − 1 + buffer]   on each axis,  base = t · extent
```

Clipping is against `B(t)`, exactly like the polyline slicer. `buffer` only enlarges `B`; nothing in
the algorithm is otherwise buffer-aware (same conclusion the polyline/mosaic work reached).

Define the **synthetic shell** `B⁺(t) = B(t)` grown by ≥ 1 unit on every side. *All* synthetic
geometry lives on/beyond `B⁺` — **strictly outside `B`** — which §7 shows is the load-bearing
invariant that lets `Mosaic` re-derive synthetic-ness geometrically with no tag bit.

## 3. Keep-rule (reuse the polyline rule verbatim)

A ring vertex is **kept** iff it is inside `B`, or an incident edge intersects `B` — i.e. exactly the
condition in `clip_polyline::segment_intersects` already used by `Grid::slice_one`/`route`. Everything
else (deep-outside vertices) is dropped. So the "keep the first vertex after crossing the border"
rule is not new code: it *is* the polyline keep-rule, applied to each ring.

Kept originals therefore form the same **arcs** (maximal keep-runs, each ≥ 2 vertices) that
`slice_one` already returns. The only polygon-specific work is **closing** the arcs of one ring back
into one ring by bridging the dropped gaps (§5) instead of emitting them as separate runs.

Key structural fact: **one input ring → one output ring per tile.** Walking a closed ring yields
alternating arcs (near/inside `B`) and gaps (outside); we keep the arcs in cyclic order and bridge
the gaps, producing a single closed ring per tile (self-touching outside `B` is fine). Exterior stays
exterior, hole stays hole. A ring that is entirely dropped simply doesn't appear in that tile.

## 4. Public shape

```
PolygonSlicerOne<V = Coord<i32>, A = ()>   // clips polygons to one fixed tile
PolygonSlicerAll<V = Coord<i32>, A = ()>   // slices multipolygon features into every tile
```

- `add_feature(..)` (only when `A = ()`), `add_feature_with(.., attr)` — mirrors the polyline
  slicers; atomic on error.
- Input: rings as `&[V]` slices (open or closed), so `Measured<M>` works (`geo-types` can't hold a
  payload). `PolygonSlicerOne` takes one polygon, `(exterior, &[holes])`. `PolygonSlicerAll` takes one
  **multipolygon** feature: an iterator of polygons, each an iterator of rings (exterior first), e.g.
  `[[&exterior[..], &hole[..]]]`.
- Read-back: both slicers hand out the same `PolygonView` per clipped polygon — `attr()` and
  `iter_rings()` (`RingView`: `vertices()` closed, tile-local; `is_hole()`). `PolygonSlicerOne::
  iter_features()` yields one per feature that reaches the tile. `PolygonSlicerAll::iter_features()`
  → per feature `attr()`, `iter_tiles()` (edge tiles, row-major) → `iter_polygons()`, and
  `iter_fill_runs()` → `FillRun { y, x: Range<i32>, polygon }` with `fill_ring(&run)` (§6). Views are `Copy`. Feature-major rather than the polyline
  slicers' tile-major order: fill runs belong to one feature and span many tiles, and append-only
  per-feature storage needs no tile index.
- `signed_area_2x(ring) -> i128` (§9) is public: the slicers preserve input winding, so a caller
  needing a fixed convention (MVT: exterior positive in y-down tile coordinates) normalizes once per
  feature.

Storage: flat `u32`-offset arenas (vertices, ring ends, polygon ends, tile entries, fill runs,
feature ends) — a polygon's first ring is its exterior, so the role needs no marker. No per-tile
allocation; `clear()` keeps all capacity, and `PolygonSlicerAll::shrink_to_fit()` releases it after
an outlier feature. **No new runtime dependency.**

## 5. Corner-detour routing (the one genuinely new algorithm)

For each dropped gap between an arc's exit vertex `E` (outside `B`) and the next arc's entry vertex
`S` (outside `B`), insert the **minimal** `B⁺` corners needed to route `E → … → S` around the
*exterior* of `B`, so the connector never enters `B`:

1. **Exit/entry side** — classify `E` and `S` by a Cohen–Sutherland **outcode** vs `B` (which
   edge(s) they're beyond). `E`'s exit side is the `B` edge the segment `[last-inside → E]` crossed;
   symmetric for `S`.
2. **Direction** — walk `∂B⁺` from `E`'s side to `S`'s side **in the input ring's winding
   direction** (compute the ring orientation once, up front, by an i128 shoelace signed area — see
   §9). Orientation picks the correct wrap so the detour preserves the winding number inside `B`;
   the wrong wrap would enclose extra area and flip the fill.
3. **Insert** the `B⁺` corners strictly between the two sides (0–3 of them), then close.

**Non-grazing obligation (correctness-critical, not cosmetic):** every synthetic segment
`E→corner`, `corner→corner`, `corner→S` must stay strictly outside `B`. Sketch: `E` and `S` are
strictly outside `B` in their outcode half-planes; consecutive inserted points are the adjacent `B⁺`
corner on the same side; each connecting segment lies wholly in one `B` edge's outer half-plane →
never meets `B`'s interior. This must be proven/tested, because a detour that clips `B` corrupts
**reassembly**, not just one tile's render (§7).

**Seam handling:** the touching edges are numbered from the ring's first vertex, so a ring whose
closure point is inside `B` has its first and last arcs split at the array seam; `close_ring` joins
them (they're contiguous in the ring) before bridging the remaining gaps.

Same-edge tiny excursion (`E`, `S` beyond the same edge) needs **zero** corners — the chord `E→S`
already stays outside. Two touching edges at most two apart stay one kept arc (the single-segment
out-and-back between them, as in the polyline clip), which needs no detour.

**Winding-matched detours (the wrap case).** A naive "route to the nearest corners" detour is wrong
when the dropped excursion *encircles* the box — e.g. a large C-/donut-shaped ring whose notch dips
into a tile it otherwise surrounds. There the excursion between the two notch crossings wraps `∂B`
fully, and a local detour would reconstruct only the notch area filled (inverted fill). The
exit/entry vertices alone can't tell a short poke-out from a full wrap, so the detour is built to be
**homotopic to the excursion** in `exterior(B)`:

1. Pick a fixed ray `R` from the box center (`+x`). For the dropped excursion polyline `[E … S]`,
   count its **signed crossings** of the part of `R` outside `B` (exact integer, the same
   crossing-number machinery as `point_in_ring`) → `W_exc`.
2. Build the reference corner path (CCW `∂B⁺` arc from `E`'s outcode slot to `S`'s), count its signed
   `R`-crossings → `W_ref`.
3. Add `W_exc − W_ref` whole `∂B⁺` loops (sign chooses CCW/CW) to the detour.

The common case computes `W_exc = W_ref` → zero extra loops → the simple 0–3 corner insertion; the
wrap case adds a loop. Because the reference and excursion share the endpoints `E`/`S` and one
crossing routine, endpoint ambiguities cancel and ring orientation falls out automatically (no
separate orientation needed for the detour; `ring_orientation` is only used to orient a
containment-fill box, §6). Corner slots come from a Cohen–Sutherland outcode of `E`/`S` vs `B`.

## 6. Containment & interior fill

**The "all vertices outside `B`" trichotomy** (the tricky family — a tile fully/partially inside a
polygon whose vertices all sit beyond the tile). Every polygon vertex is outside `B`; classify by
whether any *edge* touches `B`:
1. **An edge crosses `B`** (edge within the box, both endpoints outside) → the keep-rule keeps that
   edge as a 2-vertex arc (a chord through `B`); the §5 winding-matched detour reconstructs which side
   of the chord is filled. This is the normal clip path — no special case.
2. **No edge touches `B`, polygon encloses the tile** → containment fill (below).
3. **No edge touches `B`, tile outside the polygon** → emit nothing.

- **`One` containment:** a ring with **no edge touching `B`** that **encloses** it (`point_in_ring`
  of one `B` corner, §9) → emit the `B⁺` box as an all-synthetic ring, oriented by `ring_orientation`
  (a solid fill for that tile). No edge touching and not enclosing → emit nothing. *This case is
  absent from the naive keep-rule and must be added, or a fully-covered tile renders empty.*
- **`All` interior fill:** `route()` is edge-driven, so tiles that sit **fully inside** a large
  polygon (no ring edge within their `B`) are visited by nothing and would render empty. After
  routing, fill them:
  - A tile not touched by any ring edge has no ring within its `B`, so its whole box is uniformly
    inside or outside the fill — **one point-in-polygon test per tile is decisive.**
  - Do it as a **scanline over tile-rows** (perf, §8), not a test per tile. Each row's
    representative line is its **center line** `y = cy` — the same line every tile in the row casts
    its §5 winding ray along — so one exact integer x-crossing list per row (`crossing_x_ceil`,
    rounded up so "right of tile `tx`'s box" is a plain `tx <= k` test) serves both the fill and the
    detours. Sweeping the row's crossings left to right toggles each ring's parity; between two
    consecutive crossings the containment is constant.
  - **Rule:** a tile is covered when some polygon's exterior contains it and none of its holes do
    (per-ring crossing parity, exactly `PolygonSlicerOne`'s containment test). For valid input with
    normalized winding this is the nonzero-winding rule; unlike a raw winding sum it does not depend
    on the input's ring orientation, and it keeps `All` equal to `One` on every input.
  - **Output as runs, not boxes:** each covered span, minus the row's edge tiles, becomes one
    maximal `FillRun { y, x, polygon }` per covering polygon — never a per-tile `B⁺` box — so memory
    is `O(edge tiles + runs)` and a polygon covering 10⁸ tiles costs one run per row. The run keeps
    its polygon's exterior winding, and `fill_ring(&run)` returns the tile-local `B⁺` box `One` emits
    for that polygon in each of the run's tiles, so expanding the runs (several per tile only for
    overlapping polygons, in polygon order) reproduces `One` exactly; the snapshot tests assert it.
  - **Edge tiles** use the same parity for rings with no edge in the tile: an exterior that contains
    the tile becomes a fill box beside clipped holes, a containing hole drops its polygon, and a
    polygon wholly covering another part's edge tile (only possible for overlapping, invalid
    multipolygons) contributes a fill box there — all exactly as `One` does.
  - Robustness: half-open edge rule (`[y_min, y_max)`) so shared vertices / horizontal edges are
    counted once — this is where §10's "rings may share a vertex" matters. Each ring crosses a line an
    even number of times, so the sweep state returns to empty at every row end (no per-row reset).

## 7. Mosaic reassembly of polygons (no tag bit)

`PolygonMosaic` knows `extent + buffer`, so it re-derives each tile's box `B(tile)` and keeps only the
ring **edges that touch `B`**, dropping the rest:

```
for a cyclic edge p→q in a tile's ring, with box B(tile):
    edge touches B   → keep  (a real ring edge, authoritative here)
    edge misses B    → drop  (synthetic filler, or redundant here — owned by another tile)
```

This is **exact, not heuristic**, and splits cleanly:

- **Completeness.** Every real ring edge `p→q` is kept by the tile that owns `p`'s cell: that cell lies
  in `B` (core ⊆ box), so the edge starts inside `B` and touches it. No real edge is ever lost — it
  survives in at least the one tile that owns its start. (An edge missing `B` here but real elsewhere is
  simply redundant, re-emitted by its owner; dropping it here is harmless.)
- **Soundness.** Every synthetic edge — the `B⁺` corners, the fill boxes, and the gap-bridging detours —
  is routed strictly *outside* `B` (the §5 invariant: fill on `B⁺`, detours never grazing `B`), so it
  never touches `B` and is dropped.

**Why edges, not vertices.** An earlier design dropped synthetic *vertices* (outside `B` with neither
incident edge touching `B`). That is insufficient: a `0`-corner detour bridges a dropped gap with a
**direct chord between two _original_ crossing vertices** (both legitimately kept), so the chord is a
synthetic *edge* with no synthetic *vertex* for a per-vertex test to catch — it would survive, pollute
the edge set, and (having an endpoint in a neighbor's core) raise a false `TileError::Conflict`. Testing
the **edge's** relationship to `B` catches it: the chord lies outside `B`, so it misses and is dropped.
No per-vertex flag either way — `V` stays fully generic.

Keeping only touching edges splits each tile's ring into exactly the **original arcs** a polyline would
have produced, ending at original crossing vertices the neighbor tile shares. So:

> **Polygon reassembly = keep edges touching `B`, then the *existing* polyline `stitch`.**

- Union of the kept original directed edges, deduped, = the original ring edge set → re-chain → closed
  rings, holes and orientation intact.
- Interior-fill boxes and `One` containment fills are 100% synthetic → every edge misses `B` → dropped
  entirely → correct (a reassembled polygon's interior is implied by winding; we don't need the fill
  tiles back).
- The existing core-completeness / payload conflict checks run **on the kept edges only** (filter
  first), so synthetic geometry can raise no false `TileError::Conflict`.

Filtering happens per-tile at `add` time, before the global edge dedup, so a synthetic edge is never
inserted into the global map and coincidences with other tiles' vertices are irrelevant. The rule is
purely tile-local, hence inherently order-independent.

## 8. Performance plan

- **Reuse the streaming engine.** `PolygonSlicerAll` routes each ring's edges through
  `Grid::route_within` + a polygon `RouteSink`, inheriting the inner-box fast path (`Located`), the
  `tile_of` skip, and `Overflow` checks. The sink records `(tile, edge)` hits (sorted row-major
  once); all of a feature's rings share **one** `MAX_TILE_VISITS` budget of touched tiles (plus any
  row an edge crosses without touching a tile, possible only at `buffer == 0`), which
  covers routing only — covered tiles are never charged. A segment leaving the inner box is walked row
  by row through exactly the tiles it touches, so a long diagonal edge costs its touched tiles, not
  its bounding rectangle. Beyond that, the only limits are `u32` indexing, `i32` coordinates and,
  for polygons, the 2^28-vertex output cap per feature (`OutputTooLarge`).
- **Excursion windings without walking excursions.** §5's `W_exc` counts an excursion's crossings of
  the tile's ray; walking the excursion per tile would cost `O(ring)` per tile (the ring around an
  ocean is "the excursion" for every coastal tile). Instead the row's crossing list, in edge order,
  feeds a Fenwick tree of signs; the sweep removes each crossing once it passes it, so the signed
  crossings right of the current tile among edges `i..j` is two prefix sums — `O(log n)` per gap.
- **One closing core.** `close_ring` (arcs from the sorted touching edges, seam join, bridge/detour
  via a winding callback) is shared: `One` supplies touching edges and windings by walking the ring,
  `All` from its hits and Fenwick tree, so both produce identical rings by construction.
- **`PolygonSlicerOne`** finds each ring's touching edges with the same `segment_intersects` test,
  then closes them with the shared core (§5); a ring touching no edge is settled by `point_in_ring`
  (§6).
- **Orientation once per input ring** (i128 shoelace), reused across every tile that ring touches —
  not recomputed per tile.
- **Interior fill is the only super-linear risk**; the scanline (§6) makes it
  `O(crossings · log)` — crossings of row center lines, bounded by the routing budget — instead of
  `O(tiles · ring)`, and only classifies tiles the edge pass didn't already cover. Measured
  (Callgrind, `just bench polygon`): a 4.5k-vertex polygon on a 25-unit grid costs 7.5M instructions
  all-tiles vs 100M through a per-tile `One`; the z14-like `huge_fill` (2^28 tiles) costs ~2.4k
  instructions per edge tile and nothing per covered tile.
- Everything integer/i128 — exact and branch-cheap, no float predicates.
- Add polygon cases to `examples/profile.rs` + `benches` so `just bench`/`just hotpath` cover them,
  mirroring the polyline coverage.

## 9. Shared integer-geometry primitives (avoid duplication)

The i128 cross-product "side" test is currently inline in `clip_polyline::segment_intersects`. Extract
it into one small pure helper and build the three new predicates on it, so there is a single exact
orientation primitive in the crate:

- `orientation(a, b, c) -> i128 sign`  (the existing side test, factored out)
- `segment_intersects` (rewritten on top of it — behavior unchanged)
- `signed_area_2x(ring) -> i128`  (shoelace, i128 accumulation to avoid i32/i64 overflow) → ring
  winding for §5/§6 fill boxes; public, so callers can normalize winding
- `point_in_ring(p, ring) -> inside/boundary`  (exact integer winding) → containment §6 and the
  scanline §6

**Math width (i64 fast path, i128 fallback).** A 2-D cross product of full-`i32` points can reach
2⁶⁵, so *exactness across the whole `i32` range needs 128-bit somewhere*. Following `i_overlay`/
`i_float` (which map `i32 → Wide = i64` and cross-multiply the edge *difference* vectors, relying on
a bounded coordinate domain), `orient` evaluates the cross in `i64` on the difference vectors and
only widens to `i128` when a difference does not fit `i32` (the rare near-full-`i32`-span case). So
ordinary tile-local geometry never pays the 128-bit width, while the crate keeps its full-`i32`
contract (unlike `i_overlay`, which would require capping the coordinate range). The predicates are
built on this one primitive and never panic.

## 10. Validity (OGC Simple Features)

Validity is defined by the **OGC Simple Features** rules, not an informal vertex/edge heuristic. For a
`Polygon`: each ring is closed and **simple** (no self-intersection bar the closing point); the shell
and each hole may intersect only at a **finite set of points** (they may *touch* at points but never
along a segment, and never cross); holes lie inside the shell and don't overlap (touch at points
only); and the polygon **interior is connected** (holes can't pinch it into disconnected parts). This
is a superset of "shared vertex OK, edge crossing not" and is what the docs/comments cite.

The slicer **accepts any input without panicking** (MVT itself permits self-intersections under the
nonzero-winding fill rule); it only *guarantees* the properties below for OGC-valid input.
Consequences:

- Keep-original **preserves shared vertices verbatim** in every tile that contains them → reassembly
  restores them → the shared-vertex relationship survives a round trip. No special handling needed.
- **Inside `B` we introduce no new edges** (only original vertices are kept there), so we can never
  manufacture an edge crossing within the visible/buffer area; any inner/outer touching relationship
  inside `B` is preserved exactly.
- Synthetic detours live strictly outside `B`; two detours *can* cross each other out there, but that
  artifact is clipped away by the renderer and dropped on reassembly, so it never affects validity of
  the visible geometry.
- The scanline (§6) and `point_in_ring` (§9) must use exact integer predicates + a consistent
  on-boundary/half-open rule so shared vertices and vertices-on-the-scanline are counted once.

## 11. Dependencies & off-the-shelf reuse

**Recommendation: no new runtime dependency; do not pull `geo` into the library.**

- **Reuse `geo-types`** (already a runtime dep) for the `Coord<i32>` polygon input/output types.
- **Reuse in-crate machinery**: `Grid`, `route`/`slice_one`, `RouteSink`, `Located`, `to_local`,
  `segment_intersects`, `TileError`, the flat `TileBuf` storage, and the existing `Mosaic` stitch.
- **Why not `geo`'s algorithms in the shipping crate:** `geo`'s relational/clipping ops target
  `GeoFloat` or use kernels that can overflow for raw `i32` cross products, and don't carry this
  crate's never-panic / `unsafe`-forbid / integer-exact guarantees. Its `BooleanOps` (via
  `i_overlay`) *would* clip polygons robustly and fast — but it produces **boundary-snapped new
  vertices**, which is exactly what breaks the keep-original reassembility we're building for. So the
  off-the-shelf clipper is a poor fit for the core here (it's what you'd use if reassembly weren't a
  goal).
- **Do use `geo` as a test oracle** (it's already a `dev-dependency`): in tests, compare our
  reassembled polygon (and/or per-tile winding) against `geo`'s boolean intersection of the input
  with the tile box, as an independent correctness check. Off-the-shelf leverage where it can't
  compromise the shipped crate.

## 12. Test plan — new fixture dir + binary snapshots

Follow the existing data-driven pattern exactly (`test_each_path!` over a fixture dir, byte-exact
`assert_binary_snapshot!` `.snap.geojson`, both buffer sizes, plus a mosaic reassembly snapshot). All
cases are authored as **GeoJSON** in a **new fixture directory** so they render in QGIS/geojson.io.

- **`tests/polygons/fixtures/`** — good input polygons (each a GeoJSON `Polygon` or `MultiPolygon`,
  some with holes; a `MultiPolygon` is one `PolygonSlicerAll` feature):
  - fully inside one tile; spanning 2 and 4 tiles; a hole fully inside a tile; a hole crossing a tile
    edge; a polygon large enough to have **fully-interior tiles** (exercises §6 fill); a ring that
    pokes out and back on one edge (zero-corner detour); excursions that wrap 1/2/3 corners
    (1–3-corner detours); a polygon that **encloses** a whole tile (containment §6); an inner ring
    that **shares a vertex** with the outer ring (§10); concave/self-adjacent shapes.
  - Snapshots mirror the polyline layout: `tests/polygons/snapshots/<name>.snap.geojson` at buffer 0 and
    `tests/polygons/snapshots-5/<name>-5.snap.geojson` at buffer 5 (original polygon + every per-tile
    piece, colored), asserted byte-exact (no trailing newline; covered by the existing
    `exclude: '\.snap\.geojson'` in `.pre-commit-config.yaml`).
  - A reassembly test: slice all fixtures → every tile-insertion permutation → `Mosaic` → assert
    order-independence + edge-set equality with the input rings + one shared
    `snapshots-poly-mosaic/reassembled.snap.geojson`. Optionally cross-check against `geo` (§11).
- **`tests/polygon-bad-fixtures/`** — hand-built inconsistent tile sets (the `role: "tile x/y"`
  pattern from `mosaic_bad.rs`) that must be rejected with `TileError::Conflict` for every insertion
  order, atomically. Reuse `tests/support` loaders (`load_fixture`, `TileId::origin`, etc.); add a
  ring-aware variant only if needed.
- Reuse `tests/support/mod.rs` helpers throughout; keep the combined good-fixture footprint small
  enough that `permutations(n)` (asserts n ≤ 10) stays enumerable, as the polyline mosaic test does.
- Extend `just fmt-geojson` to also format the new fixture dirs.

Fixtures are created **at implementation time** (binary snapshots need working code to bless against),
following this spec.

## 13. Phasing

All phases are done:

1. Extract shared integer primitives (§9); rewrite `segment_intersects` on top (no behavior change).
2. `synthetic_at` trait addition (§ below) + ring role in storage.
3. `PolygonSlicerOne`: per-ring clip → seam-join → detour close (§5) → containment (§6).
4. `PolygonSlicerAll`: polygon `RouteSink` over `route_within()` + row crossing index (detour
   windings and parity) + scanline fill runs (§6/§8).
5. `Mosaic` polygon path: geometric synthetic drop-filter → existing stitch (§7).
6. Fixtures + snapshots + reassembly + bad-data tests (§12); examples/benches (§8); the
   `polygon_equivalence` fuzz target (every `All` tile against `One`).

`PolygonSlicerAll`'s tests (`tests/polygon_all.rs`) check every tile of the reachable span against
`PolygonSlicerOne` on all fixtures at several grids and on random self-intersecting / overlapping /
star-shaped multipolygons; fill coverage against `geo`'s point-in-polygon of each tile center;
reassembly through `PolygonMosaic`; and a z14-like square with a hole (2^28 tiles) in bounded time
and memory. The fixture snapshots (`tests/polygons/snapshots{,-5}/`) are shared: `clip_polygon.rs`
asserts that `All`, with its fill runs expanded, yields exactly `One`'s per-tile output.

## Appendix — `synthetic_at` and `M: Default`

Minting a synthetic corner from a bare position needs a constructor the current `Vertex` trait lacks
(`with_position` copies an existing `V`, dragging along a real `M`). Add, on a polygon-only extension
trait to keep `Vertex` clean:

```
fn synthetic_at(position: Coord<i32>) -> Self;   // Coord<i32>: = position
                                                 // Measured<M>: position + M::default()  (M: Default)
```

Per the agreed decision, synthetic corners carry `M::default()`. They're invisible (outside `B`) and
dropped on reassembly, so the value is pure filler; the `M: Default` bound is localized to the polygon
path.
