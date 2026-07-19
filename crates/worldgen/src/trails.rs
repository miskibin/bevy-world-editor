//! Wild trails: an A* walker on a coarse slope-cost grid connects lakes + clearings,
//! then the polylines are stamped into a full-res 0..1 "wear" map (1 = beaten path
//! core). The splat shader turns wear into bare trodden dirt; scatter keeps trees and
//! rocks off the lanes.

use crate::heightfield::HeightField;
use crate::noise::fbm;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

const COARSE: usize = 4; // metres per A* cell

struct Node {
    cost: f32,
    idx: usize,
}
impl PartialEq for Node {
    fn eq(&self, o: &Self) -> bool {
        self.cost == o.cost
    }
}
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Node {
    fn cmp(&self, o: &Self) -> Ordering {
        o.cost.partial_cmp(&self.cost).unwrap_or(Ordering::Equal)
    }
}

/// Dijkstra (uniform-goal A* without heuristic — the grid is small) over slope cost.
fn route(
    cost_map: &[f32],
    side: usize,
    from: (usize, usize),
    to: (usize, usize),
) -> Option<Vec<(usize, usize)>> {
    let n = side * side;
    let mut dist = vec![f32::MAX; n];
    let mut prev = vec![usize::MAX; n];
    let start = from.1 * side + from.0;
    let goal = to.1 * side + to.0;
    dist[start] = 0.0;
    let mut heap = BinaryHeap::new();
    heap.push(Node { cost: 0.0, idx: start });
    while let Some(Node { cost, idx }) = heap.pop() {
        if idx == goal {
            let mut path = Vec::new();
            let mut cur = goal;
            while cur != usize::MAX {
                path.push((cur % side, cur / side));
                cur = prev[cur];
            }
            path.reverse();
            return Some(path);
        }
        if cost > dist[idx] {
            continue;
        }
        let x = idx % side;
        let z = idx / side;
        for (dx, dz) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)]
        {
            let nx = x as i32 + dx;
            let nz = z as i32 + dz;
            if nx < 0 || nz < 0 || nx >= side as i32 || nz >= side as i32 {
                continue;
            }
            let ni = nz as usize * side + nx as usize;
            let step = if dx != 0 && dz != 0 { 1.414 } else { 1.0 };
            let c = cost + step * cost_map[ni];
            if c < dist[ni] {
                dist[ni] = c;
                prev[ni] = idx;
                heap.push(Node { cost: c, idx: ni });
            }
        }
    }
    None
}

/// Build the wear map. POIs: one shore point per lake + a few noise-picked clearings.
pub fn build_trails(
    hf: &HeightField,
    slope: &[f32],
    water: &[f32],
    seed: u32,
) -> Vec<f32> {
    let size = hf.size;
    let side = size / COARSE;

    // Coarse cost: gentle ground cheap, steep expensive, water impassable.
    let mut cost = vec![1.0f32; side * side];
    for cz in 0..side {
        for cx in 0..side {
            let i = (cz * COARSE + COARSE / 2) * size + cx * COARSE + COARSE / 2;
            cost[cz * side + cx] = if water[i].is_finite() {
                1.0e6
            } else {
                1.0 + slope[i] * 40.0
            };
        }
    }

    // POIs: per-lake shore cell (first dry coarse cell next to water), plus clearings.
    let mut pois: Vec<(usize, usize)> = Vec::new();
    let mut lake_marked = vec![false; side * side];
    for cz in 1..side - 1 {
        for cx in 1..side - 1 {
            let i = (cz * COARSE) * size + cx * COARSE;
            if water[i].is_finite() && !lake_marked[cz * side + cx] {
                // Flood-mark this lake's coarse cells so we take ONE poi per lake.
                let mut stack = vec![(cx, cz)];
                let mut shore: Option<(usize, usize)> = None;
                while let Some((x, z)) = stack.pop() {
                    if lake_marked[z * side + x] {
                        continue;
                    }
                    lake_marked[z * side + x] = true;
                    for (nx, nz) in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                        if nx == 0 || nz == 0 || nx >= side - 1 || nz >= side - 1 {
                            continue;
                        }
                        let ni = (nz * COARSE) * size + nx * COARSE;
                        if water[ni].is_finite() {
                            stack.push((nx, nz));
                        } else if shore.is_none() && cost[nz * side + nx] < 100.0 {
                            shore = Some((nx, nz));
                        }
                    }
                }
                if let Some(s) = shore {
                    pois.push(s);
                }
            }
        }
    }
    // Clearings: low-frequency noise minima on gentle ground.
    for k in 0..9u32 {
        let mut best = (0usize, 0usize, f32::MAX);
        for cz in (4..side - 4).step_by(3) {
            for cx in (4..side - 4).step_by(3) {
                let wx = (cx * COARSE) as f32;
                let wz = (cz * COARSE) as f32;
                let n = fbm(wx / 260.0 + k as f32 * 7.3, wz / 260.0, 3, seed.wrapping_add(555));
                let c = cost[cz * side + cx];
                if c < 3.0 && n < best.2 {
                    best = (cx, cz, n);
                }
            }
        }
        if best.2 < f32::MAX {
            pois.push((best.0, best.1));
        }
    }

    // Chain POIs nearest-first and stamp each route.
    let mut wear = vec![0.0f32; size * size];
    if pois.len() < 2 {
        return wear;
    }
    let mut visited = vec![false; pois.len()];
    visited[0] = true;
    let mut current = 0usize;
    for _ in 1..pois.len() {
        let (mut best, mut bd) = (usize::MAX, f32::MAX);
        for (j, p) in pois.iter().enumerate() {
            if !visited[j] {
                let d = ((p.0 as f32 - pois[current].0 as f32).powi(2)
                    + (p.1 as f32 - pois[current].1 as f32).powi(2))
                .sqrt();
                if d < bd {
                    bd = d;
                    best = j;
                }
            }
        }
        if best == usize::MAX {
            break;
        }
        if let Some(path) = route(&cost, side, pois[current], pois[best]) {
            stamp(&mut wear, size, &path, seed);
        }
        visited[best] = true;
        current = best;
    }
    wear
}

