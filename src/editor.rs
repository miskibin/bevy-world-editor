//! Interactive map editing — the brush core of the editor pivot (see
//! docs/specs/2026-07-21-map-editor-research.md and docs/FORMAT.md).
//!
//! Model: the generated world stays the untouched BASE; every stroke edits an
//! in-memory non-destructive layer (a height-delta grid, or a mask grid per channel)
//! and the displayed heightfield is always `base + delta`. Terrain chunks touched by a
//! stroke are re-meshed IN PLACE through `GroundChunkIndex` (asset insert on the same
//! handle — nothing despawns, LOD/AABB entities stay put). Undo is a command stack of
//! dirty-rect diffs, never whole-grid snapshots.
//!
//! The heightfield mutation deliberately uses `bypass_change_detection` — a flagged
//! `GeneratedWorld` change would re-queue every chunk and re-index the whole forest.

use bevy::camera::visibility::NoFrustumCulling;
use bevy::input::mouse::MouseWheel;
use bevy::light::NotShadowCaster;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use crate::genrun::{GeneratedWorld, world_offset};
use crate::terrain_mesh::{CHUNK, GroundChunkIndex, build_chunk};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Off,
    Raise,
    Lower,
    Smooth,
    Flatten,
    Paint(MaskCh),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MaskCh {
    Forest,
    Clearing,
    Path,
    Grass,
}

impl MaskCh {
    pub const ALL: [MaskCh; 4] = [MaskCh::Forest, MaskCh::Clearing, MaskCh::Path, MaskCh::Grass];
    pub fn idx(self) -> usize {
        match self {
            MaskCh::Forest => 0,
            MaskCh::Clearing => 1,
            MaskCh::Path => 2,
            MaskCh::Grass => 3,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MaskCh::Forest => "forest density",
            MaskCh::Clearing => "clearing",
            MaskCh::Path => "path",
            MaskCh::Grass => "grass density",
        }
    }
    /// Overlay tint (r, g, b) for painted weight.
    fn tint(self) -> [f32; 3] {
        match self {
            MaskCh::Forest => [0.1, 0.9, 0.2],
            MaskCh::Clearing => [0.95, 0.25, 0.15],
            MaskCh::Path => [0.75, 0.55, 0.25],
            MaskCh::Grass => [0.5, 0.9, 0.1],
        }
    }
}

/// Brush weighting from stroke centre (q = 0) to rim (q = 1). Combined with the inner
/// `hardness` plateau in `brush_fall`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BrushFalloff {
    Smooth,
    Linear,
    Sharp,
    Constant,
}

impl BrushFalloff {
    pub const ALL: [BrushFalloff; 4] =
        [BrushFalloff::Smooth, BrushFalloff::Linear, BrushFalloff::Sharp, BrushFalloff::Constant];
    pub fn label(self) -> &'static str {
        match self {
            BrushFalloff::Smooth => "smooth",
            BrushFalloff::Linear => "linear",
            BrushFalloff::Sharp => "sharp",
            BrushFalloff::Constant => "flat",
        }
    }
}

/// Shared brush weight for both sculpt and paint. `q` is the normalised distance from the
/// stroke centre (0..1, callers pass only q <= 1). `hardness` (0..1) holds the weight at a
/// full 1.0 out to q == hardness, then the selected curve remaps over (hardness..1].
fn brush_fall(mode: BrushFalloff, hardness: f32, q: f32) -> f32 {
    if q <= hardness {
        return 1.0;
    }
    let t = if hardness < 1.0 { (q - hardness) / (1.0 - hardness) } else { 1.0 };
    match mode {
        BrushFalloff::Smooth => {
            let s = 1.0 - t * t;
            s * s
        }
        BrushFalloff::Linear => 1.0 - t,
        BrushFalloff::Sharp => {
            let u = 1.0 - t;
            u * u
        }
        BrushFalloff::Constant => 1.0,
    }
}

#[derive(Resource)]
pub struct EditorState {
    pub tool: Tool,
    pub radius: f32,
    pub strength: f32,
    pub falloff: BrushFalloff,
    /// Inner plateau (0..1): fraction of the radius that stays at full brush weight.
    pub hardness: f32,
    /// Re-fire the Apply path automatically a beat after a stroke ends (live scatter).
    pub auto_apply: bool,
    /// Terrain point under the cursor this frame (world space), if any.
    pub cursor_hit: Option<Vec3>,
    /// Height-delta value under the cursor this frame (status read-out).
    pub cursor_delta: f32,
    /// Set true by the UI when pointer is over an egui panel — strokes must not paint.
    pub ui_hover: bool,
    /// True while a stroke is being applied (status read-out).
    pub stroking: bool,
    /// Seconds left on the auto-apply debounce; <= 0 means nothing pending.
    pub auto_apply_in: f32,
    /// Height the Flatten tool is pulling toward (sampled at stroke start, or Alt-picked).
    flatten_target: f32,
    // UI intents (set by the panel, consumed by editor systems).
    pub apply_clicked: bool,
    pub save_clicked: bool,
    pub load_clicked: bool,
    pub undo_clicked: bool,
    pub redo_clicked: bool,
    pub file_path: String,
    pub status: String,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            tool: Tool::Off,
            radius: 12.0,
            strength: 1.0,
            falloff: BrushFalloff::Smooth,
            hardness: 0.0,
            auto_apply: true,
            cursor_hit: None,
            cursor_delta: 0.0,
            ui_hover: false,
            stroking: false,
            auto_apply_in: 0.0,
            flatten_target: 0.0,
            apply_clicked: false,
            save_clicked: false,
            load_clicked: false,
            undo_clicked: false,
            redo_clicked: false,
            file_path: "map.nsproj".into(),
            status: String::new(),
        }
    }
}

/// The in-memory non-destructive edit layers (serialized to the .nsproj format by the
/// save path once `worldgen::project` lands).
#[derive(Resource, Default)]
pub struct EditLayers {
    pub res: usize,
    /// Generated base heights, snapshotted at world-ready. Displayed = base + delta.
    pub base_height: Vec<f32>,
    pub height_delta: Vec<f32>,
    /// Mask grids by MaskCh::idx(). Forest is neutral at 128; others start at 0.
    pub masks: [Vec<u8>; 4],
    pub dirty_since_apply: bool,
}

