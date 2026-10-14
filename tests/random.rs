//! Randomized differential tests of the all-tiles iterators against their single-tile counterparts.
//!
//! The single-tile calls are the oracle: a plain scan of the geometry against one tile's box. For
//! random lines, points and polygons (star-shaped, self-intersecting, with holes and repeated
//! vertices) on small grids, every tile around the geometry must hold exactly what the all-tiles
//! iterator yielded for it. The multi-geometries here (several polygons, parts or points) are sliced
//! one element at a time, as a caller would. Also: nothing near the `i32` limits may panic, and the
//! point iterator agrees with a direct test of each candidate tile there, errors included.
//!
//! The streams are fixed, so a failure reproduces; the tests are cheap enough to run in debug builds,
//! where overflow checks are on. The fuzz targets cover the same properties open-endedly.

#![allow(clippy::pedantic, reason = "test tool")]

use std::collections::BTreeMap;

use geo_types::Coord;
use map_tile_toolkit::{PolygonSlice, Slicer, TileId};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> i64 {
        (self.next() % n) as i64
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64)
    }
}

/// A star-shaped ring around `c` with radii in `r0..=r1`.
fn star(rng: &mut Rng, c: (i64, i64), r0: i64, r1: i64, n: usize, rev: bool) -> Vec<Coord<i32>> {
    let mut pts = Vec::new();
    for i in 0..n {
        let a = (i as f64 + rng.below(100) as f64 / 400.0) / n as f64 * std::f64::consts::TAU;
        let r = rng.range(r0, r1) as f64;
        pts.push(Coord {
            x: (c.0 as f64 + r * a.cos()).round() as i32,
            y: (c.1 as f64 + r * a.sin()).round() as i32,
        });
    }
    if rev {
        pts.reverse();
    }
    pts
}

type Rings = Vec<Vec<Coord<i32>>>;

/// A tile's runs, each with the part it came from.
type Runs = Vec<(u32, Vec<Coord<i32>>)>;

/// Every polygon of `input` sliced into all tiles, as `tile → (polygon, rings)` in polygon order.
fn all_tiles(
    slicer: &mut Slicer,
    input: &[Rings],
) -> Result<BTreeMap<TileId, Vec<(u32, Rings)>>, map_tile_toolkit::TileError> {
    let mut map: BTreeMap<TileId, Vec<(u32, Rings)>> = BTreeMap::new();
    for (polygon, rings) in (0_u32..).zip(input) {
        let mut slices = slicer.polygon(rings)?;
        // Runs recorded, expanded after (fill_ring borrows the iterator).
        let mut runs = Vec::new();
        while let Some(s) = slices.next() {
            match s? {
                PolygonSlice::Piece(p) => {
                    let rings: Rings = p.rings().map(|r| r.to_vec()).collect();
                    map.entry(p.tile()).or_default().push((polygon, rings));
                }
                PolygonSlice::Run(r) => runs.push(r),
            }
        }
        for r in &runs {
            let ring = slices.fill_ring(r).to_vec();
            for x in r.x.clone() {
                map.entry(TileId::new(x, r.y))
                    .or_default()
                    .push((polygon, vec![ring.clone()]));
            }
        }
    }
    Ok(map)
}

/// Every polygon of `input` clipped to one tile, as `(polygon, rings)` in polygon order.
fn one_tile(slicer: &mut Slicer, tile: TileId, input: &[Rings]) -> Vec<(u32, Rings)> {
    let mut one = Vec::new();
    for (polygon, rings) in (0_u32..).zip(input) {
        if let Some(p) = slicer.tile_polygon(tile, rings).expect("clip") {
            one.push((polygon, p.rings().map(|r| r.to_vec()).collect()));
        }
    }
    one
}

