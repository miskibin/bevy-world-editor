//! Heightfield storage + the base (pre-erosion) terrain generator.

use crate::noise::{fbm, ridged, smoothstep, warped_fbm};

#[derive(Clone)]
pub struct HeightField {
    /// Cells per side (the field is square).
    pub size: usize,
    /// Metres per cell.
    pub cell: f32,
    pub h: Vec<f32>,
}

impl HeightField {
    pub fn new(size: usize, cell: f32) -> Self {
        HeightField { size, cell, h: vec![0.0; size * size] }
    }

    #[inline]
    pub fn idx(&self, x: usize, z: usize) -> usize {
        z * self.size + x
    }

    #[inline]
    pub fn get(&self, x: usize, z: usize) -> f32 {
        self.h[z * self.size + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, z: usize, v: f32) {
        self.h[z * self.size + x] = v;
    }

    /// World extent in metres per side.
    pub fn extent(&self) -> f32 {
        self.size as f32 * self.cell
    }

    /// Bilinear height at a continuous CELL-space position (clamped to the field).
    pub fn sample(&self, x: f32, z: f32) -> f32 {
        let m = (self.size - 2) as f32;
        let x = x.clamp(0.0, m);
        let z = z.clamp(0.0, m);
        let ix = x as usize;
        let iz = z as usize;
        let fx = x - ix as f32;
        let fz = z - iz as f32;
        let a = self.get(ix, iz);
        let b = self.get(ix + 1, iz);
        let c = self.get(ix, iz + 1);
        let d = self.get(ix + 1, iz + 1);
        a + (b - a) * fx + (c - a) * fz + (a - b - c + d) * fx * fz
    }

    /// Height at a world-space metre position.
    pub fn sample_world(&self, wx: f32, wz: f32) -> f32 {
        self.sample(wx / self.cell, wz / self.cell)
    }

    /// Gradient (dh/dx, dh/dz per metre) at a continuous cell-space position.
    pub fn gradient(&self, x: f32, z: f32) -> (f32, f32) {
        let m = (self.size - 2) as f32;
        let x = x.clamp(0.0, m);
        let z = z.clamp(0.0, m);
        let ix = x as usize;
        let iz = z as usize;
        let fx = x - ix as f32;
        let fz = z - iz as f32;
        let a = self.get(ix, iz);
        let b = self.get(ix + 1, iz);
        let c = self.get(ix, iz + 1);
        let d = self.get(ix + 1, iz + 1);
        let gx = (b - a) * (1.0 - fz) + (d - c) * fz;
        let gz = (c - a) * (1.0 - fx) + (d - b) * fx;
        (gx / self.cell, gz / self.cell)
    }

    /// Smooth normal at world metres (y-up, normalised).
    pub fn normal_world(&self, wx: f32, wz: f32) -> [f32; 3] {
        let (gx, gz) = self.gradient(wx / self.cell, wz / self.cell);
        let len = (gx * gx + 1.0 + gz * gz).sqrt();
        [-gx / len, 1.0 / len, -gz / len]
    }
}

#[derive(Clone, Copy)]
pub struct TerrainParams {
    pub seed: u32,
    pub size: usize,
    pub cell: f32,
    /// 0 = rolling lowland only, 1 = strongly mountainous.
    pub mountainousness: f32,
    /// Peak height budget in metres for the ridged (mountain) component.
    pub mountain_height: f32,
    /// Height budget in metres for the rolling base.
    pub base_height: f32,
    /// Domain-warp strength (noise-space units; ~0.3–0.8 looks organic).
    pub warp: f32,
}

impl Default for TerrainParams {
    fn default() -> Self {
        TerrainParams {
            seed: 20260719,
            // 384 m — detail-focus default (~8× smaller area than the earlier 1 km map);
            // configurable from the panel, keep it a multiple of the 64-cell chunk.
            size: 384,
            cell: 1.0,
            mountainousness: 0.55,
            mountain_height: 170.0,
            base_height: 55.0,
            warp: 0.55,
        }
    }
}

/// Base heightfield: domain-warped rolling fBM + ridged massifs gated by a low-frequency
/// mountain mask. Erosion carves the realism afterwards — this only lays out landforms.
pub fn generate_base(p: &TerrainParams, mut progress: impl FnMut(f32)) -> HeightField {
    let mut hf = HeightField::new(p.size, p.cell);
    let ext = hf.extent();
    // Feature wavelengths in metres (sized for the 1 km reference map), CLAMPED to the
    // actual extent so a small detail-sandbox map still holds a full massif + lowland
    // instead of one corner of a much larger landform.
    let base_freq = 1.0 / 460.0f32.min(ext * 0.55);
    let ridge_freq = 1.0 / 720.0f32.min(ext * 0.75);
    let mask_freq = 1.0 / 1000.0f32.min(ext * 0.95);
    for z in 0..p.size {
        for x in 0..p.size {
            let wx = x as f32 * p.cell;
            let wz = z as f32 * p.cell;
            let rolling = warped_fbm(wx * base_freq, wz * base_freq, 6, p.warp, p.seed);
            let crest = ridged(wx * ridge_freq, wz * ridge_freq, 6, p.seed.wrapping_add(7));
            let mask_n = fbm(wx * mask_freq, wz * mask_freq, 3, p.seed.wrapping_add(23));
            // The mask's band shifts with the user weight: more mountainousness widens
            // and strengthens the massif regions instead of scaling everything uniformly
            // (uniform scaling reads as "same map, taller" — this changes the layout).
            let mask = smoothstep(0.62 - 0.34 * p.mountainousness, 0.85, mask_n);
            // Gentle continental tilt so drainage has somewhere to go at map scale.
            let tilt = 0.12 * (wx + wz) / (2.0 * ext);
            let h = rolling * p.base_height
                + crest * mask * p.mountain_height * (0.35 + 0.65 * p.mountainousness)
                + tilt * p.base_height;
            hf.set(x, z, h);
        }
        if z % 256 == 0 {
            progress(z as f32 / p.size as f32);
        }
    }
    progress(1.0);
    hf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> TerrainParams {
        TerrainParams { size: 128, ..Default::default() }
    }

    #[test]
    fn base_deterministic() {
        let a = generate_base(&small(), |_| {});
        let b = generate_base(&small(), |_| {});
        assert_eq!(a.h, b.h);
    }

    #[test]
    fn base_bounded_finite() {
        let p = small();
        let hf = generate_base(&p, |_| {});
        let max = p.base_height * 1.2 + p.mountain_height;
        for &v in &hf.h {
            assert!(v.is_finite());
            assert!(v >= -1.0 && v <= max, "v={v}");
        }
    }

    #[test]
    fn seed_changes_map() {
        let mut p2 = small();
        p2.seed = 999;
        let a = generate_base(&small(), |_| {});
        let b = generate_base(&p2, |_| {});
        assert_ne!(a.h, b.h);
    }

    #[test]
    fn sample_matches_grid_and_interpolates() {
        let hf = generate_base(&small(), |_| {});
        assert_eq!(hf.sample(10.0, 20.0), hf.get(10, 20));
        let mid = hf.sample(10.5, 20.0);
        let lo = hf.get(10, 20).min(hf.get(11, 20));
        let hi = hf.get(10, 20).max(hf.get(11, 20));
        assert!(mid >= lo && mid <= hi);
    }
}
