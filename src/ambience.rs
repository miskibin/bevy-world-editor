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

/// User-facing mixer (panel sliders): master + per-loop multipliers.
#[derive(Resource)]
pub struct AudioSettings {
    pub master: f32,
    pub birds: f32,
    pub water: f32,
    pub wind: f32,
    pub forest: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings { master: 1.0, birds: 1.0, water: 1.0, wind: 1.0, forest: 1.0 }
    }
}

pub struct AmbiencePlugin;

impl Plugin for AmbiencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WaterProximity>()
            .init_resource::<AudioSettings>()
            .add_systems(Startup, spawn_loops)
            .add_systems(Update, (rebuild_proximity, drive_volumes));
    }
}

fn spawn_loops(mut commands: Commands, asset: Res<AssetServer>) {
    // Capture harness runs silent: a screenshot/clip run should never blast the
    // ambience (and it skips decoding the multi-MB birdsong mp3 too).
    if std::env::var("WED_SHOT").is_ok() || std::env::var("WED_CLIP").is_ok() {
        info!("ambience: muted (capture harness)");
        return;
    }
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
    s: Res<AudioSettings>,
    cam: Query<&Transform, With<Camera3d>>,
    mut sinks: Query<(&Loop, &mut AudioSink)>,
    time: Res<Time>,
) {
    let (Some(world), Ok(cam)) = (world, cam.single()) else { return };
    let hf = &world.0.height;
    let p = cam.translation;
    let ground = hf.sample_world(p.x - prox.offset, p.z - prox.offset);
    let above = (p.y - ground).max(0.0);

    // Water proximity factor (also gates the wind — user verdict: wind belongs by the
    // water and much quieter overall).
    let water_near = if prox.size > 0 {
        let gx = ((p.x - prox.offset) / prox.cell).clamp(0.0, (prox.size - 1) as f32) as usize;
        let gz = ((p.z - prox.offset) / prox.cell).clamp(0.0, (prox.size - 1) as f32) as usize;
        let d = prox.dist[gz * prox.size + gx].min(WATER_HEAR);
        (1.0 - d / WATER_HEAR).powf(1.4)
    } else {
        0.0
    };
    // 0.42 (was 0.85) — user: water and wind 50% quieter.
    let water_v = water_near * (1.0 - (above / 60.0).clamp(0.0, 1.0)) * 0.42 * s.water;
    // Birds + forest bed: strongest near the ground, gone high above the canopy.
    let low = 1.0 - ((above - 12.0) / 70.0).clamp(0.0, 1.0);
    let birds_v = 0.5 * low * s.birds;
    let forest_v = 0.28 * low * s.forest;
    // Wind: quiet, mostly an open-water breeze + a whisper at real altitude.
    let wind_v =
        (0.008 + water_near * 0.07 + (above / 150.0).clamp(0.0, 1.0) * 0.04) * s.wind;

    // Ease volumes so flying between zones swells instead of snapping.
    let k = (time.delta_secs() * 2.2).min(1.0);
    for (kind, mut sink) in &mut sinks {
        let target = match kind {
            Loop::Birds => birds_v,
            Loop::Water => water_v,
            Loop::Wind => wind_v,
            Loop::Forest => forest_v,
        };
        let target = target * s.master;
        let cur = sink.volume().to_linear();
        sink.set_volume(Volume::Linear(cur + (target - cur) * k));
    }
}