enum Cmd {
    Height { rect: Rect2u, before: Vec<f32> },
    Mask { ch: MaskCh, rect: Rect2u, before: Vec<u8> },
}

#[derive(Clone, Copy)]
struct Rect2u {
    x0: usize,
    z0: usize,
    x1: usize, // exclusive
    z1: usize,
}

#[derive(Resource, Default)]
pub struct UndoStack {
    undo: Vec<Cmd>,
    redo: Vec<Cmd>,
}

impl UndoStack {
    pub fn depth(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

/// Per-stroke scratch: first-touch backup of every cell the stroke visits.
#[derive(Resource, Default)]
struct Stroke {
    active: bool,
    height_before: HashMap<usize, f32>,
    mask_before: HashMap<usize, u8>,
    rect: Option<Rect2u>,
    dirty_chunks: Vec<(usize, usize)>,
    rebuild_timer: f32,
}

/// The translucent quad that visualises the active mask channel while painting.
#[derive(Component)]
struct MaskOverlay;

#[derive(Resource, Default)]
struct OverlayAssets {
    image: Handle<Image>,
    entity: Option<Entity>,
    shown_ch: Option<MaskCh>,
    refresh: bool,
}

/// Fixed metres-at-full-deflection for the sculpt layer's PNG16 sidecar --
/// +-120 m at 16-bit depth is ~3.7 mm per step, below anything a brush produces.
const DELTA_SCALE: f32 = 120.0;

/// Set when the next world arriving is an editor Apply/Load -- init_layers must keep
/// the edit layers instead of wiping them.
#[derive(Resource, Default)]
pub struct KeepLayers(pub bool);

/// A loaded project waiting for its fresh base before layers re-apply.
#[derive(Resource, Default)]
pub struct PendingProject(pub Option<worldgen::Project>);

/// Build a worldgen Project from the live edit grids (the save/apply path).
pub fn to_project(
    layers: &EditLayers,
    params: worldgen::WorldParams,
    name: &str,
) -> worldgen::Project {
    use worldgen::{Layer, LayerData, LayerKind, LayerRaster, MaskChannel};
    let mut prj = worldgen::Project {
        format_version: 1,
        name: name.to_string(),
        params,
        layers: Vec::new(),
        rasters: Default::default(),
    };
    let res = layers.res;
    if layers.height_delta.iter().any(|d| d.abs() > 1e-4) {
        let norm: Vec<f32> =
            layers.height_delta.iter().map(|d| (d / DELTA_SCALE).clamp(-1.0, 1.0)).collect();
        prj.layers.push(Layer {
            id: 1,
            name: "sculpt".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::HeightDelta {
                sidecar: "layer-1-heightdelta.png".into(),
                scale: DELTA_SCALE,
            },
        });
        prj.rasters.insert(1, LayerRaster { w: res, h: res, data: LayerData::F32(norm) });
    }
    let channels = [
        (2u64, MaskCh::Forest, MaskChannel::ForestDensity, 128u8, "forest"),
        (3, MaskCh::Clearing, MaskChannel::Clearing, 0, "clearing"),
        (4, MaskCh::Path, MaskChannel::PathWear, 0, "path"),
        (5, MaskCh::Grass, MaskChannel::GrassDensity, 0, "grass"),
    ];
    for (id, ch, wch, neutral, label) in channels {
        let grid = &layers.masks[ch.idx()];
        if grid.iter().any(|&v| v != neutral) {
            prj.layers.push(Layer {
                id,
                name: label.into(),
                enabled: true,
                opacity: 1.0,
                kind: LayerKind::Mask {
                    channel: wch,
                    sidecar: format!("layer-{id}-{label}.png"),
                },
            });
            prj.rasters
                .insert(id, LayerRaster { w: res, h: res, data: LayerData::U8(grid.clone()) });
        }
    }
    prj
}

/// Inverse of `to_project`: pour a loaded project's layers into the live edit grids.
fn from_project(prj: &worldgen::Project, layers: &mut EditLayers) {
    use worldgen::{LayerData, LayerKind, MaskChannel};
    let res = layers.res;
    for layer in &prj.layers {
        if !layer.enabled {
            continue;
        }
        let Some(raster) = prj.rasters.get(&layer.id) else { continue };
        match (&layer.kind, &raster.data) {
            (LayerKind::HeightDelta { scale, .. }, LayerData::F32(v))
                if v.len() == res * res =>
            {
                for i in 0..res * res {
                    layers.height_delta[i] += v[i] * scale * layer.opacity;
                }
            }
            (LayerKind::Mask { channel, .. }, LayerData::U8(v)) if v.len() == res * res => {
                let idx = match channel {
                    MaskChannel::ForestDensity => MaskCh::Forest.idx(),
                    MaskChannel::Clearing => MaskCh::Clearing.idx(),
                    MaskChannel::PathWear => MaskCh::Path.idx(),
                    MaskChannel::GrassDensity => MaskCh::Grass.idx(),
                };
                layers.masks[idx].copy_from_slice(v);
            }
            _ => {}
        }
    }
}

/// Consume the panel's Apply/Save/Load intents.
fn file_ops(
    mut state: ResMut<EditorState>,
    mut layers: ResMut<EditLayers>,
    base: Option<Res<crate::genrun::BaseCache>>,
    mut regen: crate::genrun::Regen,
    mut keep: ResMut<KeepLayers>,
    mut pending: ResMut<PendingProject>,
    mut gen_params: ResMut<crate::genrun::GenParams>,
) {
    if state.apply_clicked {
        state.apply_clicked = false;
        if let Some(base) = &base {
            let prj = to_project(&layers, gen_params.0, "session");
            regen.fire_apply(prj, base.0.clone());
            keep.0 = true;
            layers.dirty_since_apply = false;
            state.status = "applying...".into();
        }
    }
    if state.save_clicked {
        state.save_clicked = false;
        let prj = to_project(&layers, gen_params.0, "map");
        state.status = match prj.save(&state.file_path) {
            Ok(()) => format!("saved {}", state.file_path),
            Err(e) => format!("save failed: {e}"),
        };
    }
    if state.load_clicked {
        state.load_clicked = false;
        match worldgen::Project::load(&state.file_path) {
            Ok(prj) => {
                // New params need a fresh base; layers re-apply once it lands.
                gen_params.0 = prj.params;
                pending.0 = Some(prj);
                let p = gen_params.clone();
                regen.fire(&p);
                state.status = format!("loading {} -- regenerating base...", state.file_path);
            }
            Err(e) => state.status = format!("load failed: {e}"),
        }
    }
}

/// Once the fresh base for a loaded project lands, pour the layers in and apply them.
fn finish_load(
    mut pending: ResMut<PendingProject>,
    base: Option<Res<crate::genrun::BaseCache>>,
    mut layers: ResMut<EditLayers>,
    mut regen: crate::genrun::Regen,
    mut keep: ResMut<KeepLayers>,
    mut state: ResMut<EditorState>,
) {
    let Some(base) = base else { return };
    if !base.is_changed() || pending.0.is_none() || layers.res == 0 {
        return;
    }
    let Some(prj) = pending.0.take() else { return };
    from_project(&prj, &mut layers);
    keep.0 = true;
    regen.fire_apply(prj, base.0.clone());
    state.status = "applying loaded layers...".into();
}

/// `WED_EDITDEMO=paint|apply` — screenshot staging: programmatically sculpts a hill,
/// flattens a camp shelf, paints a clearing + a path + a forest-boost blob, then either
/// leaves the paint overlay up (`paint`) or fires Apply (`apply`) so the shot shows the
/// scatter reacting. Runs once, after the world and layers are ready.
#[allow(clippy::too_many_arguments)]
fn stage_editdemo(
    mut done: Local<bool>,
    mut state: ResMut<EditorState>,
    mut layers: ResMut<EditLayers>,
    mut world: Option<ResMut<GeneratedWorld>>,
    index: Res<GroundChunkIndex>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut overlay: ResMut<OverlayAssets>,
    mut cam: Query<(&mut Transform, &mut crate::flycam::FlyCam)>,
) {
    if *done || layers.res == 0 || index.0.is_empty() {
        return;
    }
    let mode = std::env::var("WED_EDITDEMO").unwrap_or_default();
    let Some(world) = world.as_mut() else { return };
    // Wait until every chunk is meshed so the in-place rebuilds land on real handles.
    let res = layers.res;
    let n_chunks = res / CHUNK;
    if index.0.len() < n_chunks * n_chunks {
        return;
    }
    *done = true;

    // Stage on DRY ground: hill on the first meadow, clearing in the nearest forest
    // floor (the map centre is frequently a lake).
    let sites = worldgen::creature_sites(&world.0);
    let meadow = sites
        .iter()
        .find(|s| matches!(s.kind, worldgen::SiteKind::Meadow))
        .map(|s| (s.x, s.z))
        .unwrap_or((res as f32 * 0.3, res as f32 * 0.3));
    let forest = sites
        .iter()
        .filter(|s| matches!(s.kind, worldgen::SiteKind::ForestFloor))
        .min_by(|a, b| {
            let da = (a.x - meadow.0).hypot(a.z - meadow.1);
            let db = (b.x - meadow.0).hypot(b.z - meadow.1);
            da.total_cmp(&db)
        })
        .map(|s| (s.x, s.z))
        .unwrap_or((meadow.0 + 60.0, meadow.1));

    let w = world.bypass_change_detection();
    let Some(w0) = std::sync::Arc::get_mut(&mut w.0) else { return };

    // Sculpt: a 22 m hill on the meadow and a flattened camp shelf beside it.
    let hill = (meadow.0, meadow.1, 30.0f32, 22.0f32);
    let shelf = (meadow.0 + 42.0, meadow.1 + 18.0, 16.0f32);
    let shelf_h = w0.height.get(shelf.0 as usize, shelf.1 as usize);
    for z in 0..res {
        for x in 0..res {
            let i = z * res + x;
            let mut d = 0.0f32;
            let dh = ((x as f32 - hill.0).powi(2) + (z as f32 - hill.1).powi(2)).sqrt();
            if dh < hill.2 {
                let q = 1.0 - (dh / hill.2).powi(2);
                d += hill.3 * q * q;
            }
            let ds = ((x as f32 - shelf.0).powi(2) + (z as f32 - shelf.1).powi(2)).sqrt();
            if ds < shelf.2 {
                let cur = layers.base_height[i];
                let k = (1.0 - (ds / shelf.2).powi(2)).clamp(0.0, 1.0);
                d += (shelf_h - cur - d) * k;
            }
            if d.abs() > 1e-4 {
                layers.height_delta[i] += d;
                w0.height.h[i] = layers.base_height[i] + layers.height_delta[i];
            }
        }
    }
    // Rebuild every touched chunk in place.
    for cz in 0..n_chunks {
        for cx in 0..n_chunks {
            let x0 = cx * CHUNK;
            let z0 = cz * CHUNK;
            let near = |c: f32, lo: f32, hi: f32| c >= lo - 40.0 && c <= hi + 40.0;
            let touches = (near(hill.0, x0 as f32, (x0 + CHUNK) as f32)
                && near(hill.1, z0 as f32, (z0 + CHUNK) as f32))
                || (near(shelf.0, x0 as f32, (x0 + CHUNK) as f32)
                    && near(shelf.1, z0 as f32, (z0 + CHUNK) as f32));
            if touches {
                if let Some((full, coarse)) = index.0.get(&(x0, z0)) {
                    let _ = meshes.insert(full.id(), build_chunk(w0, x0, z0, 1));
                    let _ = meshes.insert(coarse.id(), build_chunk(w0, x0, z0, 4));
                }
            }
        }
    }

    // Masks: clearing circle in the forest, forest-boost blob, a path linking them.
    let clearing = (forest.0, forest.1, 26.0f32);
    let boost = (meadow.0 - 55.0, meadow.1 + 40.0, 24.0f32);
    for z in 0..res {
        for x in 0..res {
            let i = z * res + x;
            let dc = ((x as f32 - clearing.0).powi(2) + (z as f32 - clearing.1).powi(2)).sqrt();
            if dc < clearing.2 {
                layers.masks[MaskCh::Clearing.idx()][i] = 255;
            }
            let db = ((x as f32 - boost.0).powi(2) + (z as f32 - boost.1).powi(2)).sqrt();
            if db < boost.2 {
                layers.masks[MaskCh::Forest.idx()][i] = 255;
            }
        }
    }
    // Path: straight stroke from the clearing toward the hill.
    let steps = 260;
    for k in 0..=steps {
        let t = k as f32 / steps as f32;
        let px = clearing.0 + (hill.0 - clearing.0) * t;
        let pz = clearing.1 + (hill.1 - clearing.1) * t;
        for dz in -2i32..=2 {
            for dx in -2i32..=2 {
                let x = (px as i32 + dx).clamp(0, res as i32 - 1) as usize;
                let z = (pz as i32 + dz).clamp(0, res as i32 - 1) as usize;
                layers.masks[MaskCh::Path.idx()][z * res + x] = 255;
            }
        }
    }
    layers.dirty_since_apply = true;

    let off = world_offset(&w0.height);
    // Frame the whole scene: camera above the midpoint, looking across hill+clearing.
    let mid = ((hill.0 + clearing.0) * 0.5, (hill.1 + clearing.1) * 0.5);
    let dirx = clearing.0 - hill.0;
    let dirz = clearing.1 - hill.1;
    let len = (dirx * dirx + dirz * dirz).sqrt().max(1.0);
    let (px, pz) = (-dirz / len, dirx / len); // perpendicular
    let dist = len * 1.15 + 70.0;
    let eye_map = (mid.0 + px * dist, mid.1 + pz * dist);
    let eye = Vec3::new(
        eye_map.0 + off,
        w0.height.sample_world(eye_map.0, eye_map.1) + 95.0,
        eye_map.1 + off,
    );
    let target = Vec3::new(
        mid.0 + off,
        w0.height.sample_world(mid.0, mid.1) + 5.0,
        mid.1 + off,
    );
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(eye).looking_at(target, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }
    if mode == "apply" {
        state.tool = Tool::Off;
        state.apply_clicked = true;
    } else {
        // Leave the paint overlay + brush ring up for the shot.
        state.tool = Tool::Paint(MaskCh::Clearing);
        state.radius = 26.0;
        let hx = clearing.0 + off;
        let hz = clearing.1 + off;
        let hy = w0.height.sample_world(clearing.0, clearing.1);
        state.cursor_hit = Some(Vec3::new(hx, hy, hz));
        overlay.refresh = true;
    }
    info!(
        "editdemo staged ({mode}): hill at world ({:.0},{:.0}), clearing at ({:.0},{:.0})",
        hill.0 + off,
        hill.1 + off,
        clearing.0 + off,
        clearing.1 + off
    );
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .init_resource::<EditLayers>()
            .init_resource::<UndoStack>()
            .init_resource::<Stroke>()
            .init_resource::<OverlayAssets>()
            .init_resource::<KeepLayers>()
            .init_resource::<PendingProject>()
            .add_systems(
                Update,
                (
                    init_layers,
                    cursor_probe,
                    brush_shortcuts,
                    apply_stroke,
                    auto_apply_tick,
                    hotkeys,
                    draw_brush,
                    drive_overlay,
                    file_ops,
                    finish_load,
                )
                    .chain(),
            );
        if std::env::var("WED_EDITDEMO").is_ok() {
            app.add_systems(Update, stage_editdemo.after(init_layers).before(apply_stroke));
        }
    }
}

/// Snapshot the base heightfield when a (re)generated world lands.
fn init_layers(
    world: Option<Res<GeneratedWorld>>,
    base: Option<Res<crate::genrun::BaseCache>>,
    mut layers: ResMut<EditLayers>,
    mut undo: ResMut<UndoStack>,
    mut keep: ResMut<KeepLayers>,
    mut state: ResMut<EditorState>,
) {
    let Some(world) = world else { return };
    if !world.is_changed() {
        return;
    }
    if keep.0 {
        // Editor Apply/Load: the arriving world already INCLUDES the layers -- the base
        // stays the cached erosion result and the edit grids survive.
        keep.0 = false;
        if let Some(base) = base {
            layers.base_height = base.0.height.h.clone();
        }
        state.status = "applied".into();
        return;
    }
    let hf = &world.0.height;
    layers.res = hf.size;
    layers.base_height = hf.h.clone();
    layers.height_delta = vec![0.0; hf.size * hf.size];
    layers.masks = [
        vec![128u8; hf.size * hf.size], // forest density: 128 = neutral
        vec![0u8; hf.size * hf.size],
        vec![0u8; hf.size * hf.size],
        vec![0u8; hf.size * hf.size],
    ];
    layers.dirty_since_apply = false;
    undo.undo.clear();
    undo.redo.clear();
}

/// Ray-march the heightfield under the mouse cursor.
fn cursor_probe(
    world: Option<Res<GeneratedWorld>>,
    windows: Query<&Window>,
    cam: Query<(&Camera, &GlobalTransform), With<crate::flycam::FlyCam>>,
    mut state: ResMut<EditorState>,
    layers: Res<EditLayers>,
    cap: Res<crate::ui::UiInputCapture>,
) {
    state.cursor_hit = None;
    state.cursor_delta = 0.0;
    state.ui_hover = cap.pointer;
    if state.tool == Tool::Off || state.ui_hover {
        return;
    }
    let Some(world) = world else { return };
    let Ok(window) = windows.single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok((camera, cam_tf)) = cam.single() else { return };
    let Ok(ray) = camera.viewport_to_world(cam_tf, cursor) else { return };

    let hf = &world.0.height;
    let off = world_offset(hf);
    let sample = |p: Vec3| hf.sample_world(p.x - off, p.z - off);
    // Coarse march, then bisect the crossing interval.
    let mut t = 0.0f32;
    let mut prev = ray.origin;
    let mut prev_above = prev.y > sample(prev);
    while t < 900.0 {
        t += 1.5;
        let p = ray.origin + *ray.direction * t;
        let above = p.y > sample(p);
        if prev_above && !above {
            let (mut a, mut b) = (prev, p);
            for _ in 0..12 {
                let m = (a + b) * 0.5;
                if m.y > sample(m) {
                    a = m;
                } else {
                    b = m;
                }
            }
            let hit = (a + b) * 0.5;
            state.cursor_hit = Some(Vec3::new(hit.x, sample(hit), hit.z));
            if layers.res > 0 {
                let gx = ((hit.x - off) / hf.cell).round().clamp(0.0, (layers.res - 1) as f32);
                let gz = ((hit.z - off) / hf.cell).round().clamp(0.0, (layers.res - 1) as f32);
                state.cursor_delta = layers.height_delta[gz as usize * layers.res + gx as usize];
            }
            return;
        }
        prev = p;
        prev_above = above;
    }
}

/// Keyboard tool/size/strength shortcuts and scroll-to-resize. All gated so they never
/// fire while a panel field has focus (keyboard) or the pointer is over egui (wheel).
fn brush_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<MouseWheel>,
    buttons: Res<ButtonInput<MouseButton>>,
    cap: Res<crate::ui::UiInputCapture>,
    mut state: ResMut<EditorState>,
) {
    if !cap.keyboard {
        let tool = match () {
            _ if keys.just_pressed(KeyCode::Digit1) => Some(Tool::Off),
            _ if keys.just_pressed(KeyCode::Digit2) => Some(Tool::Raise),
            _ if keys.just_pressed(KeyCode::Digit3) => Some(Tool::Lower),
            _ if keys.just_pressed(KeyCode::Digit4) => Some(Tool::Smooth),
            _ if keys.just_pressed(KeyCode::Digit5) => Some(Tool::Flatten),
            _ if keys.just_pressed(KeyCode::Digit6) => Some(Tool::Paint(MaskCh::ALL[0])),
            _ if keys.just_pressed(KeyCode::Digit7) => Some(Tool::Paint(MaskCh::ALL[1])),
            _ if keys.just_pressed(KeyCode::Digit8) => Some(Tool::Paint(MaskCh::ALL[2])),
            _ if keys.just_pressed(KeyCode::Digit9) => Some(Tool::Paint(MaskCh::ALL[3])),
            _ => None,
        };
        if let Some(t) = tool {
            state.tool = t;
        }
        if keys.just_pressed(KeyCode::BracketLeft) {
            state.radius = (state.radius / 1.1).clamp(2.0, 80.0);
        }
        if keys.just_pressed(KeyCode::BracketRight) {
            state.radius = (state.radius * 1.1).clamp(2.0, 80.0);
        }
        if keys.just_pressed(KeyCode::Minus) {
            state.strength = (state.strength / 1.1).clamp(0.1, 4.0);
        }
        if keys.just_pressed(KeyCode::Equal) {
            state.strength = (state.strength * 1.1).clamp(0.1, 4.0);
        }
    }
    // Wheel ownership: RMB-hold keeps the fly-cam speed; otherwise an active tool takes the
    // wheel for brush size. flycam.rs mirrors this test so only one side acts on an event.
    let rmb = buttons.pressed(MouseButton::Right);
    if !cap.pointer && state.tool != Tool::Off && !rmb {
        for w in wheel.read() {
            state.radius = (state.radius * (1.0 + w.y * 0.1)).clamp(2.0, 80.0);
        }
    } else {
        wheel.clear();
    }
}

