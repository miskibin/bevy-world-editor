//! Hydraulic (droplet) + thermal erosion. This pass is what makes noise terrain read as
//! real: gullies, V-valleys, alluvial fans, scree slopes. Classic particle formulation
//! (Beyer / Lague): each droplet walks downhill carrying sediment; erode when under
//! capacity, deposit when over. A flow map (water passage per cell) falls out for free
//! and later drives moisture/vegetation.

use crate::heightfield::HeightField;
use crate::rng::Rng;

#[derive(Clone, Copy)]
pub struct ErosionParams {
    pub droplets: u32,
    /// 0 = follows gradient exactly (jittery), 1 = never turns. ~0.05 natural.
    pub inertia: f32,
    pub capacity: f32,
    pub min_capacity: f32,
    pub deposit_rate: f32,
    pub erode_rate: f32,
    pub evaporate: f32,
    pub gravity: f32,
    pub max_steps: u32,
    /// Erosion brush radius in cells — spreads digging so tracks aren't 1-cell scratches.
    pub radius: i32,
    /// Thermal relaxation passes after the hydraulic pass.
    pub thermal_passes: u32,
    /// Talus threshold: height difference per cell above which material slides.
    pub talus: f32,
}

impl Default for ErosionParams {
    fn default() -> Self {
        ErosionParams {
            // Sized for the 1024² map — ~2.4× the per-cell erosion of the 2 km first cut,
            // which is what carves the valley detail the smaller map trades size for.
            droplets: 240_000,
            inertia: 0.05,
            capacity: 4.0,
            min_capacity: 0.01,
            deposit_rate: 0.30,
            erode_rate: 0.30,
            evaporate: 0.015,
            gravity: 4.0,
            max_steps: 72,
            radius: 3,
            thermal_passes: 3,
            talus: 0.72,
        }
    }
}

/// Precomputed normalised brush: (dx, dz, weight) within `radius`.
fn build_brush(radius: i32) -> Vec<(i32, i32, f32)> {
    let mut brush = Vec::new();
    let mut sum = 0.0;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let d = ((dx * dx + dz * dz) as f32).sqrt();
            if d <= radius as f32 {
                let w = 1.0 - d / radius as f32;
                brush.push((dx, dz, w));
                sum += w;
            }
        }
    }
    for b in &mut brush {
        b.2 /= sum;
    }
    brush
}

/// Runs droplet erosion in place. Returns the flow map (water passage per cell).
pub fn erode(
    hf: &mut HeightField,
    p: &ErosionParams,
    seed: u32,
    mut progress: impl FnMut(f32),
) -> Vec<f32> {
    let size = hf.size;
    let mut flow = vec![0.0f32; size * size];
    let brush = build_brush(p.radius);
    let mut rng = Rng::new(seed ^ 0x00E0_5107);
    let margin = (p.radius + 1) as f32;
    let hi = size as f32 - 2.0 - margin as f32;

    for i in 0..p.droplets {
        let mut x = rng.range(margin, hi);
        let mut z = rng.range(margin, hi);
        let mut dx = 0.0f32;
        let mut dz = 0.0f32;
        let mut speed = 1.0f32;
        let mut water = 1.0f32;
        let mut sediment = 0.0f32;

        for _ in 0..p.max_steps {
            let (gx, gz) = hf.gradient(x, z);
            dx = dx * p.inertia - gx * (1.0 - p.inertia);
            dz = dz * p.inertia - gz * (1.0 - p.inertia);
            let len = (dx * dx + dz * dz).sqrt();
            if len < 1e-6 {
                break; // flat pit — droplet dies (deposits below)
            }
            dx /= len;
            dz /= len;
            let old_h = hf.sample(x, z);
            let ix = x as usize;
            let iz = z as usize;
            flow[iz * size + ix] += water;
            x += dx;
            z += dz;
            if x < margin || x > hi || z < margin || z > hi {
                break;
            }
            let new_h = hf.sample(x, z);
            let dh = new_h - old_h;

            let cap = (-dh).max(p.min_capacity) * speed * water * p.capacity;
            if sediment > cap || dh > 0.0 {
                // Deposit — fill the pit fully if we walked uphill, else a fraction.
                let amount = if dh > 0.0 {
                    sediment.min(dh)
                } else {
                    (sediment - cap) * p.deposit_rate
                };
                sediment -= amount;
                // Bilinear-spread deposit at the OLD position.
                let fx = (x - dx) - ix as f32;
                let fz = (z - dz) - iz as f32;
                let i00 = iz * size + ix;
                hf.h[i00] += amount * (1.0 - fx) * (1.0 - fz);
                hf.h[i00 + 1] += amount * fx * (1.0 - fz);
                hf.h[i00 + size] += amount * (1.0 - fx) * fz;
                hf.h[i00 + size + 1] += amount * fx * fz;
            } else {
                let amount = ((cap - sediment) * p.erode_rate).min(-dh);
                for &(bx, bz, w) in &brush {
                    let nx = ix as i32 + bx;
                    let nz = iz as i32 + bz;
                    let ni = nz as usize * size + nx as usize;
                    let d = amount * w;
                    hf.h[ni] -= d;
                }
                sediment += amount;
            }

            speed = (speed * speed + dh.abs() * p.gravity).sqrt();
            water *= 1.0 - p.evaporate;
            if water < 0.01 {
                break;
            }
        }

        if i % 16384 == 0 {
            progress(i as f32 / p.droplets as f32);
        }
    }

    // Defensive: erosion math should never produce non-finite cells, but a NaN in the
    // heightfield poisons meshing silently — clamp here and let the unit test scream.
    for v in hf.h.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
        }
    }
    progress(1.0);
    flow
}

