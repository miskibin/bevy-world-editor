//! Runtime creature pathfinding: a coarse navigation grid precomputed ONCE per world plus
//! a bounded A* query the Bevy app calls every few seconds per creature.
//!
//! WHY a separate coarse grid (vs. `trails.rs`, which also runs A*): `trails.rs` bakes a
//! handful of routes ONCE at generation and can afford a full-res Dijkstra. This module
//! answers many small queries at runtime, so it must be fast and *bounded* — a 4 m nav
//! grid keeps the search space ~1/16 the fine field, and `max_expand` caps the work so a
//! creature that can't reach its goal falls back to straight-line wandering instead of
//! stalling the frame.
//!
//! Everything is in map-space metres (the same frame as `TreeInstance` / `CreatureSite`),
//! and deterministic: Vec-indexed scratch arrays + a total-ordered heap (f-cost, then node
//! index) — no HashMap iteration-order dependence.

use crate::World;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::f32::consts::SQRT_2;

/// Nav-cell edge length in metres. 4 m is coarse enough to keep queries cheap yet fine
/// enough that a creature threads gaps between lakes/steep bands.
const CELL_SIZE: f32 = 4.0;

/// Impassable above this rise-over-run (matches the `slope` map's tan units). A creature
/// won't scramble a > 0.55 (~29°) face, and it also fences paths off cliff shoulders.
const MAX_SLOPE: f32 = 0.55;

/// Cost sentinel: this cell cannot be entered (water anywhere in it, or too steep).
const IMPASSABLE: u8 = u8::MAX;

/// Base per-cell move cost for flat ground. The spec cost is `1 + slope*6`, but a u8 that
/// small (range ~1..4) quantizes the slope preference almost entirely away — so we scale
/// the whole thing ×10 (base 10, gain 60 == 6×10) to keep the fractional slope penalty
/// alive through the u8 rounding while staying well under the `IMPASSABLE` sentinel.
const PASSABLE_BASE: f32 = 10.0;
const SLOPE_GAIN: f32 = 60.0;
/// Highest storable passable cost (one below the sentinel).
const MAX_COST: f32 = 254.0;

