//! Through-flowing river: trace the dominant drainage line out of the erosion flow map,
//! carve a channel down it, and fill it with water.
//!
//! **Why the flow map and not a hand-drawn spline.** The droplet erosion sim already
//! answers "where does water go on this terrain" far better than any curve we could author
//! — its high-flow cells ARE the valley floors. Carving along them means the river sits in
//! terrain that already looks like it belongs to a river, and it stays deterministic from
//! the seed like everything else in the generator.
//!
//! The three things this module guarantees, because a river that fails any of them reads
//! as broken from the first screenshot:
//!
//! 1. **The water surface only ever descends.** A traced drainage line can still step
//!    uphill across a saddle; a river that climbs is instantly wrong, so the surface is
//!    forced monotone and the bed is derived from it.
//! 2. **It crosses the whole map.** A river that stops in the middle of the sand is a
//!    canal to nowhere. Both ends are extended to the border if the trace stalls early.
//! 3. **It is fordable somewhere.** Width and depth are modulated along the course, and
//!    the shallow spots are what let [`crate::trails`] route paths across instead of
//!    treating the river as a wall that cuts the map in two.

use crate::heightfield::HeightField;
use crate::noise::fbm;

/// Result of carving. `water` follows the same convention as lake surfaces: the water
/// height where wet, `NEG_INFINITY` where dry, so it can be merged with lakes elementwise.
pub struct River {
    pub water: Vec<f32>,
    /// Channel centreline in world metres, head → mouth.
    pub course: Vec<(f32, f32)>,
    /// Authored water-surface height per course node — non-increasing by construction.
    /// Exposed because the rasterised `water` map cannot express this: where the course
    /// meanders back within a metre of itself, one grid cell serves two reaches.
    pub surface: Vec<f32>,
}

/// Channel shape. Metres.
#[derive(Debug, Clone, Copy)]
pub struct RiverParams {
    /// Half-width of the flat bed at its narrowest.
    pub half_width: f32,
    /// How much wider the widest reach gets (multiplier on `half_width`).
    pub width_swell: f32,
    /// Water depth in the deepest reaches.
    pub depth: f32,
    /// Water depth in the shallowest reach. Must stay comfortably below
    /// `trails::FORD_DEPTH` — the carve can cut a ford cell a few tens of centimetres
    /// deeper than authored where neighbouring (deeper) reaches overlap it, so leaving no
    /// margin here means some seeds end up with no crossable reach at all.
    pub min_depth: f32,
    /// Width of the sloped bank outside the flat bed.
    pub bank: f32,
    /// Minimum fall per metre of course — forces the water surface monotone downhill.
    pub grade: f32,
}

impl Default for RiverParams {
    fn default() -> Self {
        RiverParams {
            half_width: 4.5,
            width_swell: 2.1,
            depth: 2.6,
            min_depth: 0.35,
            bank: 7.0,
            // 1:1600 — enough that the surface never plateaus into a stagnant pool, small
            // enough that a 700 m course only drops half a metre and the water still
            // reads level across the map.
            grade: 0.000_62,
        }
    }
}

/// 8-neighbour offsets.
const NB: [(i32, i32); 8] =
    [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)];

