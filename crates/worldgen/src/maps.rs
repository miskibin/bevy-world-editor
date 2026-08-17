//! Derived maps: slope, moisture. Drive both the splat shader and vegetation placement.

use crate::heightfield::HeightField;

/// Slope per cell as rise-over-run (tan of the slope angle).
pub fn slope_map(hf: &HeightField) -> Vec<f32> {
    let size = hf.size;
    let mut out = vec![0.0f32; size * size];
    for z in 0..size {
        for x in 0..size {
            let (gx, gz) = hf.gradient(x as f32, z as f32);
            out[z * size + x] = (gx * gx + gz * gz).sqrt();
        }
    }
    out
}

/// Separable box blur, `passes`× (3 passes ≈ gaussian).
pub fn blur(map: &[f32], size: usize, radius: usize, passes: u32) -> Vec<f32> {
    let mut cur = map.to_vec();
    let mut tmp = vec![0.0f32; size * size];
    let r = radius as i32;
    let norm = 1.0 / (2 * radius + 1) as f32;
    for _ in 0..passes {
        // horizontal
        for z in 0..size {
            let row = z * size;
            let mut acc: f32 = 0.0;
            for dx in -r..=r {
                acc += cur[row + dx.clamp(0, size as i32 - 1).max(0) as usize];
            }
            for x in 0..size {
                tmp[row + x] = acc * norm;
                let add = (x as i32 + r + 1).clamp(0, size as i32 - 1) as usize;
                let sub = (x as i32 - r).clamp(0, size as i32 - 1) as usize;
                acc += cur[row + add] - cur[row + sub];
            }
        }
        // vertical
        for x in 0..size {
            let mut acc: f32 = 0.0;
            for dz in -r..=r {
                acc += tmp[dz.clamp(0, size as i32 - 1).max(0) as usize * size + x];
            }
            for z in 0..size {
                cur[z * size + x] = acc * norm;
                let add = (z as i32 + r + 1).clamp(0, size as i32 - 1) as usize;
                let sub = (z as i32 - r).clamp(0, size as i32 - 1) as usize;
                acc += tmp[add * size + x] - tmp[sub * size + x];
            }
        }
    }
    cur
}

/// Moisture in [0,1]: log-compressed blurred flow + proximity to the global water level
/// + a blurred halo around detected lakes (shores read lush).
///
/// `near_band` is how many metres above the water level still count as "near the water
/// table", and it decides whether the map has a meaningful water table at all. A temperate
/// landscape effectively does, so its band runs metres deep and the whole lowland reads
/// damp. A desert does not — the only wet ground is the strip beside the river — and using
/// the temperate band there soaks the entire basin, which puts riparian palms across the
/// whole map and destroys the oasis read completely.
pub fn moisture_map(
    hf: &HeightField,
    flow: &[f32],
    water: &[f32],
    water_level: f32,
    near_band: f32,
) -> Vec<f32> {
    let size = hf.size;
    // Log-compress flow (spans orders of magnitude), then blur to a soil-moisture halo.
    let mut m: Vec<f32> = flow.iter().map(|&f| (1.0 + f).ln()).collect();
    let max = m.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    m.iter_mut().for_each(|v| *v /= max);
    let mut m = blur(&m, size, 6, 2);
    let lake_mask: Vec<f32> =
        water.iter().map(|w| if w.is_finite() { 1.0 } else { 0.0 }).collect();
    let lake_halo = blur(&lake_mask, size, 8, 2);
    for z in 0..size {
        for x in 0..size {
            let h = hf.get(x, z);
            // Low ground near the water table is wet regardless of flow.
            let near_water = ((water_level + near_band - h) / near_band).clamp(0.0, 1.0);
            let i = z * size + x;
            m[i] = (m[i] * 1.6 + near_water * 0.6 + lake_halo[i] * 0.7).clamp(0.0, 1.0);
        }
    }
    m
}

/// Isolated damp patches away from the river — the oases and green hollows that keep an
/// arid map from being one river and nothing else.
///
/// Returned as a 0..1 field to be MAXed into moisture, so everything keyed on moisture —
/// ground splat, grass, props, tree species and stocking — agrees about where the green is
/// without any of them knowing that oases exist.
///
/// Two terms: sparse low-frequency blobs (hard-thresholded, so they are distinct patches
/// and not a gradient), biased toward local hollows, because a depression is where water
/// would actually collect.
pub fn oasis_field(hf: &HeightField, seed: u32) -> Vec<f32> {
    use crate::noise::{fbm, smoothstep};
    let size = hf.size;
    // Local relief: height minus a wide blur of it. Negative = a hollow.
    let relief = blur(&hf.h, size, 12, 2);
    let mut out = vec![0.0f32; size * size];
    for z in 0..size {
        for x in 0..size {
            let i = z * size + x;
            let (wx, wz) = (x as f32 * hf.cell, z as f32 * hf.cell);
            // ~150 m blobs. The high, narrow threshold is what makes them patches.
            let blob = fbm(wx / 150.0, wz / 150.0, 3, seed.wrapping_add(911));
            let patch = smoothstep(0.58, 0.74, blob);
            if patch <= 0.0 {
                continue;
            }
            // The hollow bias SHADES a patch, it must not gate it: at a 0.30 floor only
            // patches that happened to land in a depression ever got wet enough to read
            // green, so most of them were invisible.
            let hollow = smoothstep(2.5, -3.0, hf.h[i] - relief[i]);
            out[i] = patch * (0.58 + 0.42 * hollow);
        }
    }
    // Soften the rims so a patch fades into the sand instead of ending on a contour.
    blur(&out, size, 4, 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::{TerrainParams, generate_base};

    #[test]
    fn maps_in_range() {
        let tp = TerrainParams { size: 96, ..Default::default() };
        let hf = generate_base(&tp, crate::biome::TerrainStyle::Mountains, |_| {});
        let slope = slope_map(&hf);
        assert!(slope.iter().all(|s| s.is_finite() && *s >= 0.0));
        let flow: Vec<f32> = (0..96 * 96).map(|i| (i % 17) as f32).collect();
        let water = vec![f32::NEG_INFINITY; 96 * 96];
        let moist = moisture_map(&hf, &flow, &water, 10.0, 14.0);
        assert!(moist.iter().all(|m| (0.0..=1.0).contains(m)));
    }

    #[test]
    fn blur_preserves_constant_field() {
        let map = vec![0.5f32; 64 * 64];
        let b = blur(&map, 64, 4, 3);
        for &v in &b {
            assert!((v - 0.5).abs() < 1e-3, "v={v}");
        }
    }
}