/// The stroke itself: applies the brush to the active layer, live-patches the
/// heightfield + chunk meshes, and books undo data.
#[allow(clippy::too_many_arguments)]
fn apply_stroke(
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<EditorState>,
    mut layers: ResMut<EditLayers>,
    mut stroke: ResMut<Stroke>,
    mut undo: ResMut<UndoStack>,
    mut world: Option<ResMut<GeneratedWorld>>,
    index: Res<GroundChunkIndex>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut overlay: ResMut<OverlayAssets>,
) {
    let Some(world) = world.as_mut() else { return };
    if layers.res == 0 || state.tool == Tool::Off {
        state.stroking = false;
        return;
    }
    // Framerate-independent, but cap the per-frame step so a stutter can't spike a stroke.
    let dt = time.delta_secs().min(0.05);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let lmb = buttons.pressed(MouseButton::Left) && !state.ui_hover;
    let rmb = buttons.pressed(MouseButton::Right) && !state.ui_hover;
    // Ctrl inverts (raise↔lower, paint↔erase); RMB is the same, and the two compose (XOR).
    let inverse = rmb ^ ctrl;
    // Shift temporarily smooths — sculpt only, so the stroke stays a Height command.
    let is_sculpt = !matches!(state.tool, Tool::Paint(_));
    let tool = if shift && is_sculpt { Tool::Smooth } else { state.tool };
    // Alt on the Flatten tool samples the target height without laying down a stroke.
    let alt_pick = alt && matches!(state.tool, Tool::Flatten);
    if alt_pick {
        if let (true, Some(hit)) = (lmb, state.cursor_hit) {
            state.flatten_target = hit.y;
            state.status = format!("flatten target {:.1} m", hit.y);
        }
        state.stroking = false;
        return;
    }
    let painting = lmb || rmb;

    if painting && !stroke.active {
        stroke.active = true;
        stroke.height_before.clear();
        stroke.mask_before.clear();
        stroke.rect = None;
        stroke.dirty_chunks.clear();
        state.auto_apply_in = 0.0; // a fresh stroke cancels any pending auto-apply
        if let Some(hit) = state.cursor_hit {
            state.flatten_target = hit.y;
        }
    }
    state.stroking = stroke.active;

    if stroke.active && !painting {
        // Stroke ends: fold the first-touch backups into one undo command.
        stroke.active = false;
        state.stroking = false;
        if state.auto_apply {
            state.auto_apply_in = 0.6;
        }
        if let Some(rect) = stroke.rect {
            let res = layers.res;
            match state.tool {
                Tool::Paint(ch) => {
                    let grid = &layers.masks[ch.idx()];
                    let mut before = Vec::with_capacity((rect.x1 - rect.x0) * (rect.z1 - rect.z0));
                    for z in rect.z0..rect.z1 {
                        for x in rect.x0..rect.x1 {
                            let i = z * res + x;
                            before.push(*stroke.mask_before.get(&i).unwrap_or(&grid[i]));
                        }
                    }
                    undo.undo.push(Cmd::Mask { ch, rect, before });
                }
                _ => {
                    let grid = &layers.height_delta;
                    let mut before = Vec::with_capacity((rect.x1 - rect.x0) * (rect.z1 - rect.z0));
                    for z in rect.z0..rect.z1 {
                        for x in rect.x0..rect.x1 {
                            let i = z * res + x;
                            before.push(*stroke.height_before.get(&i).unwrap_or(&grid[i]));
                        }
                    }
                    undo.undo.push(Cmd::Height { rect, before });
                }
            }
            undo.redo.clear();
            layers.dirty_since_apply = true;
        }
    }

    if !painting {
        return;
    }
    let Some(hit) = state.cursor_hit else { return };

    let w = world.bypass_change_detection();
    // GeneratedWorld holds an Arc (async-gen handoff); after gen lands the resource is
    // the sole owner, so get_mut succeeds. If some system briefly holds a clone, skip
    // the frame rather than stall.
    let Some(w0) = std::sync::Arc::get_mut(&mut w.0) else { return };
    let hf = &mut w0.height;
    let res = layers.res;
    let off = world_offset(hf);
    let cell = hf.cell;
    let cx = (hit.x - off) / cell;
    let cz = (hit.z - off) / cell;
    let r_cells = state.radius / cell;
    let x0 = ((cx - r_cells).floor().max(0.0)) as usize;
    let z0 = ((cz - r_cells).floor().max(0.0)) as usize;
    let x1 = (((cx + r_cells).ceil()) as usize + 1).min(res);
    let z1 = (((cz + r_cells).ceil()) as usize + 1).min(res);
    if x0 >= x1 || z0 >= z1 {
        return;
    }
    // Grow the stroke's dirty rect.
    stroke.rect = Some(match stroke.rect {
        None => Rect2u { x0, z0, x1, z1 },
        Some(r) => Rect2u {
            x0: r.x0.min(x0),
            z0: r.z0.min(z0),
            x1: r.x1.max(x1),
            z1: r.z1.max(z1),
        },
    });

    let falloff = state.falloff;
    let hardness = state.hardness;
    match tool {
        Tool::Paint(ch) => {
            let sign: f32 = if inverse { -1.0 } else { 1.0 };
            let grid = &mut layers.masks[ch.idx()];
            for z in z0..z1 {
                for x in x0..x1 {
                    let d = ((x as f32 - cx).powi(2) + (z as f32 - cz).powi(2)).sqrt();
                    if d > r_cells {
                        continue;
                    }
                    let fall = brush_fall(falloff, hardness, d / r_cells);
                    let i = z * res + x;
                    stroke.mask_before.entry(i).or_insert(grid[i]);
                    let add = sign * state.strength * 500.0 * dt * fall;
                    grid[i] = (grid[i] as f32 + add).clamp(0.0, 255.0) as u8;
                }
            }
            overlay.refresh = true;
        }
        tool => {
            let amount = state.strength * 8.0 * dt;
            for z in z0..z1 {
                for x in x0..x1 {
                    let d = ((x as f32 - cx).powi(2) + (z as f32 - cz).powi(2)).sqrt();
                    if d > r_cells {
                        continue;
                    }
                    let fall = brush_fall(falloff, hardness, d / r_cells);
                    let i = z * res + x;
                    stroke.height_before.entry(i).or_insert(layers.height_delta[i]);
                    let cur = layers.base_height[i] + layers.height_delta[i];
                    // Ctrl/RMB flips raise↔lower (Shift-smooth and flatten ignore it).
                    let inv = if inverse { -1.0 } else { 1.0 };
                    let delta = match tool {
                        Tool::Raise => inv * amount * fall,
                        Tool::Lower => -inv * amount * fall,
                        Tool::Flatten => (state.flatten_target - cur) * (3.0 * dt * fall).min(1.0),
                        Tool::Smooth => {
                            // Pull toward the 4-neighbour average of the DISPLAYED field.
                            let n = |xx: usize, zz: usize| {
                                let j = zz.min(res - 1) * res + xx.min(res - 1);
                                layers.base_height[j] + layers.height_delta[j]
                            };
                            let avg = (n(x.saturating_sub(1), z)
                                + n(x + 1, z)
                                + n(x, z.saturating_sub(1))
                                + n(x, z + 1))
                                * 0.25;
                            (avg - cur) * (6.0 * dt * fall).min(1.0)
                        }
                        _ => 0.0,
                    };
                    layers.height_delta[i] += delta;
                    hf.h[i] = layers.base_height[i] + layers.height_delta[i];
                }
            }
            // Book the touched chunks (±1 cell so border normals re-mesh too).
            let ch_x0 = x0.saturating_sub(1) / CHUNK;
            let ch_z0 = z0.saturating_sub(1) / CHUNK;
            let ch_x1 = x1.min(res - 1) / CHUNK;
            let ch_z1 = z1.min(res - 1) / CHUNK;
            for cz in ch_z0..=ch_z1 {
                for cx in ch_x0..=ch_x1 {
                    let key = (cx * CHUNK, cz * CHUNK);
                    if !stroke.dirty_chunks.contains(&key) {
                        stroke.dirty_chunks.push(key);
                    }
                }
            }
        }
    }

    // Throttled live re-mesh of dirty chunks (in place — same handles, no respawn).
    stroke.rebuild_timer -= dt;
    if stroke.rebuild_timer <= 0.0 && !stroke.dirty_chunks.is_empty() {
        stroke.rebuild_timer = 0.08;
        for key in stroke.dirty_chunks.drain(..) {
            if let Some((full, coarse)) = index.0.get(&key) {
                let _ = meshes.insert(full.id(), build_chunk(w0, key.0, key.1, 1));
                let _ = meshes.insert(coarse.id(), build_chunk(w0, key.0, key.1, 4));
            }
        }
    }
}