/// Carve the dominant drainage line into `hf` and return the resulting water surface.
///
/// `flow` is the droplet-passage map from [`crate::erosion::erode`] (same grid as `hf`).
pub fn carve(hf: &mut HeightField, flow: &[f32], p: &RiverParams, seed: u32) -> River {
    let size = hf.size;
    let n = size * size;
    debug_assert_eq!(flow.len(), n);
    let mut water = vec![f32::NEG_INFINITY; n];
    if size < 32 {
        return River { water, course: Vec::new(), surface: Vec::new() };
    }

    // 1. Source cell: the strongest flow away from the border, where the trace has room
    //    to run in both directions.
    let margin = size / 10;
    let mut src = (margin, margin);
    let mut best = f32::MIN;
    for z in margin..size - margin {
        for x in margin..size - margin {
            let f = flow[z * size + x];
            if f > best {
                best = f;
                src = (x, z);
            }
        }
    }

    // 2. Trace both ways from the source, then join head → mouth.
    let mut up = trace(hf, flow, src, Dir::Up);
    up.reverse();
    let down = trace(hf, flow, src, Dir::Down);
    let mut cells: Vec<(usize, usize)> = up;
    cells.extend(down.into_iter().skip(1)); // `src` is in both traces
    if cells.len() < 8 {
        return River { water, course: Vec::new(), surface: Vec::new() };
    }

    // 3. Smooth away the 8-direction staircase, then resample to ~1 m steps so the stamp
    //    below leaves no gaps on diagonals.
    let course = resample(&smooth(&cells, hf.cell), 1.0);
    if course.len() < 8 {
        return River { water, course, surface: Vec::new() };
    }

    // 4. Water surface, then bed.
    //
    // The invariant that has to hold is on the SURFACE, not the bed: water cannot flow
    // uphill, but a real bed rises and falls underneath it, and that variation is exactly
    // what makes some reaches deep and others fordable. Deriving `bed = surface − depth`
    // from a monotone surface gives both at once. Forcing the *bed* monotone instead — the
    // obvious first move — flattens every ford back into one uniform trench and, worse,
    // leaves shallow reaches as a dry gully with a trickle at the bottom.
    //
    // `FREEBOARD` keeps the surface just under the surrounding grade so the banks stay dry.
    const FREEBOARD: f32 = 0.35;
    // The monotone envelope is taken over a SMOOTHED ground profile, not the raw one.
    // A running minimum latches onto the deepest pit it has passed, so one local hollow
    // would hold the surface metres below grade for the entire rest of the course — the
    // river becomes a canyon downstream of it and every reach is far too deep to ford.
    // Averaging the profile over ~±25 m first removes the pits without flattening the
    // real, map-scale fall the river is following.
    let ground: Vec<f32> = course.iter().map(|&(wx, wz)| hf.sample_world(wx, wz)).collect();
    let ground = smooth_profile(&ground, 25);
    let mut surface = Vec::with_capacity(course.len());
    let mut swells = Vec::with_capacity(course.len());
    let mut running = f32::INFINITY;
    let mut prev = course[0];
    for (i, &(wx, wz)) in course.iter().enumerate() {
        let step = ((wx - prev.0).powi(2) + (wz - prev.1).powi(2)).sqrt();
        running = (running - step * p.grade).min(ground[i] - FREEBOARD);
        surface.push(running);
        // ~90 m period: several deep reaches and several fords on a 500 m map. Too long a
        // period and a given map can end up with no crossing at all.
        swells.push(fbm(i as f32 / 90.0, 0.0, 2, seed.wrapping_add(613)));
        prev = (wx, wz);
    }
    // Rescale the swell to span the full 0..1 *on this river*.
    //
    // Load-bearing, not cosmetic: raw 2-octave fBM clusters around the middle of its range
    // and on a given seed may never dip below ~0.4, so `min_depth` would never be reached
    // and that map would have no fordable crossing at all — the river would wall it in
    // half. Normalising per-course guarantees the shallowest reach IS `min_depth`, on
    // every seed, while staying deterministic.
    {
        let lo = swells.iter().cloned().fold(f32::INFINITY, f32::min);
        let hi = swells.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let span = (hi - lo).max(1e-4);
        for s in swells.iter_mut() {
            *s = ((*s - lo) / span).clamp(0.0, 1.0);
        }
    }

    // 5. Carve, then fill — in two passes, because a later node can still cut a cell an
    //    earlier node already looked at, and a cell's wetness depends on its final height.
    //
    //    Wide reaches are deep, narrow ones are the fords (module docs #3).
    let max_r = p.half_width * p.width_swell + p.bank;
    let rad = (max_r / hf.cell).ceil() as i32;
    let half_width = |i: usize| p.half_width * (0.55 + (p.width_swell - 0.55) * swells[i]);
    let water_depth = |i: usize| p.min_depth + (p.depth - p.min_depth) * swells[i];

    let cell = hf.cell;
    let mut hits: Vec<(usize, f32)> = Vec::new();

    // Pass 1 — carve. Cross-section: flat bed inside the half-width, then a bank climbing
    // back to the local grade (bed + water depth + freeboard) over `bank` metres.
    for i in 0..course.len() {
        let hw = half_width(i);
        let water_d = water_depth(i);
        let outer = hw + p.bank;
        let b = surface[i] - water_d;
        near_cells(course[i], cell, size, rad, outer, &mut hits);
        // Only ever cut down. Raising terrain here would dam the valley the erosion sim
        // spent its whole budget opening.
        for &(idx, d) in &hits {
            let target = if d <= hw {
                b
            } else {
                let u = (d - hw) / p.bank;
                b + (water_d + FREEBOARD) * (u * u * (3.0 - 2.0 * u))
            };
            if target < hf.h[idx] {
                hf.h[idx] = target;
            }
        }
    }

    // Pass 2 — fill, from the NEAREST node rather than the highest.
    //
    // Taking the max instead looks equivalent and is not: where the course meanders back
    // within a channel-width of itself, an upstream reach several metres higher spills its
    // surface into the downstream one, and the river visibly flows uphill at the bend.
    // Nearest-node keeps each cell on the reach it actually belongs to. Filling is a
    // separate pass because a later node can still cut a cell an earlier node has already
    // looked at, and whether a cell is wet depends on its FINAL height.
    let mut best_d = vec![f32::INFINITY; n];
    for i in 0..course.len() {
        let outer = half_width(i) + p.bank;
        let surf = surface[i];
        near_cells(course[i], cell, size, rad, outer, &mut hits);
        for &(idx, d) in &hits {
            if d < best_d[idx] && hf.h[idx] < surf {
                best_d[idx] = d;
                water[idx] = surf;
            }
        }
    }

    River { water, course, surface }
}