/// Thermal erosion: material above the talus angle slides to the lowest neighbour.
/// Gives crags scree aprons and softens droplet scratch artifacts.
pub fn thermal(hf: &mut HeightField, p: &ErosionParams, mut progress: impl FnMut(f32)) {
    let size = hf.size;
    let mut delta = vec![0.0f32; size * size];
    for pass in 0..p.thermal_passes {
        delta.iter_mut().for_each(|d| *d = 0.0);
        for z in 1..size - 1 {
            for x in 1..size - 1 {
                let h = hf.get(x, z);
                let mut best_drop = 0.0;
                let mut best: Option<usize> = None;
                for (nx, nz) in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                    let drop = h - hf.get(nx, nz);
                    if drop > best_drop {
                        best_drop = drop;
                        best = Some(nz * size + nx);
                    }
                }
                if let Some(ni) = best {
                    if best_drop > p.talus {
                        let moved = (best_drop - p.talus) * 0.25;
                        delta[z * size + x] -= moved;
                        delta[ni] += moved;
                    }
                }
            }
        }
        for (h, d) in hf.h.iter_mut().zip(delta.iter()) {
            *h += d;
        }
        progress((pass + 1) as f32 / p.thermal_passes as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::{TerrainParams, generate_base};

    fn eroded() -> (HeightField, Vec<f32>) {
        let tp = TerrainParams { size: 128, ..Default::default() };
        let mut hf = generate_base(&tp, |_| {});
        let ep = ErosionParams { droplets: 4000, ..Default::default() };
        let flow = erode(&mut hf, &ep, 42, |_| {});
        thermal(&mut hf, &ep, |_| {});
        (hf, flow)
    }

    #[test]
    fn erosion_finite_and_bounded() {
        let (hf, flow) = eroded();
        for &v in &hf.h {
            assert!(v.is_finite());
            assert!(v > -50.0 && v < 400.0, "h={v}");
        }
        for &f in &flow {
            assert!(f.is_finite() && f >= 0.0);
        }
    }

    #[test]
    fn erosion_deterministic() {
        let (a, fa) = eroded();
        let (b, fb) = eroded();
        assert_eq!(a.h, b.h);
        assert_eq!(fa, fb);
    }

    #[test]
    fn erosion_actually_changes_terrain() {
        let tp = TerrainParams { size: 128, ..Default::default() };
        let base = generate_base(&tp, |_| {});
        let (hf, flow) = eroded();
        let diff: f32 = base.h.iter().zip(&hf.h).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 1.0, "erosion did nothing, diff={diff}");
        assert!(flow.iter().any(|&f| f > 0.0), "flow map empty");
    }
}
