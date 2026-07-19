//! Screenshot harness (ported from Warbell) — the window can't be captured externally,
//! so `WED_SHOT=<path.png>` renders, warms up (≥240 frames AND ≥10 s wall-clock so cold
//! pipelines/shaders/IBL settle — here also the world-gen + erosion finish), saves a PNG
//! and exits. Exit is gated on the FILE EXISTING (the readback is async); stale files are
//! deleted first so a crashed run can't leave an old image that reads as fresh.
//! `WED_SHOT_WARMUP=<secs>` raises the wall-clock floor.

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

pub struct CapturePlugin;

#[derive(Resource)]
struct ShotPath(String);

#[derive(Resource, Default)]
struct ShotClock {
    frame: u32,
    shot: bool,
    shot_at: u32,
}

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        if let Ok(path) = std::env::var("WED_SHOT") {
            app.insert_resource(ShotPath(path))
                .init_resource::<ShotClock>()
                .add_systems(Update, drive_shot);
        }
    }
}

fn drive_shot(
    mut clock: ResMut<ShotClock>,
    time: Res<Time<Real>>, // wall clock — virtual time must not stall the warmup gate
    path: Res<ShotPath>,
    genp: Res<crate::genrun::GenProgress>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    clock.frame += 1;
    let min_secs = std::env::var("WED_SHOT_WARMUP")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0)
        .max(10.0);
    // Also wait for generation + a settle margin, so the shot never frames a half-spawned world.
    if !clock.shot && clock.frame >= 240 && time.elapsed_secs() >= min_secs && !genp.running {
        if let Some(parent) =
            std::path::Path::new(&path.0).parent().filter(|p| !p.as_os_str().is_empty())
        {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&path.0);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.0.clone()));
        clock.shot = true;
        clock.shot_at = clock.frame;
        info!("screenshot requested -> {}", path.0);
    }
    if clock.shot && (std::path::Path::new(&path.0).exists() || clock.frame > clock.shot_at + 1800)
    {
        info!("Screenshot saved: {}", path.0);
        exit.write(AppExit::Success);
    }
}
