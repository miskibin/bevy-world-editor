//! Heightfield storage + the base (pre-erosion) terrain generator.

use crate::biome::TerrainStyle;
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

// serde(default) at the container level: any field missing from an older project file
// falls back to `TerrainParams::default()`, so additive params never break old saves.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
            // 1088 m — 8x the AREA of the 384 m detail sandbox (user call), still a
            // multiple of the 64-cell terrain chunk. Panel slider overrides it.
            size: 1088,
            cell: 1.0,
            mountainousness: 0.55,
            mountain_height: 170.0,
            base_height: 55.0,
            warp: 0.55,
        }
    }
}

/// Base heightfield. Dispatches on the biome's [`TerrainStyle`]; erosion carves the realism
/// afterwards, so this only lays out landforms.
pub fn generate_base(
    p: &TerrainParams,
    style: TerrainStyle,
    progress: impl FnMut(f32),
) -> HeightField {
    match style {
        TerrainStyle::Mountains => generate_mountains(p, progress),
        TerrainStyle::Mesas => generate_mesas(p, progress),
    }
}

/// Broad flat basins + hard-stepped sandstone mesas — the Stronghold-Crusader landform.
///
/// The whole look lives in one trick: plateaus are built as **stacked slabs with a
/// deliberately tiny smoothstep band**, so the transition between tiers is a scarp a
/// couple of metres wide instead of a slope. Widen `SCARP` even slightly and the mesas
/// melt into ordinary hills — that (plus running full-strength hydraulic erosion over
/// them) is the fastest way to lose the silhouette entirely.
///
/// Everything else is deliberately calm: an RTS map is mostly *buildable ground*, so the
/// basins get gentle undulation plus wind ripples of barely a metre, and the drama is
/// concentrated in the few tiers that stand above them.
fn generate_mesas(p: &TerrainParams, mut progress: impl FnMut(f32)) -> HeightField {
    let mut hf = HeightField::new(p.size, p.cell);
    let ext = hf.extent();
    // Plateau fields at two scales: big buttes and smaller outliers around their feet.
    let plat_freq = 1.0 / 300.0f32.min(ext * 0.42);
    let out_freq = 1.0 / 120.0f32.min(ext * 0.20);
    let roll_freq = 1.0 / 210.0f32.min(ext * 0.32);
    let dune_freq = 1.0 / 46.0f32;

    // Total relief budget for the stepped tiers, and how it splits between them.
    let mesa_budget = p.mountain_height * (0.30 + 0.45 * p.mountainousness);
    const TIERS: usize = 3;
    // Thresholds rise per tier so higher steps sit nested inside lower ones — that is what
    // makes a butte read as layered rock rather than as three unrelated blobs.
    const THRESH: [f32; TIERS] = [0.50, 0.615, 0.72];
    /// Half-width of the smoothstep band in plateau-field units. Small on purpose: this
    /// number IS the cliff. See the fn docs.
    const SCARP: f32 = 0.014;

    for z in 0..p.size {
        for x in 0..p.size {
            let wx = x as f32 * p.cell;
            let wz = z as f32 * p.cell;

            // Gentle basin undulation — the buildable ground.
            let rolling = warped_fbm(wx * roll_freq, wz * roll_freq, 5, p.warp, p.seed);
            // Wind ripples: ridged, sub-metre, and faded out on the plateaus (bare rock
            // does not hold dune forms).
            let dunes = ridged(wx * dune_freq, wz * dune_freq, 2, p.seed.wrapping_add(41));

            let plat = warped_fbm(wx * plat_freq, wz * plat_freq, 4, p.warp * 0.7, p.seed.wrapping_add(11));
            let outlier =
                fbm(wx * out_freq, wz * out_freq, 3, p.seed.wrapping_add(101));
            // Outliers nudge the plateau field locally, so tier edges gain bays and stacks
            // instead of tracing one smooth contour all the way round the butte.
            let field = plat + (outlier - 0.5) * 0.10;

            let mut mesa = 0.0f32;
            for (k, thr) in THRESH.iter().enumerate() {
                let step = mesa_budget * (0.46 - 0.10 * k as f32);
                mesa += smoothstep(thr - SCARP, thr + SCARP, field) * step;
            }
            // Cap height: on top of the highest tier the surface is nearly dead flat, so
            // the mesa top reads as caprock (and is usable ground).
            let on_top = smoothstep(THRESH[TIERS - 1], THRESH[TIERS - 1] + 0.05, field);
            let basin = 1.0 - smoothstep(THRESH[0] - SCARP, THRESH[0] + 0.06, field);

            // One consistent tilt across the map so the river has somewhere to run.
            let tilt = 0.25 * (wx * 0.75 + wz) / (1.75 * ext);

            // Both terms are deliberately small. Ripples of even ~1.5 m at dune wavelength
            // put a gradient over 0.15 across the WHOLE basin, which quietly destroys the
            // "broad flat buildable ground" the layout depends on — the relief has to be
            // spent on the mesas, not smeared over the floor.
            let h = rolling * p.base_height * 0.26 * (1.0 - on_top * 0.8)
                + dunes * 0.7 * basin
                + mesa
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

/// Rolling fBM + ridged massifs gated by a low-frequency mountain mask.
fn generate_mountains(p: &TerrainParams, mut progress: impl FnMut(f32)) -> HeightField {
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

    const MTN: TerrainStyle = TerrainStyle::Mountains;

    #[test]
    fn base_deterministic() {
        for style in [TerrainStyle::Mountains, TerrainStyle::Mesas] {
            let a = generate_base(&small(), style, |_| {});
            let b = generate_base(&small(), style, |_| {});
            assert_eq!(a.h, b.h, "{style:?} not deterministic");
        }
    }

    #[test]
    fn base_bounded_finite() {
        let p = small();
        let max = p.base_height * 1.2 + p.mountain_height;
        for style in [TerrainStyle::Mountains, TerrainStyle::Mesas] {
            let hf = generate_base(&p, style, |_| {});
            for &v in &hf.h {
                assert!(v.is_finite());
                assert!(v >= -1.0 && v <= max, "{style:?} v={v}");
            }
        }
    }

    #[test]
    fn seed_changes_map() {
        let mut p2 = small();
        p2.seed = 999;
        let a = generate_base(&small(), MTN, |_| {});
        let b = generate_base(&p2, MTN, |_| {});
        assert_ne!(a.h, b.h);
    }

    /// The mesa style must actually produce cliffs — and, just as importantly, must leave
    /// most of the map flat enough to build and manoeuvre on. That combination (broad
    /// level ground punctuated by walls) *is* the Crusader layout; rolling hills with the
    /// same relief budget would be a different map entirely.
    ///
    /// Measured against the shipped arid preset rather than `TerrainParams::default()`,
    /// because the preset's own relief budget is what players actually get.
    #[test]
    fn mesas_are_flat_ground_plus_real_scarps() {
        let mut p = crate::WorldParams::for_biome(crate::Biome::Arid).terrain;
        p.size = 256;
        let hf = generate_base(&p, TerrainStyle::Mesas, |_| {});
        let (mut steep, mut flat, mut n) = (0usize, 0usize, 0usize);
        for z in 1..hf.size - 1 {
            for x in 1..hf.size - 1 {
                let (gx, gz) = hf.gradient(x as f32, z as f32);
                let s = (gx * gx + gz * gz).sqrt();
                n += 1;
                if s > 1.5 {
                    steep += 1; // ~56°+ — a wall
                }
                if s < 0.25 {
                    flat += 1; // under ~14° — buildable, walkable, siegeable
                }
            }
        }
        let steep_frac = steep as f32 / n as f32;
        let flat_frac = flat as f32 / n as f32;
        assert!(steep_frac > 0.005, "no scarps at all ({steep_frac})");
        assert!(steep_frac < 0.20, "the whole map is cliff ({steep_frac})");
        assert!(flat_frac > 0.55, "not enough buildable ground ({flat_frac})");
    }

    #[test]
    fn sample_matches_grid_and_interpolates() {
        let hf = generate_base(&small(), MTN, |_| {});
        assert_eq!(hf.sample(10.0, 20.0), hf.get(10, 20));
        let mid = hf.sample(10.5, 20.0);
        let lo = hf.get(10, 20).min(hf.get(11, 20));
        let hi = hf.get(10, 20).max(hf.get(11, 20));
        assert!(mid >= lo && mid <= hi);
    }
}
