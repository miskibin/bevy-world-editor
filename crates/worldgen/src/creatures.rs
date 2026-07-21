//! Creature spawn-site classification: scans the finished world for the three habitat
//! types a creature system cares about — open meadows, lake shores, and dense forest
//! floor — and returns a deduplicated, deterministic set of sites in map-space metres
//! (the same frame as `TreeInstance`/`PropInstance`).
//!
//! This is a pure post-process over the generated `World` (heightfield + slope + water +
//! trees). It does its own coarse grid scan rather than reusing the scatter grids because
//! the site radii here (10–14 m) are much larger than a crown-spacing cell.

use crate::World;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteKind {
    /// Open, flat, dry, few trees — grazers / rabbits / deer clearings.
    Meadow,
    /// Dry ground a short walk from a lake edge — waterfowl / drinkers.
    LakeShore,
    /// Gentle, dry, closed-canopy ground — den animals / forest fauna.
    ForestFloor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CreatureSite {
    pub kind: SiteKind,
    pub x: f32,
    pub z: f32,
    /// Terrain height at the centre (map metres, y-up).
    pub y: f32,
    pub radius: f32,
}

// --- Tuning ------------------------------------------------------------------
// Slope thresholds are rise-over-run (tan), matching `maps::slope_map`. Scatter rejects
// trees above 0.75 and props above 0.65; a meadow wants genuinely flat ground, forest
// floor can sit on a gentler-than-scatter slope.
const MEADOW_MAX_SLOPE: f32 = 0.30;
const FLOOR_MAX_SLOPE: f32 = 0.45;

const MEADOW_RADIUS: f32 = 14.0;
const SHORE_RADIUS: f32 = 10.0;
const FLOOR_RADIUS: f32 = 12.0;

// Water-proximity band for a shore centre (metres): far enough to be dry standing room,
// close enough to count as "on the shore".
const SHORE_NEAR: f32 = 6.0;
const SHORE_FAR: f32 = 14.0;

// A meadow is defined by open ground; forest floor by canopy. Both counted within 14 m.
const HABITAT_RADIUS: f32 = 14.0;
const MEADOW_MAX_TREES: usize = 4;
const FLOOR_MIN_TREES: usize = 8;

const GRID_STEP: f32 = 24.0; // candidate spacing
const EDGE_MARGIN_FRAC: f32 = 0.08; // skip the outer 8% of the map (island fringe / sea)
const MIN_SITE_SEP: f32 = 30.0; // dedupe distance, per kind
const MAX_PER_KIND: usize = 40;

/// Uniform bucket grid over tree XY so a radius query only visits nearby trees instead of
/// all ~10k+ instances. Bucket pitch is chosen so a habitat-radius query touches a small
/// fixed neighbourhood.
struct TreeBuckets {
    cell: f32,
    cols: usize,
    cells: Vec<Vec<(f32, f32)>>,
}

impl TreeBuckets {
    fn build(world: &World) -> Self {
        let ext = world.height.extent();
        let cell = 20.0f32;
        let cols = ((ext / cell).ceil() as usize).max(1);
        let mut cells = vec![Vec::<(f32, f32)>::new(); cols * cols];
        for t in &world.trees {
            let bx = ((t.x / cell) as usize).min(cols - 1);
            let bz = ((t.z / cell) as usize).min(cols - 1);
            cells[bz * cols + bx].push((t.x, t.z));
        }
        TreeBuckets { cell, cols, cells }
    }

    /// Count trees whose centre is within `r` metres of (cx, cz).
    fn count_within(&self, cx: f32, cz: f32, r: f32) -> usize {
        let r2 = r * r;
        let bx0 = (((cx - r) / self.cell).floor() as i32).max(0);
        let bx1 = (((cx + r) / self.cell).floor() as i32).min(self.cols as i32 - 1);
        let bz0 = (((cz - r) / self.cell).floor() as i32).max(0);
        let bz1 = (((cz + r) / self.cell).floor() as i32).min(self.cols as i32 - 1);
        let mut n = 0;
        for bz in bz0..=bz1 {
            for bx in bx0..=bx1 {
                for &(tx, tz) in &self.cells[bz as usize * self.cols + bx as usize] {
                    let dx = tx - cx;
                    let dz = tz - cz;
                    if dx * dx + dz * dz <= r2 {
                        n += 1;
                    }
                }
            }
        }
        n
    }
}

/// Slope-map value at world metres (clamped to the field).
fn slope_at(world: &World, wx: f32, wz: f32) -> f32 {
    let hf = &world.height;
    let ix = ((wx / hf.cell) as usize).min(hf.size - 1);
    let iz = ((wz / hf.cell) as usize).min(hf.size - 1);
    world.slope[iz * hf.size + ix]
}

/// True if the cell at world metres carries a (finite) lake surface.
fn is_water_at(world: &World, wx: f32, wz: f32) -> bool {
    let hf = &world.height;
    let ix = ((wx / hf.cell) as usize).min(hf.size - 1);
    let iz = ((wz / hf.cell) as usize).min(hf.size - 1);
    world.water[iz * hf.size + ix].is_finite()
}

/// Max slope sampled over a disk of radius `r` (coarse 3 m lattice — lakes/steep bands are
/// far larger than the step, so this is a faithful patch maximum without scanning every cell).
fn max_slope_in_patch(world: &World, cx: f32, cz: f32, r: f32) -> f32 {
    let step = 3.0f32;
    let r2 = r * r;
    let mut mx = 0.0f32;
    let mut dz = -r;
    while dz <= r {
        let mut dx = -r;
        while dx <= r {
            if dx * dx + dz * dz <= r2 {
                mx = mx.max(slope_at(world, cx + dx, cz + dz));
            }
            dx += step;
        }
        dz += step;
    }
    mx
}

/// Distance (metres) to the nearest water cell within `max_r`, or `None` if all dry.
fn nearest_water_dist(world: &World, cx: f32, cz: f32, max_r: f32) -> Option<f32> {
    let hf = &world.height;
    let size = hf.size as i32;
    let cxi = (cx / hf.cell) as i32;
    let czi = (cz / hf.cell) as i32;
    let rc = (max_r / hf.cell).ceil() as i32;
    let mut best2 = i32::MAX;
    for dz in -rc..=rc {
        for dx in -rc..=rc {
            let nx = cxi + dx;
            let nz = czi + dz;
            if nx < 0 || nz < 0 || nx >= size || nz >= size {
                continue;
            }
            if world.water[(nz * size + nx) as usize].is_finite() {
                let d2 = dx * dx + dz * dz;
                if d2 < best2 {
                    best2 = d2;
                }
            }
        }
    }
    if best2 == i32::MAX {
        None
    } else {
        // Cells are square (`cell` metres); convert the cell-space distance to metres.
        Some((best2 as f32).sqrt() * hf.cell)
    }
}

/// A candidate carries a flatness score so the dedupe pass can keep the flattest sites.
struct Candidate {
    site: CreatureSite,
    flatness: f32, // lower = flatter = preferred
}

/// Greedy spatial dedupe: keep flattest-first, drop anything within `MIN_SITE_SEP` of a
/// kept site, cap at `MAX_PER_KIND`. Input order is the deterministic grid scan; the sort
/// is stable so ties resolve by scan order (no nondeterminism).
fn dedupe_and_cap(mut cands: Vec<Candidate>) -> Vec<CreatureSite> {
    cands.sort_by(|a, b| a.flatness.partial_cmp(&b.flatness).unwrap_or(std::cmp::Ordering::Equal));
    let sep2 = MIN_SITE_SEP * MIN_SITE_SEP;
    let mut kept: Vec<CreatureSite> = Vec::new();
    for c in cands {
        if kept.len() >= MAX_PER_KIND {
            break;
        }
        let ok = kept.iter().all(|k| {
            let dx = k.x - c.site.x;
            let dz = k.z - c.site.z;
            dx * dx + dz * dz >= sep2
        });
        if ok {
            kept.push(c.site);
        }
    }
    kept
}

/// Classify creature spawn sites over a finished world. Deterministic: same `World` in →
/// identical `Vec` out (grid scan + stable sort + ordered vectors, no hashing).
pub fn creature_sites(world: &World) -> Vec<CreatureSite> {
    let hf = &world.height;
    let ext = hf.extent();
    let margin = ext * EDGE_MARGIN_FRAC;
    let buckets = TreeBuckets::build(world);

    let mut meadow_c = Vec::new();
    let mut shore_c = Vec::new();
    let mut floor_c = Vec::new();

    let mut cx = margin;
    while cx <= ext - margin {
        let mut cz = margin;
        while cz <= ext - margin {
            let y = hf.sample_world(cx, cz);
            // Reused per-candidate metrics.
            let patch_slope = max_slope_in_patch(world, cx, cz, HABITAT_RADIUS);
            let water_near = nearest_water_dist(world, cx, cz, SHORE_FAR);
            let dry_patch = water_near.map_or(true, |d| d > HABITAT_RADIUS);
            let tree_n = buckets.count_within(cx, cz, HABITAT_RADIUS);
            let centre_dry = !is_water_at(world, cx, cz);

            // Meadow: flat, dry patch, sparse trees.
            if centre_dry
                && dry_patch
                && patch_slope <= MEADOW_MAX_SLOPE
                && tree_n < MEADOW_MAX_TREES
            {
                meadow_c.push(Candidate {
                    site: CreatureSite { kind: SiteKind::Meadow, x: cx, z: cz, y, radius: MEADOW_RADIUS },
                    flatness: patch_slope,
                });
            }

            // Forest floor: gentle, dry patch, dense canopy.
            if centre_dry
                && dry_patch
                && patch_slope <= FLOOR_MAX_SLOPE
                && tree_n >= FLOOR_MIN_TREES
            {
                floor_c.push(Candidate {
                    site: CreatureSite { kind: SiteKind::ForestFloor, x: cx, z: cz, y, radius: FLOOR_RADIUS },
                    flatness: patch_slope,
                });
            }

            // Lake shore: dry centre a short walk (6–14 m) from water.
            if centre_dry {
                if let Some(d) = water_near {
                    if d >= SHORE_NEAR && d <= SHORE_FAR {
                        shore_c.push(Candidate {
                            site: CreatureSite { kind: SiteKind::LakeShore, x: cx, z: cz, y, radius: SHORE_RADIUS },
                            flatness: slope_at(world, cx, cz),
                        });
                    }
                }
            }

            cz += GRID_STEP;
        }
        cx += GRID_STEP;
    }

    // Fixed output order: meadows, then shores, then forest floor — each internally
    // deduped/flattest-first. Deterministic.
    let mut out = dedupe_and_cap(meadow_c);
    out.extend(dedupe_and_cap(shore_c));
    out.extend(dedupe_and_cap(floor_c));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorldParams, generate};
    use crate::heightfield::TerrainParams;
    use crate::scatter::ForestParams;

    fn small_world() -> World {
        // Small but real map: big enough to hold lowland (flooded → lakes), forest, and
        // clearings so all three site kinds have a chance to appear.
        let p = WorldParams {
            terrain: TerrainParams { size: 256, ..Default::default() },
            forest: ForestParams::default(),
            ..Default::default()
        };
        generate(&p, |_, _| {})
    }

    #[test]
    fn sites_non_empty() {
        let w = small_world();
        let sites = creature_sites(&w);
        assert!(!sites.is_empty(), "no creature sites classified");
    }

    #[test]
    fn meadows_are_dry_and_gentle() {
        let w = small_world();
        for s in creature_sites(&w).iter().filter(|s| s.kind == SiteKind::Meadow) {
            assert!(!is_water_at(&w, s.x, s.z), "meadow centre in water");
            let slope = max_slope_in_patch(&w, s.x, s.z, HABITAT_RADIUS);
            assert!(slope <= MEADOW_MAX_SLOPE + 1e-4, "meadow too steep: {slope}");
        }
    }

    #[test]
    fn shores_are_near_water() {
        let w = small_world();
        for s in creature_sites(&w).iter().filter(|s| s.kind == SiteKind::LakeShore) {
            assert!(!is_water_at(&w, s.x, s.z), "shore centre submerged");
            let d = nearest_water_dist(&w, s.x, s.z, SHORE_FAR + 1.0)
                .expect("shore has no water in range");
            assert!(d >= SHORE_NEAR - 1e-3 && d <= SHORE_FAR + 1e-3, "shore water dist {d}");
        }
    }

    #[test]
    fn forest_floor_has_canopy() {
        let w = small_world();
        let buckets = TreeBuckets::build(&w);
        for s in creature_sites(&w).iter().filter(|s| s.kind == SiteKind::ForestFloor) {
            let n = buckets.count_within(s.x, s.z, HABITAT_RADIUS);
            assert!(n >= FLOOR_MIN_TREES, "forest floor too sparse: {n} trees");
        }
    }

    #[test]
    fn deterministic() {
        let w = small_world();
        let a = creature_sites(&w);
        let b = creature_sites(&w);
        assert_eq!(a, b, "classification not deterministic");
    }

    #[test]
    fn all_sites_in_bounds_and_capped() {
        let w = small_world();
        let ext = w.height.extent();
        let sites = creature_sites(&w);
        for s in &sites {
            assert!(s.x >= 0.0 && s.x <= ext, "x out of bounds: {}", s.x);
            assert!(s.z >= 0.0 && s.z <= ext, "z out of bounds: {}", s.z);
            assert!(s.y.is_finite(), "y not finite");
            assert!(s.radius > 0.0);
        }
        for kind in [SiteKind::Meadow, SiteKind::LakeShore, SiteKind::ForestFloor] {
            let c = sites.iter().filter(|s| s.kind == kind).count();
            assert!(c <= MAX_PER_KIND, "{kind:?} exceeded cap: {c}");
        }
    }
}