/// Moving average over a 1-D profile, window `±w`, shrinking at the ends.
fn smooth_profile(v: &[f32], w: usize) -> Vec<f32> {
    let n = v.len() as i32;
    let w = w as i32;
    (0..n)
        .map(|i| {
            let k = w.min(i).min(n - 1 - i);
            let (mut s, mut c) = (0.0f32, 0.0f32);
            for j in i - k..=i + k {
                s += v[j as usize];
                c += 1.0;
            }
            s / c
        })
        .collect()
}

/// Collect every grid cell within `outer` metres of a course point as `(index, distance)`.
/// Reuses the caller's buffer — this runs once per metre of river, twice.
fn near_cells(
    (wx, wz): (f32, f32),
    cell: f32,
    size: usize,
    rad: i32,
    outer: f32,
    out: &mut Vec<(usize, f32)>,
) {
    out.clear();
    let cx = (wx / cell) as i32;
    let cz = (wz / cell) as i32;
    for dz in -rad..=rad {
        for dx in -rad..=rad {
            let (gx, gz) = (cx + dx, cz + dz);
            if gx < 0 || gz < 0 || gx >= size as i32 || gz >= size as i32 {
                continue;
            }
            let (gx, gz) = (gx as usize, gz as usize);
            let d = ((gx as f32 * cell - wx).powi(2) + (gz as f32 * cell - wz).powi(2)).sqrt();
            if d <= outer {
                out.push((gz * size + gx, d));
            }
        }
    }
}

enum Dir {
    Up,
    Down,
}

/// Walk the drainage line from `start`. `Down` follows the steepest descent (ties to the
/// bigger flow); `Up` follows the biggest tributary. Both stop at the border, and both
/// refuse to revisit a cell so a flat basin can't trap the walk in a two-cell loop.
fn trace(hf: &HeightField, flow: &[f32], start: (usize, usize), dir: Dir) -> Vec<(usize, usize)> {
    let size = hf.size;
    let mut seen = vec![false; size * size];
    let mut path = vec![start];
    seen[start.1 * size + start.0] = true;
    let (mut x, mut z) = start;
    // A course longer than 4× the map side is a sign the walk is spiralling in a basin.
    let cap = size * 4;
    while path.len() < cap {
        let mut best: Option<(usize, usize, f32)> = None;
        for (dx, dz) in NB {
            let nx = x as i32 + dx;
            let nz = z as i32 + dz;
            if nx < 0 || nz < 0 || nx >= size as i32 || nz >= size as i32 {
                continue;
            }
            let (nx, nz) = (nx as usize, nz as usize);
            let ni = nz * size + nx;
            if seen[ni] {
                continue;
            }
            // Downstream ranks by height (lowest wins) with flow as a tie-break; upstream
            // ranks purely by flow, since "uphill" alone would climb any random slope.
            let score = match dir {
                Dir::Down => -hf.h[ni] + flow[ni] * 1e-4,
                Dir::Up => {
                    if hf.h[ni] < hf.h[z * size + x] - 0.35 {
                        continue; // never walk downhill while heading upstream
                    }
                    flow[ni]
                }
            };
            if best.is_none_or(|(_, _, s)| score > s) {
                best = Some((nx, nz, score));
            }
        }
        let Some((nx, nz, _)) = best else { break };
        seen[nz * size + nx] = true;
        path.push((nx, nz));
        x = nx;
        z = nz;
        if x == 0 || z == 0 || x == size - 1 || z == size - 1 {
            break;
        }
    }
    extend_to_border(&mut path, size);
    path
}