/// Live scatter refresh: a debounce after the last stroke ends fires the same Apply path
/// the toolbar button does, so trees/grass/water re-conform to the sculpted terrain within
/// ~a second. A single timer means back-to-back strokes never queue a storm of applies.
fn auto_apply_tick(
    time: Res<Time>,
    mut state: ResMut<EditorState>,
    layers: Res<EditLayers>,
    stroke: Res<Stroke>,
    progress: Res<crate::genrun::GenProgress>,
) {
    if state.auto_apply_in <= 0.0 || stroke.active {
        return;
    }
    state.auto_apply_in -= time.delta_secs();
    if state.auto_apply_in > 0.0 {
        return;
    }
    if progress.running {
        // A generation is still in flight — hold off and retry shortly.
        state.auto_apply_in = 0.2;
        return;
    }
    state.auto_apply_in = 0.0;
    if layers.dirty_since_apply {
        state.apply_clicked = true;
    }
}

/// Ctrl+Z / Ctrl+Y (or Ctrl+Shift+Z).
#[allow(clippy::too_many_arguments)]
fn hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<EditorState>,
    mut undo: ResMut<UndoStack>,
    mut layers: ResMut<EditLayers>,
    mut world: Option<ResMut<GeneratedWorld>>,
    index: Res<GroundChunkIndex>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut overlay: ResMut<OverlayAssets>,
) {
    // Toolbar Undo/Redo buttons set these intents; keyboard adds Ctrl+Z / Ctrl+Y.
    let btn_undo = std::mem::take(&mut state.undo_clicked);
    let btn_redo = std::mem::take(&mut state.redo_clicked);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let key_undo = ctrl && keys.just_pressed(KeyCode::KeyZ) && !shift;
    let key_redo =
        ctrl && (keys.just_pressed(KeyCode::KeyY) || (keys.just_pressed(KeyCode::KeyZ) && shift));
    let do_undo = btn_undo || key_undo;
    let do_redo = btn_redo || key_redo;
    if layers.res == 0 || (!do_undo && !do_redo) {
        return;
    }
    let Some(world) = world.as_mut() else { return };
    if do_undo {
        if let Some(cmd) = undo.undo.pop() {
            match apply_cmd(cmd, &mut layers, world, &index, &mut meshes, &mut overlay) {
                Ok(inv) => undo.redo.push(inv),
                Err(orig) => undo.undo.push(orig),
            }
        }
    } else if let Some(cmd) = undo.redo.pop() {
        match apply_cmd(cmd, &mut layers, world, &index, &mut meshes, &mut overlay) {
            Ok(inv) => undo.undo.push(inv),
            Err(orig) => undo.redo.push(orig),
        }
    }
}

