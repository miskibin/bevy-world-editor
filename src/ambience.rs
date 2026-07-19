//! Ambient soundscape: three non-spatial loops whose volumes track the camera —
//! birdsong under/near the canopy, lapping water near lakes, wind that grows with
//! altitude. Sources: birds.mp3 (freesound, user-supplied), water/wind/forest .ogg
//! reused from Warbell.

use bevy::audio::{AudioSink, AudioSource, PlaybackMode, PlaybackSettings, Volume};
use bevy::prelude::*;

use crate::genrun::GeneratedWorld;

#[derive(Component, Clone, Copy, PartialEq)]
enum Loop {
    Birds,
    Water,
    Wind,
    Forest,
}

/// Per-cell distance (metres, capped) from dry land cells to the nearest water — drives
/// the water-loop volume. Rebuilt per generated world.
#[derive(Resource, Default)]
struct WaterProximity {
    dist: Vec<f32>,
    size: usize,
    cell: f32,
    offset: f32,
}

const WATER_HEAR: f32 = 55.0;

pub struct AmbiencePlugin;

impl Plugin for AmbiencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WaterProximity>()
            .add_systems(Startup, spawn_loops)
            .add_systems(Update, (rebuild_proximity, drive_volumes));
    }
}

fn spawn_loops(mut commands: Commands, asset: Res<AssetServer>) {
    // [ambient loops — no transcripts: birdsong / lake lapping / wind / forest bed]
    let beds = [
        (Loop::Birds, "audio/birds.mp3"),
        (Loop::Water, "audio/water.ogg"),
        (Loop::Wind, "audio/wind.ogg"),
        (Loop::Forest, "audio/forest-ambient.ogg"),
    ];
    for (kind, file) in beds {
        commands.spawn((
            AudioPlayer(asset.load::<AudioSource>(file)),
            PlaybackSettings {
                mode: PlaybackMode::Loop,
                volume: Volume::Linear(0.0),
                spatial: false,
                ..default()
            },
            kind,
        ));
    }
}

/// BFS from every water cell outward over land, capped at hearing range.
fn rebuild_proximity(world: Option<Res<GeneratedWorld>>, mut prox: ResMut<WaterProximity>) {
    let Some(world) = world else { return };
    if !world.is_changed() {
        return;
    }
    let hf = &world.0.height;
    let size = hf.size;
    let mut dist = vec![f32::MAX; size * size];
    let mut queue = std::collections::VecDeque::new();
    for i in 0..size * size {
        if world.0.water[i].is_finite() {
            dist[i] = 0.0;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        let d = dist[i];
        if d > WATER_HEAR {
            continue;
        }
        let x = i % size;
        let z = i / size;
        for (nx, nz) in [(x.wrapping_sub(1), z), (x + 1, z), (x, z.wrapping_sub(1)), (x, z + 1)] {
            if nx >= size || nz >= size {
                continue;
            }
            let ni = nz * size + nx;
            if dist[ni] > d + hf.cell {
                dist[ni] = d + hf.cell;
                queue.push_back(ni);
            }
        }
    }
    prox.dist = dist;
    prox.size = size;
    prox.cell = hf.cell;
    prox.offset = crate::genrun::world_offset(hf);
}

fn drive_volumes(
    world: Option<Res<GeneratedWorld>>,
    prox: Res<WaterProximity>,
    cam: Query<&Transform, With<Camera3d>>,
    mut sinks: Query<(&Loop, &mut AudioSink)>,
    time: Res<Time>,
) {
    let (Some(world), Ok(cam)) = (world, cam.single()) else { return };
    let hf = &world.0.height;
    let p = cam.translation;
    let ground = hf.sample_world(p.x - prox.offset, p.z - prox.offset);
    let above = (p.y - ground).max(0.0);

    // Water: proximity in the map plane, faded by height above the terrain.
    let water_v = if prox.size > 0 {
        let gx = ((p.x - prox.offset) / prox.cell).clamp(0.0, (prox.size - 1) as f32) as usize;
        let gz = ((p.z - prox.offset) / prox.cell).clamp(0.0, (prox.size - 1) as f32) as usize;
        let d = prox.dist[gz * prox.size + gx].min(WATER_HEAR);
        (1.0 - d / WATER_HEAR).powf(1.4) * (1.0 - (above / 60.0).clamp(0.0, 1.0)) * 0.85
    } else {
        0.0
    };
    // Birds + forest bed: strongest near the ground, gone high above the canopy.
    let low = 1.0 - ((above - 12.0) / 70.0).clamp(0.0, 1.0);
    let birds_v = 0.5 * low;
    let forest_v = 0.28 * low;
    // Wind: takes over with altitude.
    let wind_v = 0.10 + (above / 110.0).clamp(0.0, 1.0) * 0.55;

    // Ease volumes so flying between zones swells instead of snapping.
    let k = (time.delta_secs() * 2.2).min(1.0);
    for (kind, mut sink) in &mut sinks {
        let target = match kind {
            Loop::Birds => birds_v,
            Loop::Water => water_v,
            Loop::Wind => wind_v,
            Loop::Forest => forest_v,
        };
        let cur = sink.volume().to_linear();
        sink.set_volume(Volume::Linear(cur + (target - cur) * k));
    }
}