#[test]
fn random_polygons_match_single_tile() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let mut checked_tiles = 0u64;
    let mut cases = 0;
    for case in 0..600 {
        let extent = [4u32, 5, 8, 10, 16, 25, 33, 64][case % 8];
        let buffer = rng.below(u64::from(extent).div_ceil(2)) as u16;
        let span = i64::from(extent) * 6;
        let npoly = rng.range(1, 3) as usize;
        let mut input: Vec<Rings> = Vec::new();
        for _ in 0..npoly {
            let c = (rng.range(-span, span), rng.range(-span, span));
            let r1 = rng.range(2, span);
            let r0 = (r1 / 3).max(1);
            let n = rng.range(3, 40) as usize;
            let rev = rng.below(2) == 0;
            let mut rings = vec![star(&mut rng, c, r0, r1, n, rev)];
            for _ in 0..rng.below(3) {
                let hr = (r0 / 2).max(1);
                let hc = (c.0 + rng.range(-1, 1), c.1 + rng.range(-1, 1));
                let hn = rng.range(3, 12) as usize;
                rings.push(star(&mut rng, hc, 1.max(hr / 2), hr, hn, !rev));
            }
            // Sometimes repeat vertices / close the ring.
            if rng.below(4) == 0 {
                let first = rings[0][0];
                rings[0].push(first);
            }
            if rng.below(4) == 0 {
                let mut dup = Vec::new();
                for v in &rings[0] {
                    dup.push(*v);
                    if rng.below(3) == 0 {
                        dup.push(*v);
                    }
                }
                rings[0] = dup;
            }
            input.push(rings);
        }
        let mut slicer = Slicer::new(extent, buffer).unwrap();
        let got = match all_tiles(&mut slicer, &input) {
            Ok(g) => g,
            Err(e) => panic!("case {case}: {e}"),
        };
        // Reachable tile span, padded.
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for p in &input {
            for r in p {
                for v in r {
                    lo = lo.min(i64::from(v.x.min(v.y)));
                    hi = hi.max(i64::from(v.x.max(v.y)));
                }
            }
        }
        let e = i64::from(extent);
        let (t0, t1) = ((lo / e) - 2, (hi / e) + 2);
        for ty in t0..=t1 {
            for tx in t0..=t1 {
                let tile = TileId::new(tx as i32, ty as i32);
                let one = one_tile(&mut slicer, tile, &input);
                let empty = Vec::new();
                let g = got.get(&tile).unwrap_or(&empty);
                assert_eq!(
                    g, &one,
                    "case {case} extent {extent} buffer {buffer} tile {tile:?}\ninput {input:?}"
                );
                checked_tiles += 1;
            }
        }
        cases += 1;
    }
    eprintln!("{cases} cases, {checked_tiles} tiles");
}

#[test]
fn random_lines_and_points_match_single_tile() {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    let mut runs_total = 0u64;
    for case in 0..1200 {
        let extent = [2u32, 3, 4, 5, 8, 10, 16, 25, 33, 64][case % 10];
        let buffer = rng.below(u64::from(extent).div_ceil(2)) as u16;
        let span = i64::from(extent) * 6;
        let nparts = rng.range(1, 3);
        let mut parts: Vec<Vec<Coord<i32>>> = Vec::new();
        for _ in 0..nparts {
            let n = rng.range(0, 40);
            let mut cur = (rng.range(-span, span), rng.range(-span, span));
            let mut pts = Vec::new();
            for _ in 0..n {
                pts.push(Coord {
                    x: cur.0 as i32,
                    y: cur.1 as i32,
                });
                let step = match rng.below(4) {
                    0 => 1,
                    1 => i64::from(extent) / 2 + 1,
                    2 => i64::from(extent),
                    _ => span,
                };
                match rng.below(5) {
                    0 => {} // duplicate
                    1 => cur.0 += rng.range(-step, step),
                    2 => cur.1 += rng.range(-step, step),
                    _ => {
                        cur.0 += rng.range(-step, step);
                        cur.1 += rng.range(-step, step);
                    }
                }
            }
            parts.push(pts);
        }
        let slicer: Slicer = Slicer::new(extent, buffer).unwrap();
        let mut got: BTreeMap<TileId, Runs> = BTreeMap::new();
        for (part, line) in (0_u32..).zip(&parts) {
            for s in slicer.line(line).unwrap() {
                assert!(s.run().iter().count() >= 2);
                got.entry(s.tile())
                    .or_default()
                    .push((part, s.run().to_vec()));
                runs_total += 1;
            }
        }
        for v in got.values_mut() {
            v.sort_by_key(|(p, r)| (*p, r.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>()));
        }
        let e = i64::from(extent);
        let (t0, t1) = ((-span / e) - 3, (span * 3 / e) + 3);
        for ty in t0..=t1 {
            for tx in t0..=t1 {
                let tile = TileId::new(tx as i32, ty as i32);
                let mut one: Runs = (0_u32..)
                    .zip(&parts)
                    .flat_map(|(part, line)| {
                        slicer
                            .tile_line(tile, line)
                            .unwrap()
                            .map(move |s| (part, s.run().to_vec()))
                    })
                    .collect();
                one.sort_by_key(|(p, r)| (*p, r.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>()));
                let empty = Vec::new();
                assert_eq!(
                    got.get(&tile).unwrap_or(&empty),
                    &one,
                    "case {case} e{extent} b{buffer} tile {tile:?} parts {parts:?}"
                );
            }
        }
        // Points: against a brute-force box test.
        let pts: Vec<Coord<i32>> = parts.iter().flatten().copied().collect();
        let mut gp: BTreeMap<TileId, Vec<(u32, Coord<i32>)>> = BTreeMap::new();
        for (index, &p) in (0_u32..).zip(&pts) {
            for s in slicer.point(p).unwrap() {
                gp.entry(s.tile()).or_default().push((index, s.point()));
            }
        }
        for ty in t0..=t1 {
            for tx in t0..=t1 {
                let tile = TileId::new(tx as i32, ty as i32);
                let one: Vec<(u32, Coord<i32>)> = (0_u32..)
                    .zip(&pts)
                    .filter_map(|(index, &p)| Some((index, slicer.tile_point(tile, p).unwrap()?)))
                    .collect();
                let mut g = gp.get(&tile).cloned().unwrap_or_default();
                g.sort_by_key(|x| x.0);
                assert_eq!(g, one, "points case {case} e{extent} b{buffer} {tile:?}");
            }
        }
    }
    eprintln!("{runs_total} runs");
}

