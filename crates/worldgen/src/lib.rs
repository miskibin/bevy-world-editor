//! Pure, deterministic world generation — no Bevy, no I/O. The Bevy app consumes the
//! `World` output (heightfield + derived maps + tree instances) and turns it into meshes.

pub mod erosion;
pub mod heightfield;
pub mod maps;
pub mod noise;
pub mod rng;
pub mod scatter;
pub mod tree;

pub use erosion::ErosionParams;
pub use heightfield::{HeightField, TerrainParams};
pub use scatter::{ForestParams, TreeInstance};
pub use tree::{ALL_SPECIES, Species, TreeSkeleton};

#[derive(Clone, Copy)]
pub struct WorldParams {
    pub terrain: TerrainParams,
    pub erosion: ErosionParams,
    pub forest: ForestParams,
}

impl Default for WorldParams {
    fn default() -> Self {
        WorldParams {
            terrain: TerrainParams::default(),
            erosion: ErosionParams::default(),
            forest: ForestParams::default(),
        }
    }
}

pub struct World {
    pub height: HeightField,
    pub slope: Vec<f32>,
    pub moisture: Vec<f32>,
    pub flow: Vec<f32>,
    pub trees: Vec<TreeInstance>,
}

/// Full pipeline. `progress(fraction, stage_label)` is called from the worker thread.
pub fn generate(p: &WorldParams, mut progress: impl FnMut(f32, &str)) -> World {
    progress(0.0, "landforms");
    let mut height =
        heightfield::generate_base(&p.terrain, |f| progress(f * 0.20, "landforms"));
    progress(0.20, "hydraulic erosion");
    let flow = erosion::erode(&mut height, &p.erosion, p.terrain.seed, |f| {
        progress(0.20 + f * 0.50, "hydraulic erosion")
    });
    progress(0.70, "thermal erosion");
    erosion::thermal(&mut height, &p.erosion, |f| progress(0.70 + f * 0.05, "thermal erosion"));
    progress(0.75, "derived maps");
    let slope = maps::slope_map(&height);
    let moisture = maps::moisture_map(&height, &flow, p.forest.water_level);
    progress(0.85, "forest scatter");
    let trees = scatter::scatter(&height, &slope, &moisture, &p.forest);
    progress(1.0, "done");
    World { height, slope, moisture, flow, trees }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_pipeline_small() {
        let p = WorldParams {
            terrain: TerrainParams { size: 192, ..Default::default() },
            erosion: ErosionParams { droplets: 5000, ..Default::default() },
            forest: ForestParams::default(),
        };
        let mut last = -1.0f32;
        let w = generate(&p, |f, _| {
            assert!(f >= last - 1e-3, "progress went backwards: {last} -> {f}");
            last = f;
        });
        assert!((last - 1.0).abs() < 1e-6);
        assert!(!w.trees.is_empty());
        assert_eq!(w.slope.len(), w.height.h.len());
        assert_eq!(w.moisture.len(), w.height.h.len());
    }
}
