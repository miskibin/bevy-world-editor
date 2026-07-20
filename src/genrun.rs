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

impl Default for GenParams {
    fn default() -> Self {
        let mut p = worldgen::WorldParams::default();
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

// NB: spawn modules trigger on `Res<GeneratedWorld>` change detection, not a message —
// a message written the frame the resource is queued races the readers (consumed, then
// the resource isn't there yet; next frame the message is gone).

#[derive(Resource, Default)]
pub struct GenProgress {
    pub running: bool,
    pub fraction: f32,
    pub stage: String,
}

#[derive(Resource, Default)]
struct GenTask(Option<(Task<worldgen::World>, Arc<Mutex<(f32, String)>>)>);

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
        worldgen::generate(&p, |f, stage| {
            if let Ok(mut s) = shared2.lock() {
                s.0 = f;
                s.1 = stage.to_string();
            }
        })
    });
    task.0 = Some((t, shared));
    progress.running = true;
    progress.fraction = 0.0;
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
    if let Some(world) = block_on(future::poll_once(t)) {
        task.0 = None;
        progress.running = false;
        progress.fraction = 1.0;
        for e in &old {
            commands.entity(e).despawn();
        }
        info!(
            "world ready: {} trees, {}x{} cells",
            world.trees.len(),
            world.height.size,
            world.height.size
        );
        // WED_EYE="x,z,h,tx,tz": eye at terrain height + h over MAP-space (x,z), looking
        // at terrain level over (tx,tz) — ground-truth staging without knowing heights.
        if let Some(v) = std::env::var("WED_EYE").ok().map(|s| {
            s.split(',').filter_map(|p| p.trim().parse::<f32>().ok()).collect::<Vec<_>>()
        }) {
            if v.len() == 5 {
                let hf = &world.height;
                let off = world_offset(hf);
                let eye =
                    Vec3::new(v[0], hf.sample_world(v[0] - off, v[1] - off) + v[2], v[1]);
                let target =
                    Vec3::new(v[3], hf.sample_world(v[3] - off, v[4] - off) + v[2], v[4]);
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
        commands.insert_resource(GeneratedWorld(Arc::new(world)));
    }
}