#[test]
fn random_self_intersecting_polygons_match_single_tile() {
    let mut rng = Rng(0xABCD_EF01_2345_6789);
    let mut tiles_checked = 0u64;
    for case in 0..600 {
        let extent = [3u32, 4, 5, 8, 10, 16, 25, 64][case % 8];
        let buffer = rng.below(u64::from(extent).div_ceil(2)) as u16;
        let span = i64::from(extent) * 5;
        let mut input: Vec<Rings> = Vec::new();
        for _ in 0..rng.range(1, 2) {
            let mut rings = Vec::new();
            for _ in 0..rng.range(1, 4) {
                let n = rng.range(3, 14);
                let ring: Vec<Coord<i32>> = (0..n)
                    .map(|_| Coord {
                        x: rng.range(-span, span) as i32,
                        y: rng.range(-span, span) as i32,
                    })
                    .collect();
                rings.push(ring);
            }
            input.push(rings);
        }
        let mut slicer = Slicer::new(extent, buffer).unwrap();
        let got = all_tiles(&mut slicer, &input).unwrap();
        let e = i64::from(extent);
        let (t0, t1) = ((-span / e) - 3, (span / e) + 3);
        for ty in t0..=t1 {
            for tx in t0..=t1 {
                let tile = TileId::new(tx as i32, ty as i32);
                let one = one_tile(&mut slicer, tile, &input);
                let empty = Vec::new();
                let g = got.get(&tile).unwrap_or(&empty);
                assert_eq!(
                    g, &one,
                    "case {case} e{extent} b{buffer} {tile:?}\ninput {input:?}"
                );
                tiles_checked += 1;
            }
        }
    }
    eprintln!("{tiles_checked} tiles");
}

