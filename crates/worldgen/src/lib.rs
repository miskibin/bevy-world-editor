//! Pure, deterministic world generation — no Bevy, no I/O. The Bevy app consumes the
//! `World` output (heightfield + derived maps + tree instances) and turns it into meshes.

pub mod creatures;
pub mod erosion;
pub mod heightfield;
pub mod lakes;
pub mod maps;
pub mod noise;
pub mod path;
pub mod rng;
pub mod scatter;
pub mod trails;
pub mod tree;

pub use creatures::{creature_sites, CreatureSite, SiteKind};
pub use erosion::ErosionParams;
pub use path::{find_path, PathGrid};
pub use heightfield::{HeightField, TerrainParams};
pub use scatter::{ForestParams, PropInstance, RockInstance, TreeInstance};
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
    /// Per-cell lake surface height, `NEG_INFINITY` where dry (priority-flood).
    pub water: Vec<f32>,
    pub lake_count: usize,
    /// Per-cell trail wear 0..1 (1 = beaten path core).
    pub trails: Vec<f32>,
    pub trees: Vec<TreeInstance>,
    pub rocks: Vec<RockInstance>,
    pub props: Vec<PropInstance>,
}

/// Full pipeline. `progress(fraction, stage_label)` is called from the worker thread.
pub fn generate(p: &WorldParams, mut progress: impl FnMut(f32, &str)) -> World {
    progress(0.0, "landforms");
    let mut height =
        heightfield::generate_base(&p.terrain, |f| progress(f * 0.20, "landforms"));
    progress(0.20, "hydraulic erosion");
    // Droplet count is authored against the reference 1024² map; scale by actual area so
    // the erosion DENSITY (carving per cell) stays constant across map sizes.
    let mut ep = p.erosion;
    let area_ratio = (p.terrain.size as f32 / 1024.0).powi(2);
    ep.droplets = ((ep.droplets as f32 * area_ratio) as u32).max(2_000);
    let flow = erosion::erode(&mut height, &ep, p.terrain.seed, |f| {
        progress(0.20 + f * 0.50, "hydraulic erosion")
    });
    progress(0.70, "thermal erosion");
    erosion::thermal(&mut height, &p.erosion, |f| progress(0.70 + f * 0.05, "thermal erosion"));
    progress(0.75, "lakes");
    // 0.8m/120-cell floor: fewer puddle-tier lakes (user: "less water in the scene").
    let ws = lakes::detect_lakes(&height, p.forest.water_level, 0.8, 120);
    progress(0.80, "derived maps");
    let slope = maps::slope_map(&height);
    let moisture = maps::moisture_map(&height, &flow, &ws.surface, p.forest.water_level);
    progress(0.84, "trails");
    let trails = trails::build_trails(&height, &slope, &ws.surface, p.terrain.seed);
    progress(0.90, "scatter");
    let trees = scatter::scatter(&height, &slope, &moisture, &ws.surface, &trails, &p.forest);
    let rocks = scatter::scatter_rocks(&height, &slope, &ws.surface, p.terrain.seed);
    let props =
        scatter::scatter_props(&height, &slope, &moisture, &ws.surface, &trails, &p.forest);
    progress(1.0, "done");
    World {
        height,
        slope,
        moisture,
        flow,
        water: ws.surface,
        lake_count: ws.lake_count,
        trails,
        trees,
        rocks,
        props,
    }
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