/// Swap a diff into the grids; Ok(inverse) on success, Err(original) if the world was
/// briefly unavailable (Arc shared) — the caller puts the command back untouched.
fn apply_cmd(
    cmd: Cmd,
    layers: &mut EditLayers,
    world: &mut ResMut<GeneratedWorld>,
    index: &GroundChunkIndex,
    meshes: &mut Assets<Mesh>,
    overlay: &mut OverlayAssets,
) -> Result<Cmd, Cmd> {
    let res = layers.res;
    match cmd {
        Cmd::Height { rect, before } => {
            let mut now = Vec::with_capacity(before.len());
            let w = world.bypass_change_detection();
            let Some(w0) = std::sync::Arc::get_mut(&mut w.0) else {
                return Err(Cmd::Height { rect, before });
            };
            let mut k = 0;
            for z in rect.z0..rect.z1 {
                for x in rect.x0..rect.x1 {
                    let i = z * res + x;
                    now.push(layers.height_delta[i]);
                    layers.height_delta[i] = before[k];
                    w0.height.h[i] = layers.base_height[i] + before[k];
                    k += 1;
                }
            }
            for cz in (rect.z0.saturating_sub(1) / CHUNK)..=(rect.z1.min(res - 1) / CHUNK) {
                for cx in (rect.x0.saturating_sub(1) / CHUNK)..=(rect.x1.min(res - 1) / CHUNK) {
                    if let Some((full, coarse)) = index.0.get(&(cx * CHUNK, cz * CHUNK)) {
                        let _ = meshes.insert(full.id(), build_chunk(w0, cx * CHUNK, cz * CHUNK, 1));
                        let _ = meshes.insert(coarse.id(), build_chunk(w0, cx * CHUNK, cz * CHUNK, 4));
                    }
                }
            }
            layers.dirty_since_apply = true;
            Ok(Cmd::Height { rect, before: now })
        }
        Cmd::Mask { ch, rect, before } => {
            let grid = &mut layers.masks[ch.idx()];
            let mut now = Vec::with_capacity(before.len());
            let mut k = 0;
            for z in rect.z0..rect.z1 {
                for x in rect.x0..rect.x1 {
                    let i = z * res + x;
                    now.push(grid[i]);
                    grid[i] = before[k];
                    k += 1;
                }
            }
            overlay.refresh = true;
            layers.dirty_since_apply = true;
            Ok(Cmd::Mask { ch, rect, before: now })
        }
    }
}

