//! Pure, deterministic world generation — no Bevy, no rendering. The Bevy app consumes the
//! `World` output (heightfield + derived maps + tree instances) and turns it into meshes.
//!
//! Two entry points:
//! - [`generate`] — the original one-shot pipeline (params → world), unchanged in behaviour.
//! - [`build_project`] — the non-destructive project pipeline: the same procedural base plus
//!   an ordered layer stack (height deltas, painted masks, instance overrides). `generate`
//!   is now `build_project` over an empty stack, so the two agree byte-for-byte.
//!
//! The expensive erosion stage is isolated in [`generate_base`] (→ [`BaseFields`]) so an
//! editor can cache it and re-run only the cheap downstream steps ([`build_world_from_base`])
//! when the user tweaks a layer that doesn't touch the base terrain.

pub mod creatures;
pub mod erosion;
pub mod heightfield;
pub mod lakes;
pub mod maps;
pub mod noise;
pub mod path;
pub mod export;
pub mod project;
pub mod rng;
pub mod scatter;
pub mod trails;
pub mod tree;

pub use creatures::{creature_sites, CreatureSite, SiteKind};
pub use erosion::ErosionParams;
pub use heightfield::{HeightField, TerrainParams};
pub use path::{find_path, PathGrid};
pub use project::{
    InstanceAdd, InstanceRemove, Layer, LayerData, LayerKind, LayerRaster, MaskChannel, Project,
    RemoveKind, FORMAT_VERSION,
};
pub use scatter::{ForestParams, PropInstance, RockInstance, ScatterMasks, TreeInstance};
pub use tree::{Species, TreeSkeleton, ALL_SPECIES};

// serde(default): a project file written by an older editor (missing a newer sub-param)
// still loads — each absent field falls back to `WorldParams::default()`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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

/// A hand-placed instance in the finished world. Unlike scattered trees/rocks/props this
/// carries an opaque mesh id string (`<family>/<kind>/<variant>`) — the renderer resolves it.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedInstance {
    pub mesh: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub scale: f32,
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
    /// Instances added by `Instances` layers (empty for a plain `generate`).
    pub added: Vec<AddedInstance>,
}

/// The expensive, params-only base: landforms + hydraulic + thermal erosion. An editor caches
/// this and re-runs [`build_world_from_base`] on layer edits that don't touch the base.
pub struct BaseFields {
    /// Eroded heightfield (pre height-delta layers).
    pub height: HeightField,
    /// Water-passage map from the droplet sim (drives moisture; not re-derivable cheaply,
    /// so it rides the cached base rather than being "recomputed" downstream).
    pub flow: Vec<f32>,
}

/// Landforms + erosion — the cacheable base stage. Progress spans 0.0..0.75 (the same
/// fractions the original `generate` used, so callers see identical progress reporting).
pub fn generate_base(p: &WorldParams, progress: &mut dyn FnMut(f32, &str)) -> BaseFields {
    progress(0.0, "landforms");
    let mut height = heightfield::generate_base(&p.terrain, |f| progress(f * 0.20, "landforms"));
    progress(0.20, "hydraulic erosion");
    // Droplet count is authored against the reference 1024² map; scale by actual area so the
    // erosion DENSITY (carving per cell) stays constant across map sizes.
    let mut ep = p.erosion;
    let area_ratio = (p.terrain.size as f32 / 1024.0).powi(2);
    ep.droplets = ((ep.droplets as f32 * area_ratio) as u32).max(2_000);
    let flow = erosion::erode(&mut height, &ep, p.terrain.seed, |f| {
        progress(0.20 + f * 0.50, "hydraulic erosion")
    });
    progress(0.70, "thermal erosion");
    erosion::thermal(&mut height, &p.erosion, |f| progress(0.70 + f * 0.05, "thermal erosion"));
    BaseFields { height, flow }
}

/// Build a world from a project. Runs [`generate_base`] then [`build_world_from_base`].
pub fn build_project(project: &Project, progress: &mut dyn FnMut(f32, &str)) -> World {
    let base = generate_base(&project.params, progress);
    build_world_from_base(project, &base, progress)
}