/// Guarantee #2 from the module docs: if a trace stalled inland, keep going in the mean
/// heading of its last stretch until the border. A dead-end river is worse than a
/// slightly-too-straight one.
fn extend_to_border(path: &mut Vec<(usize, usize)>, size: usize) {
    let Some(&(ex, ez)) = path.last() else { return };
    if ex == 0 || ez == 0 || ex == size - 1 || ez == size - 1 || path.len() < 4 {
        return;
    }
    let k = path.len().min(12);
    let (sx, sz) = path[path.len() - k];
    let (mut dx, mut dz) = (ex as f32 - sx as f32, ez as f32 - sz as f32);
    let len = (dx * dx + dz * dz).sqrt();
    if len < 1e-3 {
        return;
    }
    dx /= len;
    dz /= len;
    let (mut fx, mut fz) = (ex as f32, ez as f32);
    for _ in 0..size * 2 {
        fx += dx;
        fz += dz;
        if fx < 0.0 || fz < 0.0 || fx > (size - 1) as f32 || fz > (size - 1) as f32 {
            break;
        }
        let (nx, nz) = (fx as usize, fz as usize);
        if path.last() != Some(&(nx, nz)) {
            path.push((nx, nz));
        }
        if nx == 0 || nz == 0 || nx == size - 1 || nz == size - 1 {
            break;
        }
    }
}

/// Moving average over the cell path → world metres. The window is wide because the raw
/// 8-neighbour walk is visibly stair-stepped and a river is the one feature a player
/// traces with their eye from end to end.
///
/// The window **shrinks towards the ends** so the first and last points survive untouched.
/// A clamped-window average instead drags both endpoints several cells inland, which
/// silently breaks guarantee #2 — the river stops short of the border and dead-ends in
/// open sand.
fn smooth(cells: &[(usize, usize)], cell: f32) -> Vec<(f32, f32)> {
    const W: i32 = 7;
    let n = cells.len() as i32;
    (0..n)
        .map(|i| {
            let w = W.min(i).min(n - 1 - i);
            let (mut sx, mut sz, mut c) = (0.0f32, 0.0f32, 0.0f32);
            for k in -w..=w {
                let j = (i + k) as usize;
                sx += cells[j].0 as f32;
                sz += cells[j].1 as f32;
                c += 1.0;
            }
            (sx / c * cell, sz / c * cell)
        })
        .collect()
}