/// Brush ring gizmo following the terrain: an outer ring at `radius`, an inner ring at the
/// hardness plateau, and (for Flatten) a centre tick at the target height. The ring colour
/// flips live to signal the invert modifier (Ctrl / RMB) before the user commits a stroke.
fn draw_brush(
    state: Res<EditorState>,
    world: Option<Res<GeneratedWorld>>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut gizmos: Gizmos,
) {
    let (Some(hit), Some(world)) = (state.cursor_hit, world) else { return };
    if state.tool == Tool::Off {
        return;
    }
    let hf = &world.0.height;
    let off = world_offset(hf);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let inverting = ctrl ^ buttons.pressed(MouseButton::Right);
    let base = match state.tool {
        Tool::Lower => Color::srgb(1.0, 0.45, 0.3),
        Tool::Paint(ch) => {
            let t = ch.tint();
            Color::srgb(t[0], t[1], t[2])
        }
        _ => Color::srgb(0.3, 0.9, 1.0),
    };
    // Invert flips the ring to a warning orange so the mode reads before clicking.
    let col = if inverting { Color::srgb(1.0, 0.5, 0.15) } else { base };
    let draped_ring = |gizmos: &mut Gizmos, radius: f32, col: Color| {
        let n = 48;
        let mut prev: Option<Vec3> = None;
        for k in 0..=n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let x = hit.x + a.cos() * radius;
            let z = hit.z + a.sin() * radius;
            let y = hf.sample_world(x - off, z - off) + 0.25;
            let p = Vec3::new(x, y, z);
            if let Some(q) = prev {
                gizmos.line(q, p, col);
            }
            prev = Some(p);
        }
    };
    draped_ring(&mut gizmos, state.radius, col);
    if state.hardness > 0.05 {
        let inner = col.with_alpha(0.5);
        draped_ring(&mut gizmos, state.radius * state.hardness, inner);
    }
    // Flatten: a short vertical tick from the ground up to the target height at the centre.
    if state.tool == Tool::Flatten {
        let base_pt = Vec3::new(hit.x, hit.y, hit.z);
        let tip = Vec3::new(hit.x, state.flatten_target + 0.25, hit.z);
        gizmos.line(base_pt, tip, Color::srgb(1.0, 0.95, 0.4));
    }
}