/// The cheap downstream half of the pipeline over a cached [`BaseFields`]: apply enabled
/// HeightDelta layers, then recompute lakes/slope/moisture, trails (∪ PathWear masks),
/// mask-aware scatter, and finally the Instances layers. Progress spans 0.75..1.0.
///
/// Disabled layers and layers with `opacity == 0.0` are skipped. With an empty stack this is
/// exactly the tail of the original `generate`, so [`build_project`] over an empty project
/// reproduces `generate` byte-for-byte.
pub fn build_world_from_base(
    project: &Project,
    base: &BaseFields,
    progress: &mut dyn FnMut(f32, &str),
) -> World {
    let p = &project.params;

    // 1. Height deltas — added to the eroded field BEFORE anything downstream recomputes.
    let mut height = base.height.clone();
    for layer in &project.layers {
        if !layer.enabled || layer.opacity == 0.0 {
            continue;
        }
        if let LayerKind::HeightDelta { scale, .. } = &layer.kind {
            if let Some(raster) = project.rasters.get(&layer.id) {
                apply_height_delta(&mut height, raster, *scale, layer.opacity);
            }
        }
    }

    // 2. Recomputed maps (lakes/slope/moisture). Flow rides the cached base — the droplet
    //    sim can't be cheaply re-run, and moisture uses it as an approximation.
    progress(0.75, "lakes");
    let ws = lakes::detect_lakes(&height, p.forest.water_level, 0.8, 120);
    progress(0.80, "derived maps");
    let slope = maps::slope_map(&height);
    let moisture = maps::moisture_map(&height, &base.flow, &ws.surface, p.forest.water_level);

    // 3. Trails, then max-combine painted PathWear masks so hand-painted paths repel scatter.
    progress(0.84, "trails");
    let mut trails = trails::build_trails(&height, &slope, &ws.surface, p.terrain.seed);
    for layer in &project.layers {
        if !layer.enabled || layer.opacity == 0.0 {
            continue;
        }
        if let LayerKind::Mask { channel: MaskChannel::PathWear, .. } = &layer.kind {
            if let Some(raster) = project.rasters.get(&layer.id) {
                max_combine_wear(&mut trails, height.size, raster, layer.opacity);
            }
        }
    }

    // 4. Mask-aware scatter. Resolve the ForestDensity/Clearing channels to owned effective
    //    rasters (opacity baked, last enabled layer wins) so the hot scatter loop stays lean.
    progress(0.90, "scatter");
    let forest_density = resolve_mask(project, MaskChannel::ForestDensity);
    let clearing = resolve_mask(project, MaskChannel::Clearing);
    let masks = ScatterMasks {
        forest_density: forest_density.as_ref(),
        clearing: clearing.as_ref(),
    };
    let trees =
        scatter::scatter_masked(&height, &slope, &moisture, &ws.surface, &trails, &p.forest, &masks);
    let rocks = scatter::scatter_rocks_masked(&height, &slope, &ws.surface, p.terrain.seed, &masks);
    let props = scatter::scatter_props_masked(
        &height, &slope, &moisture, &ws.surface, &trails, &p.forest, &masks,
    );

    let mut world = World {
        height,
        slope,
        moisture,
        flow: base.flow.clone(),
        water: ws.surface,
        lake_count: ws.lake_count,
        trails,
        trees,
        rocks,
        props,
        added: Vec::new(),
    };

    // 5. Instance overrides, applied after scatter in stack order.
    for layer in &project.layers {
        if !layer.enabled || layer.opacity == 0.0 {
            continue;
        }
        if let LayerKind::Instances { added, removed } = &layer.kind {
            apply_instances(&mut world, added, removed);
        }
    }

    progress(1.0, "done");
    world
}

/// Full pipeline. `progress(fraction, stage_label)` is called from the worker thread.
/// Now a thin shim over [`build_project`] with an empty layer stack.
pub fn generate(p: &WorldParams, mut progress: impl FnMut(f32, &str)) -> World {
    let project = Project::new("", *p);
    build_project(&project, &mut progress)
}

// --- Layer application helpers ------------------------------------------------------------

/// Add a HeightDelta raster to the grid. The raster stores the normalised signed value in
/// -1..1; the applied delta is `sample · scale · opacity` metres (FORMAT.md). Bilinear so a
/// raster at a different resolution than the grid still maps cleanly.
fn apply_height_delta(height: &mut HeightField, raster: &LayerRaster, scale: f32, opacity: f32) {
    let size = height.size;
    let inv = if size > 1 { 1.0 / (size - 1) as f32 } else { 0.0 };
    for z in 0..size {
        for x in 0..size {
            let u = x as f32 * inv;
            let v = z as f32 * inv;
            let delta = raster.sample_norm(u, v) * scale * opacity;
            height.h[z * size + x] += delta;
        }
    }
}

