//! Async world-generation orchestration: runs the pure `worldgen` pipeline on the compute
//! task pool, reports progress, and republishes the finished world to the spawning
//! modules via the `WorldReady` message. Regenerate = despawn everything tagged
//! `WorldEntity` + rebuild from the fresh `GeneratedWorld`.

use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, futures_lite::future};
use std::sync::{Arc, Mutex};

pub const WATER_LEVEL: f32 = 8.0;

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

/// Fired when a fresh world is ready — spawn modules rebuild on it.
#[derive(Message)]
pub struct WorldReady;

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
            .add_message::<WorldReady>()
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

// SystemParam-free wrapper so ui.rs can call regenerate without knowing GenTask.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Regen<'w> {
    params: Res<'w, GenParams>,
    task: ResMut<'w, GenTask>,
    progress: ResMut<'w, GenProgress>,
}

impl Regen<'_> {
    pub fn fire(&mut self) {
        request_regenerate(&self.params, &mut self.task, &mut self.progress);
    }
}

fn poll_gen(
    mut commands: Commands,
    mut task: ResMut<GenTask>,
    mut progress: ResMut<GenProgress>,
    mut ready: MessageWriter<WorldReady>,
    old: Query<Entity, With<WorldEntity>>,
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
        commands.insert_resource(GeneratedWorld(Arc::new(world)));
        ready.write(WorldReady);
    }
}