/// Resample a polyline to a fixed step in metres.
fn resample(pts: &[(f32, f32)], step: f32) -> Vec<(f32, f32)> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    let mut carry = 0.0f32;
    for w in pts.windows(2) {
        let (ax, az) = w[0];
        let (bx, bz) = w[1];
        let seg = ((bx - ax).powi(2) + (bz - az).powi(2)).sqrt();
        if seg < 1e-5 {
            continue;
        }
        let mut t = step - carry;
        while t <= seg {
            let u = t / seg;
            out.push((ax + (bx - ax) * u, az + (bz - az) * u));
            t += step;
        }
        carry = (carry + seg) % step;
    }
    // The walk above stops at the last whole step, which can fall short of the final point
    // by up to `step`. That final metre is the one that has to touch the border
    // (guarantee #2), so it is appended explicitly.
    let last = *pts.last().unwrap();
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::TerrainStyle;
    use crate::erosion::{ErosionParams, erode};
    use crate::heightfield::{TerrainParams, generate_base};

    fn eroded(size: usize, seed: u32) -> (HeightField, Vec<f32>) {
        let tp = TerrainParams { size, seed, ..Default::default() };
        let mut hf = generate_base(&tp, TerrainStyle::Mesas, |_| {});
        let ep = ErosionParams { droplets: 9000, ..Default::default() };
        let flow = erode(&mut hf, &ep, seed, |_| {});
        (hf, flow)
    }

    #[test]
    fn river_is_wet_deterministic_and_finite() {
        let p = RiverParams::default();
        let (mut a, fa) = eroded(256, 7);
        let (mut b, fb) = eroded(256, 7);
        let ra = carve(&mut a, &fa, &p, 7);
        let rb = carve(&mut b, &fb, &p, 7);
        assert_eq!(ra.course.len(), rb.course.len());
        assert_eq!(a.h, b.h, "carve is not deterministic");
        let wet = ra.water.iter().filter(|w| w.is_finite()).count();
        assert!(wet > 500, "river barely got wet: {wet} cells");
        assert!(a.h.iter().all(|v| v.is_finite()));
        assert!(ra.water.iter().all(|w| !w.is_nan()));
    }

    /// Guarantee #1: the water surface must never climb from head to mouth.
    #[test]
    fn water_surface_never_flows_uphill() {
        let p = RiverParams::default();
        let (mut hf, flow) = eroded(256, 11);
        let r = carve(&mut hf, &flow, &p, 11);
        assert!(r.surface.len() > 100, "course is barely there");
        let mut prev = f32::INFINITY;
        for &s in &r.surface {
            assert!(s <= prev, "river climbs: {s} after {prev}");
            prev = s;
        }
        // And the rasterised map agrees with the authored surface: every wet cell sits
        // between the mouth and head levels, never above the source.
        let (head, mouth) = (r.surface[0], *r.surface.last().unwrap());
        for &w in r.water.iter().filter(|w| w.is_finite()) {
            assert!(w <= head + 1e-3 && w >= mouth - 1e-3, "stray water level {w}");
        }
    }

    /// Guarantee #2: both ends reach the border, so the river crosses the map.
    #[test]
    fn river_crosses_the_whole_map() {
        let p = RiverParams::default();
        for seed in [3u32, 19, 42] {
            let (mut hf, flow) = eroded(256, seed);
            let ext = hf.extent();
            let r = carve(&mut hf, &flow, &p, seed);
            let at_border = |&(x, z): &(f32, f32)| {
                x < 3.0 || z < 3.0 || x > ext - 3.0 || z > ext - 3.0
            };
            let head = r.course.first().expect("no course");
            let mouth = r.course.last().unwrap();
            assert!(at_border(head) && at_border(mouth), "seed {seed}: river ends inland");
            // And it is a real crossing, not a nick in one corner.
            assert!(r.course.len() as f32 > ext * 0.6, "seed {seed}: course too short");
        }
    }

    /// Guarantee #3: every seed leaves at least one reach shallow enough for
    /// `trails::FORD_DEPTH`, or the river walls the map in half.
    #[test]
    fn river_has_fords() {
        let p = RiverParams::default();
        for seed in [5u32, 19, 31] {
            let (mut hf, flow) = eroded(256, seed);
            let r = carve(&mut hf, &flow, &p, seed);
            let mut shallow = 0;
            for &(wx, wz) in r.course.iter() {
                let i = ((wz / hf.cell) as usize).min(hf.size - 1) * hf.size
                    + ((wx / hf.cell) as usize).min(hf.size - 1);
                let d = r.water[i] - hf.h[i];
                // Must match `trails::FORD_DEPTH`; `RiverParams::min_depth` keeps margin.
                if d.is_finite() && d <= 1.1 {
                    shallow += 1;
                }
            }
            assert!(shallow > 0, "seed {seed}: no fordable reach anywhere on the river");
        }
    }


    #[test]
    fn carve_only_lowers_terrain() {
        let p = RiverParams::default();
        let (before, flow) = eroded(192, 23);
        let mut after = before.clone();
        carve(&mut after, &flow, &p, 23);
        for (a, b) in after.h.iter().zip(&before.h) {
            assert!(*a <= *b + 1e-4, "carve raised ground: {a} > {b}");
        }
    }
}