/// Max-combine a PathWear mask into the trail-wear field (FORMAT.md: painted paths look
/// walked and repel scatter like generated trails).
fn max_combine_wear(trails: &mut [f32], size: usize, raster: &LayerRaster, opacity: f32) {
    let inv = if size > 1 { 1.0 / (size - 1) as f32 } else { 0.0 };
    for z in 0..size {
        for x in 0..size {
            let u = x as f32 * inv;
            let v = z as f32 * inv;
            let w = (raster.sample_norm(u, v) * opacity).clamp(0.0, 1.0);
            let idx = z * size + x;
            trails[idx] = trails[idx].max(w);
        }
    }
}

/// Resolve the effective mask for one channel into an owned raster of weights `w` in 0..1,
/// with layer opacity baked in. Stack order: the LAST enabled layer of the channel wins
/// (top overrides). Returns `None` when no such layer/raster exists — the no-mask path.
fn resolve_mask(project: &Project, channel: MaskChannel) -> Option<LayerRaster> {
    let mut chosen: Option<&Layer> = None;
    for layer in &project.layers {
        if !layer.enabled || layer.opacity == 0.0 {
            continue;
        }
        if let LayerKind::Mask { channel: c, .. } = &layer.kind {
            if *c == channel && project.rasters.contains_key(&layer.id) {
                chosen = Some(layer); // later in the stack overrides
            }
        }
    }
    let layer = chosen?;
    let raster = project.rasters.get(&layer.id)?;
    let op = layer.opacity;
    let data: Vec<f32> = (0..raster.w * raster.h)
        .map(|i| {
            let raw = match &raster.data {
                LayerData::U8(v) => v[i] as f32 / 255.0,
                LayerData::F32(v) => v[i],
            };
            raw * op
        })
        .collect();
    Some(LayerRaster::new_f32(raster.w, raster.h, data))
}

#[inline]
fn dist2(ax: f32, az: f32, bx: f32, bz: f32) -> f32 {
    let dx = ax - bx;
    let dz = az - bz;
    dx * dx + dz * dz
}

/// Mesh-id family prefix (`tree`/`rock`/`prop`/…) — the text before the first `/`.
fn mesh_family(mesh: &str) -> &str {
    mesh.split('/').next().unwrap_or(mesh)
}