#[test]
fn extreme_coordinates_never_panic() {
    let mut rng = Rng(0x0F0F_1234_9876_5432);
    let mut errs = 0;
    let mut oks = 0;
    for case in 0..1500 {
        // (The largest extent every buffer allows: `extent + buffer` must fit `i32`.)
        let extent = [
            1u32,
            2,
            3,
            5,
            8,
            25,
            4096,
            1 << 20,
            i32::MAX as u32 - u32::from(u16::MAX),
        ][case % 9];
        let buffer = rng.below(u64::from(extent).div_ceil(2).min(60000)) as u16;
        let base: i64 = match rng.below(4) {
            0 => i64::from(i32::MIN),
            1 => i64::from(i32::MAX) - 2000,
            2 => 0,
            _ => -1000,
        };
        let mut pt = || {
            let v = |r: &mut Rng| {
                (base + r.below(2000)).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
            };
            Coord {
                x: v(&mut rng),
                y: v(&mut rng),
            }
        };
        let n = 3 + (case % 5);
        let ring: Vec<Coord<i32>> = (0..n).map(|_| pt()).collect();
        let slicer: Slicer = Slicer::new(extent, buffer).unwrap();
        let mut sl: Slicer = slicer.clone();
        match slicer.line(&ring) {
            Ok(slices) => {
                // Whatever the all-tiles call accepted, each tile it yielded can be asked directly, and
                // gives the same runs.
                let mut by_tile: BTreeMap<TileId, Vec<Vec<Coord<i32>>>> = BTreeMap::new();
                let mut taken = 0;
                for s in slices.take(2000) {
                    oks += 1;
                    taken += 1;
                    by_tile.entry(s.tile()).or_default().push(s.run().to_vec());
                }
                for (tile, runs) in &by_tile {
                    let one: Vec<Vec<Coord<i32>>> = slicer
                        .tile_line(*tile, &ring)
                        .expect("accepted by line, so by tile_line")
                        .map(|s| s.run().to_vec())
                        .collect();
                    // (The cap may cut a tile's runs short, never add any.)
                    assert!(runs.len() <= one.len(), "e{extent} b{buffer} {tile:?}");
                    if taken < 2000 {
                        assert_eq!(runs, &one, "e{extent} b{buffer} {tile:?} {ring:?}");
                    }
                }
            }
            Err(_) => errs += 1,
        }
        for &p in &ring {
            match slicer.point(p) {
                Ok(slices) => slices.for_each(drop),
                Err(_) => errs += 1,
            }
        }
        let poly = [ring.clone()];
        match sl.polygon(&poly) {
            Ok(mut it) => {
                while let Some(s) = it.next() {
                    match s {
                        Ok(PolygonSlice::Piece(p)) => {
                            for r in p.rings() {
                                for _ in r.iter() {}
                            }
                        }
                        Ok(PolygonSlice::Run(r)) => {
                            let _ = it.fill_ring(&r).to_vec();
                        }
                        Err(_) => errs += 1,
                    }
                }
            }
            Err(_) => errs += 1,
        }
        let tile = TileId::new(
            rng.range(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            rng.range(-3, 3) as i32,
        );
        if let Ok(it) = slicer.tile_line(tile, &ring) {
            for s in it {
                let _ = s;
            }
        }
        let _ = sl.tile_polygon(tile, &poly);
        // And the tile of the first vertex.
        let t0 = TileId::new(
            ring[0].x.div_euclid(extent as i32),
            ring[0].y.div_euclid(extent as i32),
        );
        let _ = sl.tile_polygon(t0, &poly);
    }
    eprintln!("oks {oks} errs {errs}");
}

#[test]
fn points_at_the_limits_match_brute_force() {
    let mut rng = Rng(0x7777_1111_3333_5555);
    for case in 0..3000 {
        let extent = [
            3u32,
            4,
            5,
            8,
            25,
            4096,
            1 << 20,
            1 << 29,
            (1 << 29) + 7,
            // The largest extent every buffer allows (`extent + buffer` must fit `i32`).
            i32::MAX as u32 - u32::from(u16::MAX),
        ][case % 10];
        let buffer = rng.below(u64::from(extent).div_ceil(2).min(60000)) as u16;
        let slicer: Slicer = Slicer::new(extent, buffer).unwrap();
        let base: i64 = [
            i64::from(i32::MIN),
            i64::from(i32::MAX) - 300,
            0,
            -150,
            1 << 30,
        ][rng.below(5) as usize];
        let coord = |r: &mut Rng| {
            (base + r.below(300)).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        let pts: Vec<Coord<i32>> = (0..4)
            .map(|_| Coord {
                x: coord(&mut rng),
                y: coord(&mut rng),
            })
            .collect();
        // Per point: the call fails up front exactly when one of the tiles holding it cannot be
        // asked (its box overflows); else it yields x-major over the 3x3 tiles around its own, those
        // whose `tile_point` holds it.
        for &pt in &pts {
            let t = (
                pt.x.div_euclid(extent as i32),
                pt.y.div_euclid(extent as i32),
            );
            let mut expected: Vec<(TileId, Coord<i32>)> = Vec::new();
            let mut expected_err = false;
            for dx in -1i64..=1 {
                for dy in -1i64..=1 {
                    let (Ok(tx), Ok(ty)) = (
                        i32::try_from(i64::from(t.0) + dx),
                        i32::try_from(i64::from(t.1) + dy),
                    ) else {
                        continue;
                    };
                    let tile = TileId::new(tx, ty);
                    match slicer.tile_point(tile, pt) {
                        Err(_) => {
                            let (e, b) = (i64::from(extent), i64::from(buffer));
                            let axis = |t: i32, v: i32| {
                                let lo = i64::from(t) * e - b;
                                (lo..=lo + e - 1 + 2 * b).contains(&i64::from(v))
                            };
                            expected_err |= axis(tx, pt.x) && axis(ty, pt.y);
                        }
                        Ok(Some(local)) => expected.push((tile, local)),
                        Ok(None) => {}
                    }
                }
            }
            let got: Option<Vec<_>> = slicer
                .point(pt)
                .ok()
                .map(|it| it.map(|s| (s.tile(), s.point())).collect());
            assert_eq!(
                got.is_none(),
                expected_err,
                "case {case} e{extent} b{buffer} {pt:?}"
            );
            if let Some(got) = got {
                assert_eq!(got, expected, "case {case} e{extent} b{buffer} {pt:?}");
            }
        }
    }
}