/// 8-connected neighbour offsets. Fixed order → deterministic expansion.
const DIRS: [(i32, i32); 8] =
    [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

/// Coarse nav grid: one `u8` move cost per 4 m cell, `IMPASSABLE` where a creature can't go.
/// Built once from a `&World`; queried many times at runtime.
pub struct PathGrid {
    /// Cells per side (square grid).
    dim: usize,
    /// Metres per nav cell (`CELL_SIZE`).
    cell_size: f32,
    /// `dim*dim` move costs, row-major (`z*dim + x`). `IMPASSABLE` = blocked.
    cost: Vec<u8>,
}

impl PathGrid {
    /// Precompute the nav grid from a finished world. A coarse cell is impassable if ANY
    /// fine cell it covers holds lake water or exceeds `MAX_SLOPE`; otherwise its cost is
    /// `PASSABLE_BASE + SLOPE_GAIN * (max slope in the cell)`.
    pub fn build(world: &World) -> PathGrid {
        let hf = &world.height;
        let ext = hf.extent();
        // ceil so the last (partial) strip of the map still gets a nav cell.
        let dim = (ext / CELL_SIZE).ceil() as usize;
        let mut cost = vec![IMPASSABLE; dim * dim];

        for gz in 0..dim {
            for gx in 0..dim {
                // Fine-field cell span covered by this coarse cell (clamped to the field;
                // the last coarse cell can overhang the edge).
                let wx0 = gx as f32 * CELL_SIZE;
                let wz0 = gz as f32 * CELL_SIZE;
                let fx0 = (wx0 / hf.cell).floor() as usize;
                let fz0 = (wz0 / hf.cell).floor() as usize;
                let fx1 = (((wx0 + CELL_SIZE) / hf.cell).ceil() as usize).min(hf.size);
                let fz1 = (((wz0 + CELL_SIZE) / hf.cell).ceil() as usize).min(hf.size);

                // Off the map (overhang past the field) → leave impassable.
                if fx0 >= hf.size || fz0 >= hf.size || fx0 >= fx1 || fz0 >= fz1 {
                    continue;
                }

                let mut blocked = false;
                let mut smax = 0.0f32;
                'scan: for fz in fz0..fz1 {
                    for fx in fx0..fx1 {
                        let i = fz * hf.size + fx;
                        // Finite water surface = lake covers this cell.
                        if world.water[i].is_finite() {
                            blocked = true;
                            break 'scan;
                        }
                        smax = smax.max(world.slope[i]);
                    }
                }

                if blocked || smax > MAX_SLOPE {
                    continue; // stays IMPASSABLE
                }
                let c = (PASSABLE_BASE + SLOPE_GAIN * smax).clamp(PASSABLE_BASE, MAX_COST);
                cost[gz * dim + gx] = c as u8;
            }
        }

        PathGrid { dim, cell_size: CELL_SIZE, cost }
    }

    /// Cells per side.
    pub fn dims(&self) -> usize {
        self.dim
    }

    /// Nav-cell edge length in metres.
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// True if the cell at a map-space metre position is walkable.
    pub fn passable_at(&self, x: f32, z: f32) -> bool {
        let (gx, gz) = self.clamp_cell((x, z));
        self.passable(gx, gz)
    }

    // --- internals -----------------------------------------------------------

    #[inline]
    fn passable(&self, gx: usize, gz: usize) -> bool {
        self.cost[gz * self.dim + gx] != IMPASSABLE
    }

    /// Bounds-checked passability by signed cell coords (out of range = blocked).
    #[inline]
    fn passable_ci(&self, x: i32, z: i32) -> bool {
        x >= 0 && z >= 0 && (x as usize) < self.dim && (z as usize) < self.dim
            && self.passable(x as usize, z as usize)
    }

    /// Map point → nav-cell coords, clamped into the grid.
    #[inline]
    fn clamp_cell(&self, p: (f32, f32)) -> (usize, usize) {
        let hi = (self.dim - 1) as f32;
        let gx = (p.0 / self.cell_size).floor().clamp(0.0, hi) as usize;
        let gz = (p.1 / self.cell_size).floor().clamp(0.0, hi) as usize;
        (gx, gz)
    }

    /// Cell-centre map point for a linear cell index.
    #[inline]
    fn cell_center(&self, idx: usize) -> (f32, f32) {
        let gx = idx % self.dim;
        let gz = idx / self.dim;
        ((gx as f32 + 0.5) * self.cell_size, (gz as f32 + 0.5) * self.cell_size)
    }

    /// Octile heuristic between two cells, scaled by the minimum per-cell cost so it stays
    /// admissible (no passable step is cheaper than `PASSABLE_BASE`, no diagonal cheaper
    /// than `PASSABLE_BASE * SQRT_2`).
    #[inline]
    fn heuristic(&self, a: usize, b: usize) -> f32 {
        let ax = (a % self.dim) as f32;
        let az = (a / self.dim) as f32;
        let bx = (b % self.dim) as f32;
        let bz = (b / self.dim) as f32;
        let dx = (ax - bx).abs();
        let dz = (az - bz).abs();
        let (hi, lo) = if dx > dz { (dx, dz) } else { (dz, dx) };
        PASSABLE_BASE * (hi + (SQRT_2 - 1.0) * lo)
    }

    /// Snap a map point to a passable nav cell: identity if it's already walkable, else a
    /// spiral search out to 4 cells picking the nearest passable one (ties: earliest in the
    /// fixed scan order → deterministic). `None` if nothing walkable is that close.
    fn snap_passable(&self, p: (f32, f32)) -> Option<usize> {
        let (gx, gz) = self.clamp_cell(p);
        if self.passable(gx, gz) {
            return Some(gz * self.dim + gx);
        }
        for r in 1..=4i32 {
            let mut best: Option<(i32, usize)> = None; // (dist², cell index)
            for dz in -r..=r {
                for dx in -r..=r {
                    // ring shell only (skip the interior already covered by smaller r)
                    if dx.abs() != r && dz.abs() != r {
                        continue;
                    }
                    let nx = gx as i32 + dx;
                    let nz = gz as i32 + dz;
                    if !self.passable_ci(nx, nz) {
                        continue;
                    }
                    let d2 = dx * dx + dz * dz;
                    let idx = nz as usize * self.dim + nx as usize;
                    // Strictly-less keeps the FIRST candidate on ties → deterministic.
                    if best.map_or(true, |(bd, _)| d2 < bd) {
                        best = Some((d2, idx));
                    }
                }
            }
            if let Some((_, idx)) = best {
                return Some(idx);
            }
        }
        None
    }

    /// Supercover line-of-sight test: true iff the segment `a`→`b` crosses ONLY passable
    /// cells. Amanatides–Woo DDA that, at an exact corner crossing, refuses the step when
    /// both orthogonal cells are blocked (forbids diagonal corner-cutting through a wall).
    fn line_passable(&self, a: (f32, f32), b: (f32, f32)) -> bool {
        let cs = self.cell_size;
        let ax = a.0 / cs;
        let az = a.1 / cs;
        let bx = b.0 / cs;
        let bz = b.1 / cs;
        let mut ix = ax.floor() as i32;
        let mut iz = az.floor() as i32;
        let ex = bx.floor() as i32;
        let ez = bz.floor() as i32;

        if !self.passable_ci(ix, iz) {
            return false;
        }
        if ix == ex && iz == ez {
            return true;
        }

        let dx = bx - ax;
        let dz = bz - az;
        let step_x = if dx > 0.0 { 1 } else { -1 };
        let step_z = if dz > 0.0 { 1 } else { -1 };
        let tdx = if dx != 0.0 { (1.0 / dx).abs() } else { f32::INFINITY };
        let tdz = if dz != 0.0 { (1.0 / dz).abs() } else { f32::INFINITY };
        // t to the first cell boundary on each axis.
        let mut tmx = if dx > 0.0 {
            ((ix as f32 + 1.0) - ax) * tdx
        } else if dx < 0.0 {
            (ax - ix as f32) * tdx
        } else {
            f32::INFINITY
        };
        let mut tmz = if dz > 0.0 {
            ((iz as f32 + 1.0) - az) * tdz
        } else if dz < 0.0 {
            (az - iz as f32) * tdz
        } else {
            f32::INFINITY
        };

        // Bounded: a straight segment crosses at most ~2·dim cells; cap guards float edge cases.
        let cap = self.dim as i32 * 4 + 8;
        let mut iter = 0;
        loop {
            iter += 1;
            if iter > cap {
                return false;
            }
            if tmx < tmz {
                ix += step_x;
                tmx += tdx;
            } else if tmz < tmx {
                iz += step_z;
                tmz += tdz;
            } else {
                // Exact corner: reject a diagonal squeeze between two blocked cells.
                if !self.passable_ci(ix + step_x, iz) && !self.passable_ci(ix, iz + step_z) {
                    return false;
                }
                ix += step_x;
                iz += step_z;
                tmx += tdx;
                tmz += tdz;
            }
            if !self.passable_ci(ix, iz) {
                return false;
            }
            if ix == ex && iz == ez {
                return true;
            }
        }
    }
}