/// Apply one Instances layer: removals first (cull scattered + already-added within radius),
/// then adds (y sampled from the final terrain). Doing removals before adds means a layer's
/// own adds survive its own removals — a layer is an authoring unit, not a self-eraser.
fn apply_instances(world: &mut World, added: &[InstanceAdd], removed: &[InstanceRemove]) {
    for r in removed {
        let r2 = r.radius * r.radius;
        match r.kind {
            RemoveKind::Tree => world.trees.retain(|t| dist2(t.x, t.z, r.x, r.z) > r2),
            RemoveKind::Rock => world.rocks.retain(|t| dist2(t.x, t.z, r.x, r.z) > r2),
            RemoveKind::Prop => world.props.retain(|t| dist2(t.x, t.z, r.x, r.z) > r2),
        }
        let fam = r.kind.family();
        world
            .added
            .retain(|a| !(mesh_family(&a.mesh) == fam && dist2(a.x, a.z, r.x, r.z) <= r2));
    }
    for a in added {
        let y = world.height.sample_world(a.x, a.z);
        world.added.push(AddedInstance {
            mesh: a.mesh.clone(),
            x: a.x,
            y,
            z: a.z,
            yaw: a.yaw,
            scale: a.scale,
        });
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
        assert!(w.added.is_empty(), "plain generate must add no instances");
    }

    // --- Staged / project pipeline -------------------------------------------------------

    fn test_params() -> WorldParams {
        // Small + few droplets: erosion is the only slow stage. 160² keeps a full base+build
        // under a fraction of a second so we can build many variants from one cached base.
        WorldParams {
            terrain: TerrainParams { size: 160, ..Default::default() },
            erosion: ErosionParams { droplets: 6000, ..Default::default() },
            forest: ForestParams::default(),
        }
    }

    fn noprog() -> impl FnMut(f32, &str) {
        |_, _| {}
    }

    /// A full-map raster of one constant normalised f32 value (height deltas).
    fn const_f32(size: usize, v: f32) -> LayerRaster {
        LayerRaster::new_f32(size, size, vec![v; size * size])
    }

    /// A full-map U8 mask of one constant byte.
    fn const_u8(size: usize, v: u8) -> LayerRaster {
        LayerRaster::new_u8(size, size, vec![v; size * size])
    }

    #[test]
    fn empty_stack_matches_generate() {
        let p = test_params();
        let plain = generate(&p, noprog());
        let proj = Project::new("empty", p);
        let staged = build_project(&proj, &mut noprog());
        // Base terrain identical.
        assert_eq!(plain.height.h, staged.height.h, "heights diverged");
        // Scatter identical (count + endpoints).
        assert_eq!(plain.trees.len(), staged.trees.len(), "tree count diverged");
        assert!(!plain.trees.is_empty());
        let (a0, b0) = (plain.trees.first().unwrap(), staged.trees.first().unwrap());
        let (a1, b1) = (plain.trees.last().unwrap(), staged.trees.last().unwrap());
        assert_eq!((a0.x, a0.z, a0.species), (b0.x, b0.z, b0.species));
        assert_eq!((a1.x, a1.z, a1.species), (b1.x, b1.z, b1.species));
        assert_eq!(plain.rocks.len(), staged.rocks.len());
        assert_eq!(plain.props.len(), staged.props.len());
    }

    #[test]
    fn build_project_deterministic() {
        let p = test_params();
        let proj = Project::new("det", p);
        let a = build_project(&proj, &mut noprog());
        let b = build_project(&proj, &mut noprog());
        assert_eq!(a.height.h, b.height.h);
        assert_eq!(a.trees.len(), b.trees.len());
        assert_eq!(a.trees.first().map(|t| (t.x, t.z)), b.trees.first().map(|t| (t.x, t.z)));
        assert_eq!(a.trees.last().map(|t| (t.x, t.z)), b.trees.last().map(|t| (t.x, t.z)));
    }

    #[test]
    fn height_delta_raises_region_and_recomputes_maps() {
        let p = test_params();
        let size = p.terrain.size;
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());

        // Raster: left half neutral (0 ⇒ no delta), right half +1 ⇒ +scale metres. The step
        // creates a fresh gradient the slope map must pick up.
        let mut data = vec![0.0f32; size * size];
        for z in 0..size {
            for x in size / 2..size {
                data[z * size + x] = 1.0;
            }
        }
        let scale = 40.0;
        let mut proj = Project::new("delta", p);
        proj.layers.push(Layer {
            id: 1,
            name: "raise".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::HeightDelta { sidecar: "l1.png".into(), scale },
        });
        proj.rasters.insert(1, LayerRaster::new_f32(size, size, data));
        let raised = build_world_from_base(&proj, &base, &mut noprog());

        // Deep in the right region a grid cell rose by ~scale; the left region is untouched.
        let deep = (size * 7) / 8;
        let idx_r = (size / 2) * size + deep;
        assert!(
            (raised.height.h[idx_r] - baseline.height.h[idx_r] - scale).abs() < 0.5,
            "right region should rise ~{scale}m, got {}",
            raised.height.h[idx_r] - baseline.height.h[idx_r]
        );
        let idx_l = (size / 2) * size + size / 8;
        assert!(
            (raised.height.h[idx_l] - baseline.height.h[idx_l]).abs() < 0.5,
            "left region should be unchanged"
        );
        // Maps recomputed: the step edge (last low column before the raised half) now has a
        // cliff, so its slope jumps vs the smooth baseline.
        let bnd = (size / 2) * size + (size / 2 - 1);
        assert!(
            (raised.slope[bnd] - baseline.slope[bnd]).abs() > 1e-3,
            "slope at the delta boundary should change (maps recomputed): {} vs {}",
            raised.slope[bnd],
            baseline.slope[bnd]
        );
    }

    #[test]
    fn clearing_mask_suppresses_scatter() {
        let p = test_params();
        let size = p.terrain.size;
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());
        assert!(!baseline.trees.is_empty(), "baseline should have trees to suppress");

        let mut proj = Project::new("clear", p);
        proj.layers.push(Layer {
            id: 1,
            name: "clearing".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::Mask {
                channel: MaskChannel::Clearing,
                sidecar: "l1.png".into(),
            },
        });
        // All-white clearing ⇒ w = 1.0 > 0.5 everywhere ⇒ hard exclusion.
        proj.rasters.insert(1, const_u8(size, 255));
        let cleared = build_world_from_base(&proj, &base, &mut noprog());
        assert!(cleared.trees.is_empty(), "clearing must remove all trees");
        assert!(cleared.props.is_empty(), "clearing must remove all props");
        assert!(cleared.rocks.is_empty(), "clearing must remove all rocks");
    }

    #[test]
    fn forest_density_mask_boosts_trees() {
        let p = test_params();
        let size = p.terrain.size;
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());

        let mut proj = Project::new("dense", p);
        proj.layers.push(Layer {
            id: 1,
            name: "forest".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::Mask {
                channel: MaskChannel::ForestDensity,
                sidecar: "l1.png".into(),
            },
        });
        // All-white ⇒ w = 1.0 ⇒ stocking ×2 vs the neutral baseline.
        proj.rasters.insert(1, const_u8(size, 255));
        let dense = build_world_from_base(&proj, &base, &mut noprog());
        assert!(
            dense.trees.len() > baseline.trees.len(),
            "forest-density 1.0 should add trees: dense={} baseline={}",
            dense.trees.len(),
            baseline.trees.len()
        );
    }

    #[test]
    fn instance_add_lands_on_terrain_and_remove_culls_a_tree() {
        let p = test_params();
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());
        let victim = *baseline.trees.first().expect("need a tree to remove");

        let mut proj = Project::new("inst", p);
        proj.layers.push(Layer {
            id: 1,
            name: "edits".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::Instances {
                added: vec![InstanceAdd {
                    mesh: "prop/log/1".into(),
                    x: 42.0,
                    z: 71.0,
                    yaw: 0.3,
                    scale: 1.0,
                }],
                removed: vec![InstanceRemove {
                    kind: RemoveKind::Tree,
                    x: victim.x,
                    z: victim.z,
                    radius: 1.0,
                }],
            },
        });
        let world = build_world_from_base(&proj, &base, &mut noprog());

        // Added instance's y equals the terrain height at its (x,z).
        let add = world.added.iter().find(|a| a.mesh == "prop/log/1").expect("added instance");
        let expect_y = world.height.sample_world(42.0, 71.0);
        assert!((add.y - expect_y).abs() < 1e-4, "added y must sit on the terrain");

        // The specific scattered tree at the removal centre is gone.
        let still_there =
            world.trees.iter().any(|t| dist2(t.x, t.z, victim.x, victim.z) <= 1.0);
        assert!(!still_there, "removal should cull the tree at its centre");
    }

    #[test]
    fn disabled_layer_is_a_noop() {
        // A disabled clearing layer must not touch scatter.
        let p = test_params();
        let size = p.terrain.size;
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());

        let mut proj = Project::new("off", p);
        proj.layers.push(Layer {
            id: 1,
            name: "clearing".into(),
            enabled: false,
            opacity: 1.0,
            kind: LayerKind::Mask { channel: MaskChannel::Clearing, sidecar: "l1.png".into() },
        });
        proj.rasters.insert(1, const_u8(size, 255));
        let world = build_world_from_base(&proj, &base, &mut noprog());
        assert_eq!(world.trees.len(), baseline.trees.len(), "disabled layer changed scatter");
    }

    #[test]
    fn pathwear_mask_repels_scatter() {
        // A full-strength PathWear mask floods the wear field; trees keep off worn ground.
        let p = test_params();
        let size = p.terrain.size;
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());

        let mut proj = Project::new("paths", p);
        proj.layers.push(Layer {
            id: 1,
            name: "wear".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::Mask { channel: MaskChannel::PathWear, sidecar: "l1.png".into() },
        });
        proj.rasters.insert(1, const_u8(size, 255));
        let worn = build_world_from_base(&proj, &base, &mut noprog());
        assert!(
            worn.trees.len() < baseline.trees.len(),
            "path wear should repel trees: worn={} baseline={}",
            worn.trees.len(),
            baseline.trees.len()
        );
        assert!(worn.trails.iter().all(|&w| w >= 1.0 - 1e-6), "wear field flooded to 1.0");
    }

    #[test]
    fn const_f32_raster_helper_is_neutral_height() {
        // A neutral (0.0) height-delta raster must not move terrain — guards the encode path.
        let p = test_params();
        let size = p.terrain.size;
        let base = generate_base(&p, &mut noprog());
        let baseline = build_world_from_base(&Project::new("b", p), &base, &mut noprog());
        let mut proj = Project::new("zero", p);
        proj.layers.push(Layer {
            id: 1,
            name: "zero".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::HeightDelta { sidecar: "l1.png".into(), scale: 50.0 },
        });
        proj.rasters.insert(1, const_f32(size, 0.0));
        let world = build_world_from_base(&proj, &base, &mut noprog());
        assert_eq!(world.height.h, baseline.height.h, "zero delta must not move terrain");
    }
}
