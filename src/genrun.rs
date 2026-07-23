//! Async world-generation orchestration: runs the pure `worldgen` pipeline on the compute
//! task pool, reports progress, and republishes the finished world to the spawning
//! modules via the `WorldReady` message. Regenerate = despawn everything tagged
//! `WorldEntity` + rebuild from the fresh `GeneratedWorld`.

use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, futures_lite::future};
use std::sync::{Arc, Mutex};

// NB: keep in sync with worldgen ForestParams::default — this const also feeds the
// terrain material's wet band. (It silently overrode the new 5.0 default back to 8.)
pub const WATER_LEVEL: f32 = 5.0;

/// Half the map side — the world is spawned centred on the origin.
pub fn world_offset(hf: &worldgen::HeightField) -> f32 {
    -hf.extent() * 0.5
}

/// Tag for every spawned world entity (terrain chunks, trees, water) — regenerate sweeps these.
#[derive(Component)]
pub struct WorldEntity;

#[derive(Resource, Clone)]
pub struct GenParams(pub worldgen::WorldParams);

/// The editor opens on a blank flat map. The screenshot / profile / edit-demo harnesses
/// still expect the procedural forest at boot, so those keep the full procedural default.
fn boots_procedural() -> bool {
    ["WED_SHOT", "WED_CLIP", "WED_PROFILE", "WED_EDITDEMO", "WED_PROCGEN"]
        .iter()
        .any(|k| std::env::var(k).is_ok())
}

/// The visual harnesses (screenshots/clips/profile/edit-demo) were tuned for the full
/// cinematic post pipeline, so when any is set we keep the old pretty defaults — SSAO,
/// bloom, god rays, DoF, haze, supersampling — so their output stays comparable. The
/// interactive editor otherwise boots with all heavy/cosmetic post OFF (fast, responsive
/// edit-mode viewport; every effect stays available as an opt-in toggle).
pub fn cinematic_defaults() -> bool {
    boots_procedural()
}

impl Default for GenParams {
    fn default() -> Self {
        let mut p = if boots_procedural() {
            worldgen::WorldParams::default()
        } else {
            // Blank flat editor canvas: 512 m plane, scatter off, near-instant regen.
            worldgen::WorldParams::flat(512)
        };
        p.forest.water_level = WATER_LEVEL;
        if let Ok(seed) = std::env::var("WED_SEED") {
            if let Ok(s) = seed.trim().parse::<u32>() {
                p.terrain.seed = s;
                p.forest.seed = s;
            }
        }
        GenParams(p)
    }
}

/// The last finished world, shared with meshing/scatter systems.
#[derive(Resource, Clone)]
pub struct GeneratedWorld(pub Arc<worldgen::World>);

/// Cached expensive stage (landforms + erosion) — the editor's Apply re-runs only the
/// cheap downstream half (lakes/maps/trails/scatter) against this.
#[derive(Resource, Clone)]
pub struct BaseCache(pub Arc<worldgen::BaseFields>);

// NB: spawn modules trigger on `Res<GeneratedWorld>` change detection, not a message —
// a message written the frame the resource is queued races the readers (consumed, then
// the resource isn't there yet; next frame the message is gone).

#[derive(Resource, Default)]
pub struct GenProgress {
    pub running: bool,
    pub fraction: f32,
    pub stage: String,
}

enum GenOut {
    /// Fresh generation: new base + world. Repositions the camera.
    Fresh(worldgen::BaseFields, worldgen::World),
    /// Editor Apply/Load against a cached base: world only, camera stays put.
    Applied(worldgen::World),
}

#[derive(Resource, Default)]
struct GenTask(Option<(Task<GenOut>, Arc<Mutex<(f32, String)>>)>);

pub struct GenPlugin;

impl Plugin for GenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GenParams>()
            .init_resource::<GenProgress>()
            .init_resource::<GenTask>()
            .add_systems(Startup, kick_initial)
            .add_systems(Update, poll_gen);
    }
}

/// Start generating (used at boot and by the UI's Regenerate button).
fn start_generation(params: &GenParams, task: &mut GenTask, progress: &mut GenProgress) {
    let p = params.0;
    let shared = Arc::new(Mutex::new((0.0f32, String::from("landforms"))));
    let shared2 = shared.clone();
    let t = AsyncComputeTaskPool::get().spawn(async move {
        let mut cb = |f: f32, stage: &str| {
            if let Ok(mut s) = shared2.lock() {
                s.0 = f;
                s.1 = stage.to_string();
            }
        };
        // Staged so the erosion result can be cached for the editor's fast Apply.
        let base = worldgen::generate_base(&p, &mut cb);
        let project = worldgen::Project {
            format_version: 1,
            name: String::new(),
            params: p,
            layers: Vec::new(),
            rasters: Default::default(),
        };
        let world = worldgen::build_world_from_base(&project, &base, &mut cb);
        GenOut::Fresh(base, world)
    });
    task.0 = Some((t, shared));
    progress.running = true;
    progress.fraction = 0.0;
}