/// Heap node: min-f-cost first, ties broken by ascending cell index (deterministic).
struct HNode {
    f: f32,
    idx: u32,
}
impl PartialEq for HNode {
    fn eq(&self, o: &Self) -> bool {
        self.f == o.f && self.idx == o.idx
    }
}
impl Eq for HNode {}
impl Ord for HNode {
    fn cmp(&self, o: &Self) -> Ordering {
        // BinaryHeap is a max-heap: invert so the SMALLEST f (then smallest idx) pops first.
        o.f.partial_cmp(&self.f)
            .unwrap_or(Ordering::Equal)
            .then_with(|| o.idx.cmp(&self.idx))
    }
}
impl PartialOrd for HNode {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Core A* over the nav grid. Returns the cell-index chain start→goal, or `None` if the
/// goal is unreachable OR the `max_expand` node-expansion budget is exhausted first.
fn astar(grid: &PathGrid, start: usize, goal: usize, max_expand: usize) -> Option<Vec<usize>> {
    let n = grid.dim * grid.dim;
    // Fresh scratch per query — at ≤ ~1100² cells these allocs are cheap enough, and it
    // sidesteps any cross-query state that could break determinism.
    let mut g = vec![f32::INFINITY; n];
    let mut came = vec![u32::MAX; n];
    let mut closed = vec![false; n];
    let mut heap = BinaryHeap::new();

    g[start] = 0.0;
    heap.push(HNode { f: grid.heuristic(start, goal), idx: start as u32 });
    let mut expansions = 0usize;
    let dim = grid.dim as i32;

    while let Some(HNode { idx, .. }) = heap.pop() {
        let idx = idx as usize;
        if closed[idx] {
            continue; // stale duplicate; a better entry already settled this cell
        }
        if idx == goal {
            // Walk parents back to the start.
            let mut path = vec![idx];
            let mut cur = idx;
            while came[cur] != u32::MAX {
                cur = came[cur] as usize;
                path.push(cur);
            }
            path.reverse();
            return Some(path);
        }
        closed[idx] = true;
        expansions += 1;
        if expansions > max_expand {
            return None; // budget blown → caller falls back to straight-line wandering
        }

        let x = (idx % grid.dim) as i32;
        let z = (idx / grid.dim) as i32;
        for &(dx, dz) in &DIRS {
            let nx = x + dx;
            let nz = z + dz;
            if nx < 0 || nz < 0 || nx >= dim || nz >= dim {
                continue;
            }
            let (ux, uz) = (nx as usize, nz as usize);
            if !grid.passable(ux, uz) {
                continue;
            }
            let diagonal = dx != 0 && dz != 0;
            if diagonal {
                // No corner-cutting: both shared orthogonal cells must be open.
                if !grid.passable_ci(x + dx, z) || !grid.passable_ci(x, z + dz) {
                    continue;
                }
            }
            let nidx = uz * grid.dim + ux;
            if closed[nidx] {
                continue;
            }
            let step = grid.cost[nidx] as f32 * if diagonal { SQRT_2 } else { 1.0 };
            let ng = g[idx] + step;
            if ng < g[nidx] {
                g[nidx] = ng;
                came[nidx] = idx as u32;
                heap.push(HNode { f: ng + grid.heuristic(nidx, goal), idx: nidx as u32 });
            }
        }
    }
    None
}

/// Greedy line-of-sight string-pulling: from each anchor, skip ahead to the FURTHEST later
/// waypoint still reachable by a clear (only-passable) straight line. Always keeps the exact
/// first/last points. Output length ≤ input length.
fn smooth(grid: &PathGrid, pts: &[(f32, f32)]) -> Vec<(f32, f32)> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len());
    out.push(pts[0]);
    let mut anchor = 0;
    while anchor < pts.len() - 1 {
        // Adjacent waypoint is always reachable (neighbouring passable cells); look past it
        // for the farthest one with clear line of sight.
        let mut furthest = anchor + 1;
        for j in (anchor + 2)..pts.len() {
            if grid.line_passable(pts[anchor], pts[j]) {
                furthest = j;
            }
        }
        out.push(pts[furthest]);
        anchor = furthest;
    }
    out
}