/// Rasterise a coarse path into the full-res wear map: ~0.9 m beaten core fading out by
/// ~2.8 m, with a noisy meander so the lane doesn't run on the A* grid.
fn stamp(wear: &mut [f32], size: usize, path: &[(usize, usize)], seed: u32) {
    for w in path.windows(2) {
        let (ax, az) = (w[0].0 as f32 * COARSE as f32, w[0].1 as f32 * COARSE as f32);
        let (bx, bz) = (w[1].0 as f32 * COARSE as f32, w[1].1 as f32 * COARSE as f32);
        let len = ((bx - ax).powi(2) + (bz - az).powi(2)).sqrt().max(0.5);
        let steps = (len * 2.0) as usize + 1;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            // Meander: low-freq sideways wobble breaks the grid-straight segments.
            let mx = ax + (bx - ax) * t;
            let mz = az + (bz - az) * t;
            let wob =
                (fbm(mx / 31.0, mz / 31.0, 2, seed.wrapping_add(99)) - 0.5) * 5.0;
            let (dx, dz) = ((bz - az) / len, -(bx - ax) / len); // perpendicular
            let cx = mx + dx * wob;
            let cz = mz + dz * wob;
            let r = 5;
            for oz in -r..=r {
                for ox in -r..=r {
                    let gx = (cx + ox as f32) as i32;
                    let gz = (cz + oz as f32) as i32;
                    if gx < 0 || gz < 0 || gx >= size as i32 || gz >= size as i32 {
                        continue;
                    }
                    let d = ((gx as f32 - cx).powi(2) + (gz as f32 - cz).powi(2)).sqrt();
                    // Wider lane (user: "nie widzę ścieżek"): ~1.5 m beaten core, 4.5 m halo.
                    let v = 1.0 - crate::noise::smoothstep(1.5, 4.5, d);
                    let i = gz as usize * size + gx as usize;
                    wear[i] = wear[i].max(v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::{TerrainParams, generate_base};
    use crate::maps::slope_map;

    #[test]
    fn trails_exist_and_bounded() {
        let tp = TerrainParams { size: 256, ..Default::default() };
        let hf = generate_base(&tp, |_| {});
        let slope = slope_map(&hf);
        let water = vec![f32::NEG_INFINITY; 256 * 256];
        let wear = build_trails(&hf, &slope, &water, 7);
        assert!(wear.iter().all(|w| (0.0..=1.0).contains(w)));
        let worn = wear.iter().filter(|&&w| w > 0.5).count();
        assert!(worn > 100, "no trails stamped ({worn} worn cells)");
        // Deterministic.
        let wear2 = build_trails(&hf, &slope, &water, 7);
        assert_eq!(wear, wear2);
    }
}
