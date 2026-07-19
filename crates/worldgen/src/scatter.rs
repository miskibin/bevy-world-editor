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
            density: 0.75,
            species_weights: [1.0, 1.0, 1.0, 1.0],
            treeline: 215.0,
            max_slope: 0.75,
            water_level: 8.0,
            spacing: 5.2,
        }
    }
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

    fn world() -> (HeightField, Vec<f32>, Vec<f32>) {
        let tp = TerrainParams { size: 256, ..Default::default() };
        let mut hf = generate_base(&tp, |_| {});
        let ep = ErosionParams { droplets: 8000, ..Default::default() };
        let flow = erode(&mut hf, &ep, 1, |_| {});
        let slope = slope_map(&hf);
        let moist = moisture_map(&hf, &flow, 8.0);
        (hf, slope, moist)
    }

    #[test]
    fn scatter_deterministic_and_valid() {
        let (hf, slope, moist) = world();
        let p = ForestParams::default();
        let a = scatter(&hf, &slope, &moist, &p);
        let b = scatter(&hf, &slope, &moist, &p);
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
        let (hf, slope, moist) = world();
        let lo = scatter(&hf, &slope, &moist, &ForestParams { density: 0.2, ..Default::default() });
        let hi = scatter(&hf, &slope, &moist, &ForestParams { density: 0.9, ..Default::default() });
        assert!(hi.len() > lo.len() * 2, "hi={} lo={}", hi.len(), lo.len());
    }

    #[test]
    fn zero_weights_yield_nothing() {
        let (hf, slope, moist) = world();
        let none = scatter(
            &hf,
            &slope,
            &moist,
            &ForestParams { species_weights: [0.0; 4], ..Default::default() },
        );
        assert!(none.is_empty());
    }
}
