//! Capture harness (ported from Warbell) — the window can't be captured externally, so we
//! render to disk ourselves. Two modes, both read once at startup:
//!
//! - `WED_SHOT=<path.png>` — single screenshot: warm up (≥240 frames AND ≥10 s wall-clock so
//!   cold pipelines/shaders/IBL settle — here also the world-gen + erosion finish), grab the
//!   window, save, exit. Exit is gated on the FILE EXISTING (the readback is async); stale
//!   files are deleted first so a crashed run can't leave an old image that reads as fresh.
//!   `WED_SHOT_WARMUP=<secs>` raises the wall-clock floor.
//! - `WED_CLIP=<dir>` — frame-sequence capture for GIFs / video: after a warm-up (and once
//!   generation has finished), save every frame as `<dir>/frame_00001.png …` for
//!   `WED_CLIP_FRAMES` frames, then exit. A clamped fixed timestep keeps the world motion
//!   smooth despite the per-frame PNG-encode stall, and an optional slow camera orbit
//!   (`WED_CLIP_ORBIT`) circles a point of interest. ffmpeg then stitches the sequence into a
//!   clip at `WED_CLIP_FPS`.
//!
//! Clip knobs (all optional, env, read at startup):
//! | `WED_CLIP_FRAMES` | saved frames (default 150) |
//! | `WED_CLIP_FPS`    | playback fps → fixed timestep + ffmpeg rate (default 30) |
//! | `WED_CLIP_WARMUP` | warm-up frames before the first save, after gen finishes (default 30) |
//! | `WED_CLIP_ORBIT`  | `"cx,cy,cz,radius,height,deg_per_sec"` slow camera orbit around a point |
//!
//! NB: genrun auto-places the fly-cam AFTER generation unless `WED_CAM` is set. For an orbit
//! clip ALSO set `WED_CAM` to any pose — that disables the auto-placement, and the orbit drives
//! the camera every frame anyway.

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use std::time::Duration;

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
        } else if let Ok(dir) = std::env::var("WED_CLIP") {
            app.insert_resource(clip_cfg(dir))
                .init_resource::<ClipClock>()
                .add_systems(Startup, clip_setup)
                .add_systems(Update, (clip_orbit, drive_clip).chain());
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

// ── clip mode ──────────────────────────────────────────────────────────────────────

#[derive(Resource)]
struct ClipCfg {
    dir: String,
    frames: u32,
    warmup: u32,
    fps: u32,
    orbit: Option<Orbit>,
}

#[derive(Clone, Copy)]
struct Orbit {
    center: Vec3,
    radius: f32,
    height: f32,
    /// degrees per second
    speed: f32,
}

#[derive(Resource, Default)]
struct ClipClock {
    /// total ticks elapsed (warm-up included), only advanced once generation has finished
    frame: u32,
    /// frames written to disk
    saved: u32,
    /// tick the last frame was written — start of the flush tail
    done_at: Option<u32>,
}

fn clip_cfg(dir: String) -> ClipCfg {
    let num = |k: &str, d: f32| {
        std::env::var(k).ok().and_then(|s| s.trim().parse::<f32>().ok()).unwrap_or(d)
    };
    ClipCfg {
        dir,
        frames: num("WED_CLIP_FRAMES", 150.0).max(1.0) as u32,
        warmup: num("WED_CLIP_WARMUP", 30.0).max(0.0) as u32,
        fps: num("WED_CLIP_FPS", 30.0).max(1.0) as u32,
        orbit: std::env::var("WED_CLIP_ORBIT").ok().and_then(parse_orbit),
    }
}

fn parse_orbit(s: String) -> Option<Orbit> {
    let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    (v.len() == 6).then(|| Orbit {
        center: Vec3::new(v[0], v[1], v[2]),
        radius: v[3],
        height: v[4],
        speed: v[5],
    })
}

fn clip_setup(cfg: Res<ClipCfg>, mut vtime: ResMut<Time<Virtual>>) {
    let _ = std::fs::create_dir_all(&cfg.dir);
    // Clamp the per-tick delta to exactly one playback frame. Encoding a PNG every frame makes a
    // tick take far longer than 1/fps of wall-clock, so without this the world would fast-forward
    // in big jumps between saved frames. Clamped, every tick advances the sim by ≤1/fps, so the
    // recorded motion plays back as smooth real-time when ffmpeg assembles at WED_CLIP_FPS.
    vtime.set_max_delta(Duration::from_secs_f32(1.0 / cfg.fps as f32));
}

/// Optional cinematic move: circle `center` at a fixed radius/height, `speed` deg/s. Driven off
/// the saved-frame clock (not wall time) so the path is deterministic. Writes the fly-cam's
/// yaw/pitch AND translation — the fly-cam's `fly` system rewrites the camera rotation from those
/// every frame, so setting only the Transform would be immediately clobbered.
fn clip_orbit(
    cfg: Res<ClipCfg>,
    clock: Res<ClipClock>,
    mut cam: Query<(&mut Transform, &mut crate::flycam::FlyCam)>,
) {
    let Some(o) = cfg.orbit else { return };
    let t = clock.frame.saturating_sub(cfg.warmup) as f32 / cfg.fps as f32;
    let ang = (o.speed * t).to_radians();
    let pos = Vec3::new(o.center.x + o.radius * ang.cos(), o.height, o.center.z + o.radius * ang.sin());
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(pos).looking_at(o.center, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }
}

fn drive_clip(
    cfg: Res<ClipCfg>,
    mut clock: ResMut<ClipClock>,
    genp: Res<crate::genrun::GenProgress>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    // Hold the clock at 0 until the async world-gen finishes, so the warm-up (and every saved
    // frame) renders a fully-spawned world — never a half-generated one.
    if genp.running {
        return;
    }
    // Flush tail: all frames written → wait a few ticks for the async disk writes to land, exit.
    if let Some(done) = clock.done_at {
        if clock.frame >= done + 15 {
            info!("Screenshot saved: {} frames -> {}", clock.saved, cfg.dir);
            exit.write(AppExit::Success);
        }
        clock.frame += 1;
        return;
    }

    if clock.frame >= cfg.warmup && clock.saved < cfg.frames {
        clock.saved += 1;
        let path = format!("{}/frame_{:05}.png", cfg.dir, clock.saved);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        if clock.saved >= cfg.frames {
            clock.done_at = Some(clock.frame);
        }
    }
    clock.frame += 1;
}