/// The editor's Apply/Load: rebuild the cheap downstream half against a cached base.
fn start_apply(project: worldgen::Project, base: Arc<worldgen::BaseFields>, task: &mut GenTask, progress: &mut GenProgress) {
    let shared = Arc::new(Mutex::new((0.75f32, String::from("apply"))));
    let shared2 = shared.clone();
    let t = AsyncComputeTaskPool::get().spawn(async move {
        let mut cb = |f: f32, stage: &str| {
            if let Ok(mut s) = shared2.lock() {
                s.0 = f;
                s.1 = stage.to_string();
            }
        };
        GenOut::Applied(worldgen::build_world_from_base(&project, &base, &mut cb))
    });
    task.0 = Some((t, shared));
    progress.running = true;
}

fn kick_initial(params: Res<GenParams>, mut task: ResMut<GenTask>, mut progress: ResMut<GenProgress>) {
    start_generation(&params, &mut task, &mut progress);
}

/// Relaunch with the current params (the UI's Regenerate path, via `Regen`).
fn request_regenerate(
    params: &GenParams,
    task: &mut GenTask,
    progress: &mut GenProgress,
) {
    // A still-running task is detached and dropped — its result is stale either way.
    start_generation(params, task, progress);
}

// Wrapper so ui.rs can trigger a regenerate without knowing GenTask. Deliberately does
// NOT hold GenParams — the panel already borrows it mutably (B0002 otherwise).
#[derive(bevy::ecs::system::SystemParam)]
pub struct Regen<'w> {
    task: ResMut<'w, GenTask>,
    progress: ResMut<'w, GenProgress>,
}

impl Regen<'_> {
    pub fn fire(&mut self, params: &GenParams) {
        request_regenerate(params, &mut self.task, &mut self.progress);
    }

    /// Editor Apply: downstream rebuild only, camera untouched.
    pub fn fire_apply(&mut self, project: worldgen::Project, base: Arc<worldgen::BaseFields>) {
        start_apply(project, base, &mut self.task, &mut self.progress);
    }

    // The panel reads progress through here too — holding its own Res<GenProgress>
    // alongside this param is a B0002 access conflict.
    pub fn running(&self) -> bool {
        self.progress.running
    }

    pub fn fraction(&self) -> f32 {
        self.progress.fraction
    }

    pub fn stage(&self) -> String {
        self.progress.stage.clone()
    }
}

fn poll_gen(
    mut commands: Commands,
    mut task: ResMut<GenTask>,
    mut progress: ResMut<GenProgress>,
    old: Query<Entity, With<WorldEntity>>,
    mut cam: Query<(&mut Transform, &mut crate::flycam::FlyCam)>,
) {
    let Some((t, shared)) = task.0.as_mut() else { return };
    if let Ok(s) = shared.lock() {
        progress.fraction = s.0;
        progress.stage = s.1.clone();
    }
    if let Some(out) = block_on(future::poll_once(t)) {
        task.0 = None;
        progress.running = false;
        progress.fraction = 1.0;
        for e in &old {
            commands.entity(e).despawn();
        }
        let (world, reposition) = match out {
            GenOut::Fresh(base, world) => {
                commands.insert_resource(BaseCache(Arc::new(base)));
                (world, true)
            }
            GenOut::Applied(world) => (world, false),
        };
        info!(
            "world ready: {} trees, {}x{} cells",
            world.trees.len(),
            world.height.size,
            world.height.size
        );
        // WED_EYE="x,z,h,tx,tz[,th]": eye at terrain height + h over WORLD-space (x,z)
        // — the map is centred on the origin, so x/z run -extent/2 .. +extent/2 —
        // looking at terrain height + th (default h) over (tx,tz). Pass a smaller `th`
        // to aim DOWN at the ground — with th == h the view is dead horizontal, which
        // is useless for judging ground detail.
        if reposition {
        if let Some(v) = std::env::var("WED_EYE").ok().map(|s| {
            s.split(',').filter_map(|p| p.trim().parse::<f32>().ok()).collect::<Vec<_>>()
        }) {
            if v.len() >= 5 {
                let hf = &world.height;
                let off = world_offset(hf);
                let th = if v.len() >= 6 { v[5] } else { v[2] };
                let eye =
                    Vec3::new(v[0], hf.sample_world(v[0] - off, v[1] - off) + v[2], v[1]);
                let target =
                    Vec3::new(v[3], hf.sample_world(v[3] - off, v[4] - off) + th, v[4]);
                for (mut tf, mut fc) in &mut cam {
                    *tf = Transform::from_translation(eye).looking_at(target, Vec3::Y);
                    let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
                    fc.yaw = yaw;
                    fc.pitch = pitch;
                }
            }
        } else
        // Drop the fly-cam at a sane vantage above the fresh terrain — unless WED_CAM
        // staged an explicit pose for a screenshot.
        if std::env::var("WED_CAM").is_err() {
            let hf = &world.height;
            let off = world_offset(hf);
            let ext = hf.extent();
            let (cx, cz) = (ext * 0.5, ext * 0.72);
            let eye = Vec3::new(
                cx + off,
                hf.sample_world(cx, cz) + 45.0,
                cz + off,
            );
            let target = Vec3::new(0.0, hf.sample_world(ext * 0.5, ext * 0.5) + 20.0, 0.0);
            for (mut tf, mut fc) in &mut cam {
                *tf = Transform::from_translation(eye).looking_at(target, Vec3::Y);
                let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
                fc.yaw = yaw;
                fc.pitch = pitch;
            }
        }
        }
        commands.insert_resource(GeneratedWorld(Arc::new(world)));
    }
}