/// Keep the mask overlay quad + texture in sync with the active paint channel.
fn drive_overlay(
    state: Res<EditorState>,
    layers: Res<EditLayers>,
    world: Option<Res<GeneratedWorld>>,
    mut overlay: ResMut<OverlayAssets>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let want = match state.tool {
        Tool::Paint(ch) => Some(ch),
        _ => None,
    };
    if want == overlay.shown_ch && !overlay.refresh {
        return;
    }
    let Some(world) = world else { return };
    if layers.res == 0 {
        return;
    }
    let res = layers.res;

    // (Re)build the overlay image for the active channel.
    if let Some(ch) = want {
        let tint = ch.tint();
        let grid = &layers.masks[ch.idx()];
        let mut px = vec![0u8; res * res * 4];
        for i in 0..res * res {
            let w = grid[i] as f32 / 255.0;
            // Forest is neutral at 0.5 — show boost green, suppression red-ish.
            let (c, a) = if ch == MaskCh::Forest {
                if w >= 0.5 {
                    (tint, (w - 0.5) * 2.0)
                } else {
                    ([0.9, 0.3, 0.1], (0.5 - w) * 2.0)
                }
            } else {
                (tint, w)
            };
            px[i * 4] = (c[0] * 255.0) as u8;
            px[i * 4 + 1] = (c[1] * 255.0) as u8;
            px[i * 4 + 2] = (c[2] * 255.0) as u8;
            px[i * 4 + 3] = (a * 150.0) as u8;
        }
        let image = Image::new(
            bevy::render::render_resource::Extent3d {
                width: res as u32,
                height: res as u32,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            px,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        if overlay.entity.is_none() {
            let img_h = images.add(image);
            overlay.image = img_h.clone();
            // A terrain-hugging overlay would need a draped mesh; a big quad slightly
            // above the highest point reads well enough for painting from the air, but
            // draping matters when painting from ground level — so drape it: reuse the
            // heightfield as a coarse mesh with the overlay texture.
            let mesh = overlay_mesh(&world.0);
            let mat = mats.add(StandardMaterial {
                base_color_texture: Some(img_h),
                base_color: Color::srgba(1.0, 1.0, 1.0, 1.0),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                depth_bias: 60.0,
                cull_mode: None,
                ..default()
            });
            let e = commands
                .spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(mat),
                    Transform::default(),
                    NoFrustumCulling,
                    NotShadowCaster,
                    MaskOverlay,
                ))
                .id();
            overlay.entity = Some(e);
        } else {
            let _ = images.insert(overlay.image.id(), image);
        }
        if let Some(e) = overlay.entity {
            commands.entity(e).insert(Visibility::Visible);
        }
    } else if let Some(e) = overlay.entity {
        commands.entity(e).insert(Visibility::Hidden);
    }
    overlay.shown_ch = want;
    overlay.refresh = false;
}

