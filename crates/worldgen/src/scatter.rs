//! Forest placement: deterministic jittered-grid scatter with site-based species mix.

use crate::heightfield::HeightField;
use crate::noise::fbm;
use crate::rng::{Rng, lowbias32};
use crate::tree::Species;

#[derive(Clone, Copy)]
pub struct ForestParams {
    pub seed: u32,
    /// 0..1 overall stocking, 1 = closed-canopy where the site allows.
    pub density: f32,
    /// Relative preference weights: [pine, spruce, broadleaf, birch].
    pub species_weights: [f32; 4],
    /// Metres — above this trees thin out and stop.
    pub treeline: f32,
    /// Rise-over-run above which slopes go bare.
    pub max_slope: f32,
    pub water_level: f32,
    /// Grid pitch in metres between candidate sites (~crown spacing).
    pub spacing: f32,
}

impl Default for ForestParams {
    fn default() -> Self {
        ForestParams {
            seed: 20260719,
            density: 0.72,
            species_weights: [1.0, 1.0, 1.0, 1.0],
            treeline: 215.0,
            max_slope: 0.75,
            water_level: 8.0,
            // Fewer-but-prettier: wider spacing frees the frame budget for richer LOD0s.
            spacing: 5.4,
        }
    }
}

#[derive(Clone, Copy)]
pub struct RockInstance {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Base radius in metres.
    pub scale: f32,
    pub yaw: f32,
    /// Mesh variant index.
    pub kind: u8,
}

/// Boulders + outcrops: slope-loving, noise-clustered, extra along lake shores.
pub fn scatter_rocks(
    hf: &HeightField,
    slope: &[f32],
    water: &[f32],
    seed: u32,
) -> Vec<RockInstance> {
    let ext = hf.extent();
    let spacing = 7.0f32;
    let n = (ext / spacing) as i32;
    let mut out = Vec::new();
    for gz in 1..n - 1 {
        for gx in 1..n - 1 {
            let site_seed = lowbias32(
                (gx as u32)
                    .wrapping_mul(0x9E37_79B9)
                    .wrapping_add((gz as u32).wrapping_mul(0x0068_E31D))
                    .wrapping_add(seed ^ 0x00C0_FFEE),
            );
            let mut rng = Rng::new(site_seed);
            let wx = (gx as f32 + rng.f32()) * spacing;
            let wz = (gz as f32 + rng.f32()) * spacing;
            let ix = (wx / hf.cell) as usize;
            let iz = (wz / hf.cell) as usize;
            let i = iz * hf.size + ix;
            if water[i].is_finite() {
                continue; // submerged
            }
            let s = slope[i];
            // Clustered fields (talus, outcrop bands) rather than uniform sprinkle.
            let cluster = fbm(wx / 88.0, wz / 88.0, 3, seed.wrapping_add(77));
            let mut p = 0.012 + smoothstep_f(0.30, 0.85, s) * 0.22;
            p *= 0.35 + smoothstep_f(0.45, 0.75, cluster) * 2.6;
            // Shore rocks: dry ground within ~1.5 m above a nearby lake surface.
            let shore = neighborhood_water(hf, water, ix, iz);
            if let Some(surf) = shore {
                if hf.h[i] - surf < 1.5 {
                    p += 0.10;
                }
            }
            if !rng.chance(p) {
                continue;
            }
            let big = rng.chance(0.07);
            out.push(RockInstance {
                x: wx,
                y: hf.sample_world(wx, wz),
                z: wz,
                scale: if big { rng.range(2.0, 3.8) } else { rng.range(0.45, 1.7) },
                yaw: rng.range(0.0, std::f32::consts::TAU),
                kind: (rng.next_u32() % 4) as u8,
            });
        }
    }
    out
}