/// A* between two map-space points, bounded by `max_expand` node expansions.
///
/// Returns smoothed map-space waypoints (exact `from` first, exact `to` last, cell centres
/// between) or `None` if unreachable / over budget. If `to` (or `from`) lands on an
/// impassable cell it is snapped to the nearest passable cell within ~4 cells; if nothing
/// walkable is that close, `None`.
pub fn find_path(
    grid: &PathGrid,
    from: (f32, f32),
    to: (f32, f32),
    max_expand: usize,
) -> Option<Vec<(f32, f32)>> {
    let start = grid.snap_passable(from)?;
    let goal = grid.snap_passable(to)?;

    if start == goal {
        // Same nav cell: no routing to do — hand back the caller's exact endpoints.
        return Some(if from == to { vec![from] } else { vec![from, to] });
    }

    let cells = astar(grid, start, goal, max_expand)?;

    // Cell chain → waypoints: exact endpoints, cell centres in between.
    let last = cells.len() - 1;
    let raw: Vec<(f32, f32)> = cells
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if i == 0 {
                from
            } else if i == last {
                to
            } else {
                grid.cell_center(c)
            }
        })
        .collect();

    Some(smooth(grid, &raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::TerrainParams;
    use crate::scatter::ForestParams;
    use crate::{generate, WorldParams};
    use std::collections::VecDeque;

    fn small_world() -> World {
        // Real (if small) map: lowland that floods into lakes + hills, so the nav grid has
        // both passable land and impassable water/steep cells to route around.
        let p = WorldParams {
            terrain: TerrainParams { size: 256, ..Default::default() },
            forest: ForestParams::default(),
            ..Default::default()
        };
        generate(&p, |_, _| {})
    }

    fn first_passable(grid: &PathGrid) -> usize {
        (0..grid.dim * grid.dim).find(|&i| grid.cost[i] != IMPASSABLE).expect("no passable cell")
    }

    /// A*-reachable neighbours of a cell (8-connected, diagonals blocked by corner-cutting)
    /// — the SAME graph `find_path` traverses, so a component/BFS built on it guarantees the
    /// endpoints it yields are genuinely routable.
    fn a_star_neighbors(grid: &PathGrid, c: usize) -> Vec<usize> {
        let x = (c % grid.dim) as i32;
        let z = (c / grid.dim) as i32;
        let mut out = Vec::new();
        for &(dx, dz) in &DIRS {
            let nx = x + dx;
            let nz = z + dz;
            if !grid.passable_ci(nx, nz) {
                continue;
            }
            if dx != 0 && dz != 0
                && (!grid.passable_ci(x + dx, z) || !grid.passable_ci(x, z + dz))
            {
                continue;
            }
            out.push(nz as usize * grid.dim + nx as usize);
        }
        out
    }

    /// Farthest reachable cell from `start` (BFS depth) over the A* graph.
    fn bfs_farthest(grid: &PathGrid, start: usize) -> usize {
        let mut seen = vec![false; grid.dim * grid.dim];
        let mut q = VecDeque::new();
        seen[start] = true;
        q.push_back(start);
        let mut last = start;
        while let Some(c) = q.pop_front() {
            last = c;
            for ni in a_star_neighbors(grid, c) {
                if !seen[ni] {
                    seen[ni] = true;
                    q.push_back(ni);
                }
            }
        }
        last
    }

    /// Two distinct, mutually-reachable cells: a seed in the LARGEST A*-connected component
    /// and the farthest cell reachable from it. Panics if the world is too trivial to route.
    fn pick_endpoints(grid: &PathGrid) -> (usize, usize) {
        let n = grid.dim * grid.dim;
        let mut seen = vec![false; n];
        let mut best_seed = usize::MAX;
        let mut best_size = 0usize;
        for s in 0..n {
            if seen[s] || grid.cost[s] == IMPASSABLE {
                continue;
            }
            // Flood this component.
            let mut stack = vec![s];
            seen[s] = true;
            let mut size = 0usize;
            while let Some(c) = stack.pop() {
                size += 1;
                for ni in a_star_neighbors(grid, c) {
                    if !seen[ni] {
                        seen[ni] = true;
                        stack.push(ni);
                    }
                }
            }
            if size > best_size {
                best_size = size;
                best_seed = s;
            }
        }
        assert!(best_size > 1, "world too trivial for a real path (largest comp {best_size})");
        let goal = bfs_farthest(grid, best_seed);
        (best_seed, goal)
    }

    #[test]
    fn path_between_passable_points_stays_walkable() {
        let w = small_world();
        let grid = PathGrid::build(&w);
        let (start, goal) = pick_endpoints(&grid);
        assert_ne!(start, goal, "world too trivial for a real path");

        let from = grid.cell_center(start);
        let to = grid.cell_center(goal);
        let path = find_path(&grid, from, to, 1_000_000).expect("no path found");

        assert!(path.len() >= 2);
        assert_eq!(path[0], from);
        assert_eq!(*path.last().unwrap(), to);
        // Every waypoint sits on walkable ground.
        for &(x, z) in &path {
            assert!(grid.passable_at(x, z), "waypoint ({x},{z}) not passable");
        }
        // And each hop has clear line of sight (smoothing invariant).
        for seg in path.windows(2) {
            assert!(grid.line_passable(seg[0], seg[1]), "segment crosses a wall");
        }
    }

    #[test]
    fn path_around_water_never_enters_a_lake() {
        let w = small_world();
        assert!(w.lake_count > 0, "test world has no lakes to route around");
        let grid = PathGrid::build(&w);

        // Sanity: some nav cells are blocked (lakes and/or steep faces).
        let blocked = grid.cost.iter().filter(|&&c| c == IMPASSABLE).count();
        assert!(blocked > 0, "nav grid has no impassable cells");

        let (start, goal) = pick_endpoints(&grid);
        let from = grid.cell_center(start);
        let to = grid.cell_center(goal);
        let path = find_path(&grid, from, to, 1_000_000).expect("no path found");

        // Water cells are IMPASSABLE, so a dry path == every waypoint on a passable cell.
        for &(x, z) in &path {
            let (gx, gz) = grid.clamp_cell((x, z));
            assert!(grid.passable(gx, gz), "path waypoint ({x},{z}) is in water/steep");
        }
    }

    #[test]
    fn deterministic() {
        let w = small_world();
        let grid = PathGrid::build(&w);
        let (start, goal) = pick_endpoints(&grid);
        let from = grid.cell_center(start);
        let to = grid.cell_center(goal);

        let a = find_path(&grid, from, to, 1_000_000);
        let b = find_path(&grid, from, to, 1_000_000);
        assert_eq!(a, b, "identical queries gave different paths");
    }

    #[test]
    fn tiny_budget_returns_none() {
        let w = small_world();
        let grid = PathGrid::build(&w);
        let (start, goal) = pick_endpoints(&grid);
        let from = grid.cell_center(start);
        let to = grid.cell_center(goal);

        // A far goal needs many expansions; a 1-node budget cannot reach it.
        assert!(find_path(&grid, from, to, 1).is_none(), "tiny budget should fail");
        // But an ample budget does.
        assert!(find_path(&grid, from, to, 1_000_000).is_some(), "ample budget should succeed");
    }

    #[test]
    fn smoothing_never_grows_the_path() {
        let w = small_world();
        let grid = PathGrid::build(&w);
        let (start, goal) = pick_endpoints(&grid);
        let raw = astar(&grid, start, goal, 1_000_000).expect("no raw path");
        let from = grid.cell_center(start);
        let to = grid.cell_center(goal);
        let smoothed = find_path(&grid, from, to, 1_000_000).expect("no path");
        assert!(
            smoothed.len() <= raw.len(),
            "smoothed {} > raw {}",
            smoothed.len(),
            raw.len()
        );
    }

    #[test]
    fn from_equals_to_is_short() {
        let w = small_world();
        let grid = PathGrid::build(&w);
        let p = grid.cell_center(first_passable(&grid));
        let path = find_path(&grid, p, p, 1_000_000).expect("no path");
        assert!(path.len() >= 1 && path.len() <= 2, "len {}", path.len());
        assert_eq!(path[0], p);
    }
}