/// Coarse draped mesh (stride 8) over the whole map, UV = grid coords, lifted 0.4 m.
fn overlay_mesh(w: &worldgen::World) -> Mesh {
    use bevy::mesh::{Indices, PrimitiveTopology};
    let hf = &w.height;
    let size = hf.size;
    let off = world_offset(hf);
    const STRIDE: usize = 8;
    let side = size / STRIDE + 1;
    let mut positions = Vec::with_capacity(side * side);
    let mut uvs = Vec::with_capacity(side * side);
    let mut indices = Vec::with_capacity((side - 1) * (side - 1) * 6);
    for vz in 0..side {
        for vx in 0..side {
            let gx = (vx * STRIDE).min(size - 1);
            let gz = (vz * STRIDE).min(size - 1);
            positions.push([
                gx as f32 * hf.cell + off,
                hf.get(gx, gz) + 0.4,
                gz as f32 * hf.cell + off,
            ]);
            uvs.push([gx as f32 / size as f32, gz as f32 / size as f32]);
        }
    }
    for vz in 0..side - 1 {
        for vx in 0..side - 1 {
            let a = (vz * side + vx) as u32;
            let b = a + 1;
            let c = a + side as u32;
            indices.extend_from_slice(&[a, c, b, b, c, a + side as u32 + 1]);
        }
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    let n = positions.len();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}