fn smoothstep_f(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Highest lake surface within a 6-cell box, if any.
fn neighborhood_water(hf: &HeightField, water: &[f32], x: usize, z: usize) -> Option<f32> {
    let mut best = f32::NEG_INFINITY;
    for dz in -6i32..=6 {
        for dx in -6i32..=6 {
            let nx = x as i32 + dx;
            let nz = z as i32 + dz;
            if nx < 0 || nz < 0 || nx >= hf.size as i32 || nz >= hf.size as i32 {
                continue;
            }
            let w = water[nz as usize * hf.size + nx as usize];
            if w.is_finite() {
                best = best.max(w);
            }
        }
    }
    best.is_finite().then_some(best)
}

/// Undergrowth prop kinds.
pub const PROP_BUSH_BROADLEAF: u8 = 0;
pub const PROP_BUSH_BIRCH: u8 = 1;
pub const PROP_LOG: u8 = 2;
pub const PROP_STUMP: u8 = 3;

#[derive(Clone, Copy)]
pub struct PropInstance {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub scale: f32,
    pub kind: u8,
}

/// Bushes cluster at forest edges (the clearing-noise boundary band), logs/stumps lie
/// sparsely inside the woods. Trails stay clear.
pub fn scatter_props(
    hf: &HeightField,
    slope: &[f32],
    moisture: &[f32],
    water: &[f32],
    trails: &[f32],
    p: &ForestParams,
) -> Vec<PropInstance> {
    let ext = hf.extent();
    let spacing = 4.4f32;
    let n = (ext / spacing) as i32;
    let clearing_freq = 1.0 / 260.0;
    let mut out = Vec::new();
    for gz in 1..n - 1 {
        for gx in 1..n - 1 {
            let site_seed = lowbias32(
                (gx as u32)
                    .wrapping_mul(0x1234_7A31)
                    .wrapping_add((gz as u32).wrapping_mul(0x0068_E31D))
                    .wrapping_add(p.seed ^ 0x00B0_5511),
            );
            let mut rng = Rng::new(site_seed);
            let wx = (gx as f32 + rng.f32()) * spacing;
            let wz = (gz as f32 + rng.f32()) * spacing;
            let ix = ((wx / hf.cell) as usize).min(hf.size - 1);
            let iz = ((wz / hf.cell) as usize).min(hf.size - 1);
            let i = iz * hf.size + ix;
            let h = hf.h[i];
            if water[i].is_finite()
                || h < p.water_level + 0.6
                || slope[i] > 0.65
                || trails[i] > 0.30
                || h > p.treeline
            {
                continue;
            }
            let clearing = fbm(wx * clearing_freq, wz * clearing_freq, 3, p.seed.wrapping_add(555));
            let thr = 0.40 - 0.25 * p.density;
            let in_forest = clearing > thr;
            // Edge band: near the clearing threshold from either side.
            let edge = 1.0 - ((clearing - thr).abs() / 0.06).min(1.0);
            let m = moisture[i];
            let bush_p = edge * 0.35 + if in_forest { 0.015 } else { 0.030 * m };
            let log_p = if in_forest { 0.020 } else { 0.0 };
            let r = rng.f32();
            let kind = if r < bush_p {
                if rng.chance(0.6) { PROP_BUSH_BROADLEAF } else { PROP_BUSH_BIRCH }
            } else if r < bush_p + log_p {
                if rng.chance(0.72) { PROP_LOG } else { PROP_STUMP }
            } else {
                continue;
            };
            out.push(PropInstance {
                x: wx,
                y: hf.sample_world(wx, wz),
                z: wz,
                yaw: rng.range(0.0, std::f32::consts::TAU),
                scale: rng.range(0.75, 1.45),
                kind,
            });
        }
    }
    out
}

#[derive(Clone, Copy)]
pub struct TreeInstance {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub species: Species,
    pub variant: u8,
    pub yaw: f32,
    pub scale: f32,
    /// -1..1, small albedo hue jitter applied at render time.
    pub hue: f32,
}

/// Sample a map (size×size, cell metres) at world metres, clamped.
fn sample_map(map: &[f32], size: usize, cell: f32, wx: f32, wz: f32) -> f32 {
    let x = (wx / cell).clamp(0.0, (size - 1) as f32) as usize;
    let z = (wz / cell).clamp(0.0, (size - 1) as f32) as usize;
    map[z * size + x]
}

pub fn scatter(
    hf: &HeightField,
    slope: &[f32],
    moisture: &[f32],
    water: &[f32],
    trails: &[f32],
    p: &ForestParams,
) -> Vec<TreeInstance> {
    let ext = hf.extent();
    let n = (ext / p.spacing) as i32;
    let mut out = Vec::new();
    let clearing_freq = 1.0 / 260.0;
    for gz in 1..n - 1 {
        for gx in 1..n - 1 {
            // Per-site RNG keyed on grid coords — stable as parameters change elsewhere.
            let site_seed = lowbias32(
                (gx as u32)
                    .wrapping_mul(0x0068_E31D)
                    .wrapping_add((gz as u32).wrapping_mul(0x02E1_B213))
                    .wrapping_add(p.seed),
            );
            let mut rng = Rng::new(site_seed);
            let wx = (gx as f32 + rng.f32()) * p.spacing;
            let wz = (gz as f32 + rng.f32()) * p.spacing;
            let h = hf.sample_world(wx, wz);
            let s = sample_map(slope, hf.size, hf.cell, wx, wz);
            let m = sample_map(moisture, hf.size, hf.cell, wx, wz);

            if h < p.water_level + 0.6 || s > p.max_slope {
                continue;
            }
            // No trees in (or right at the rim of) mountain lakes, none on the trails.
            let wi = ((wz / hf.cell) as usize).min(hf.size - 1) * hf.size
                + ((wx / hf.cell) as usize).min(hf.size - 1);
            if water[wi].is_finite() && h < water[wi] + 0.4 {
                continue;
            }
            if trails[wi] > 0.40 {
                continue;
            }
            // Treeline: thin over the last 25 m, hard stop above.
            let alt_ok = ((p.treeline - h) / 25.0).clamp(0.0, 1.0);
            if alt_ok <= 0.0 {
                continue;
            }
            // Meadow clearings from low-frequency noise, opened wider at low density.
            let clearing =
                fbm(wx * clearing_freq, wz * clearing_freq, 3, p.seed.wrapping_add(555));
            if clearing < 0.40 - 0.25 * p.density {
                continue;
            }
            // Stocking: denser on moist, gentler ground.
            let stock = p.density * alt_ok * (0.45 + 0.55 * m) * (1.0 - (s / p.max_slope) * 0.5);
            if !rng.chance(stock) {
                continue;
            }

            // Site-modified species preference:
            //   pine   — dry, sandy, ridge sites
            //   spruce — moist and/or high sites
            //   beech  — mid-elevation, mesic slopes
            //   birch  — wet lowland, pioneer in clearings' edges
            let elev = (h / p.treeline).clamp(0.0, 1.0);
            let w = [
                p.species_weights[0] * (1.2 - m) * (0.4 + elev),
                p.species_weights[1] * (0.4 + 0.8 * m) * (0.5 + elev * 0.9),
                p.species_weights[2] * (0.5 + 0.7 * m) * (1.1 - elev).max(0.05),
                p.species_weights[3] * (0.3 + m) * (1.0 - elev * 0.6),
            ];
            let total: f32 = w.iter().sum();
            if total <= 0.0 {
                continue;
            }
            let mut pick = rng.f32() * total;
            let mut species = Species::Pine;
            for (i, sp) in [Species::Pine, Species::Spruce, Species::Broadleaf, Species::Birch]
                .into_iter()
                .enumerate()
            {
                if pick < w[i] {
                    species = sp;
                    break;
                }
                pick -= w[i];
            }

            out.push(TreeInstance {
                x: wx,
                y: h,
                z: wz,
                species,
                variant: (rng.next_u32() % 4) as u8,
                yaw: rng.range(0.0, std::f32::consts::TAU),
                scale: rng.range(0.82, 1.28),
                hue: rng.signed(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erosion::{ErosionParams, erode};
    use crate::heightfield::{TerrainParams, generate_base};
    use crate::maps::{moisture_map, slope_map};

    fn world() -> (HeightField, Vec<f32>, Vec<f32>, Vec<f32>) {
        let tp = TerrainParams { size: 256, ..Default::default() };
        let mut hf = generate_base(&tp, |_| {});
        let ep = ErosionParams { droplets: 8000, ..Default::default() };
        let flow = erode(&mut hf, &ep, 1, |_| {});
        let slope = slope_map(&hf);
        let water = vec![f32::NEG_INFINITY; 256 * 256];
        let moist = moisture_map(&hf, &flow, &water, 8.0);
        (hf, slope, moist, water)
    }

    fn no_trails() -> Vec<f32> {
        vec![0.0; 256 * 256]
    }

    #[test]
    fn scatter_deterministic_and_valid() {
        let (hf, slope, moist, water) = world();
        let p = ForestParams::default();
        let a = scatter(&hf, &slope, &moist, &water, &no_trails(), &p);
        let b = scatter(&hf, &slope, &moist, &water, &no_trails(), &p);
        assert_eq!(a.len(), b.len());
        assert!(!a.is_empty(), "no trees scattered");
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.x, y.x);
            assert_eq!(x.z, y.z);
        }
        for t in &a {
            assert!(t.y >= p.water_level, "tree under water");
            let s = sample_map(&slope, hf.size, hf.cell, t.x, t.z);
            assert!(s <= p.max_slope, "tree on a cliff");
            assert!(t.scale > 0.5 && t.scale < 2.0);
        }
    }

    #[test]
    fn density_scales_count() {
        let (hf, slope, moist, water) = world();
        let lo = scatter(
            &hf, &slope, &moist, &water, &no_trails(),
            &ForestParams { density: 0.2, ..Default::default() },
        );
        let hi = scatter(
            &hf, &slope, &moist, &water, &no_trails(),
            &ForestParams { density: 0.9, ..Default::default() },
        );
        assert!(hi.len() > lo.len() * 2, "hi={} lo={}", hi.len(), lo.len());
    }

    #[test]
    fn zero_weights_yield_nothing() {
        let (hf, slope, moist, water) = world();
        let none = scatter(
            &hf,
            &slope,
            &moist,
            &water,
            &no_trails(),
            &ForestParams { species_weights: [0.0; 4], ..Default::default() },
        );
        assert!(none.is_empty());
    }

    #[test]
    fn rocks_deterministic_and_dry() {
        let (hf, slope, _moist, water) = world();
        let a = scatter_rocks(&hf, &slope, &water, 5);
        let b = scatter_rocks(&hf, &slope, &water, 5);
        assert_eq!(a.len(), b.len());
        assert!(!a.is_empty(), "no rocks scattered");
        for r in &a {
            assert!(r.scale > 0.2 && r.scale < 5.0);
            assert!(r.y.is_finite());
        }
    }
}
