//! `WED_PROFILE=1` — scripted benchmark harness. Flies the camera through a fixed set of
//! representative poses (forest interior, canopy fly-over, lake shore, high overview,
//! treeline ridge), holds each for a settle + measure window, and prints a per-pose table
//! of median frame time, fps and the heaviest GPU passes, then a summary and exit.
//!
//! Deterministic: same seed, same poses, same windows → runs are comparable across
//! changes. Pair with `WED_GPUSTRESS=<mult>` to emulate a weaker GPU, `WED_LOWGFX=1` to
//! measure the LOW preset, or `WED_PROFILE_SECS=<n>` to lengthen each measure window.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

use crate::genrun::{GenProgress, GeneratedWorld, world_offset};

/// One benchmarked viewpoint: label + how to derive the pose from the world.
struct Pose {
    label: &'static str,
    /// Fractions of the map extent for eye and target, plus height offsets in metres.
    eye: (f32, f32, f32),
    target: (f32, f32, f32),
}

const POSES: [Pose; 5] = [
    Pose { label: "forest-interior", eye: (0.35, 0.35, 1.7), target: (0.45, 0.45, 1.2) },
    Pose { label: "canopy-flyover", eye: (0.30, 0.55, 35.0), target: (0.60, 0.55, 5.0) },
    Pose { label: "lake-shore", eye: (0.50, 0.50, 3.0), target: (0.62, 0.58, 0.0) },
    Pose { label: "high-overview", eye: (0.50, 0.90, 260.0), target: (0.50, 0.35, 0.0) },
    Pose { label: "ridge-vista", eye: (0.20, 0.20, 60.0), target: (0.80, 0.80, 0.0) },
];

#[derive(Resource)]
struct ProfileRun {
    idx: usize,
    /// Seconds spent in the current pose.
    elapsed: f32,
    samples: Vec<f32>,
    results: Vec<(String, f32, f32, String)>,
    measure_secs: f32,
    settle_secs: f32,
}

pub struct ProfilePlugin;

impl Plugin for ProfilePlugin {
    fn build(&self, app: &mut App) {
        if std::env::var("WED_PROFILE").is_err() {
            return;
        }
        let measure_secs = std::env::var("WED_PROFILE_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(4.0);
        app.insert_resource(ProfileRun {
            idx: 0,
            elapsed: 0.0,
            samples: Vec::new(),
            results: Vec::new(),
            measure_secs,
            settle_secs: 2.5,
        })
        .add_systems(Update, drive_profile);
    }
}

fn drive_profile(
    time: Res<Time<Real>>,
    genp: Res<GenProgress>,
    world: Option<Res<GeneratedWorld>>,
    diags: Res<DiagnosticsStore>,
    mut run: ResMut<ProfileRun>,
    mut cam: Query<(&mut Transform, &mut crate::flycam::FlyCam)>,
    mut exit: MessageWriter<AppExit>,
    mut warm: Local<f32>,
) {
    let Some(world) = world else { return };
    if genp.running {
        return;
    }
    // One-off warm-up so shader/pipeline compilation isn't charged to pose #1.
    *warm += time.delta_secs();
    if *warm < 6.0 {
        return;
    }
    if run.idx >= POSES.len() {
        return;
    }

    let hf = &world.0.height;
    let off = world_offset(hf);
    let ext = hf.extent();
    let pose = &POSES[run.idx];
    let place = |(fx, fz, h): (f32, f32, f32)| {
        let (mx, mz) = (ext * fx, ext * fz);
        Vec3::new(mx + off, hf.sample_world(mx, mz) + h, mz + off)
    };
    let (eye, target) = (place(pose.eye), place(pose.target));
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(eye).looking_at(target, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }

    run.elapsed += time.delta_secs();
    // Settle window: let streaming (grass/tree chunks) catch up before sampling.
    if run.elapsed < run.settle_secs {
        return;
    }
    if let Some(ms) = diags.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME).and_then(|d| d.value()) {
        run.samples.push(ms as f32);
    }
    if run.elapsed < run.settle_secs + run.measure_secs {
        return;
    }

    // Pose finished: median frame time + the heaviest GPU passes.
    let mut s = std::mem::take(&mut run.samples);
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if s.is_empty() { 0.0 } else { s[s.len() / 2] };
    let p95 = if s.is_empty() { 0.0 } else { s[(s.len() * 95 / 100).min(s.len() - 1)] };
    let passes = crate::stats::top_passes(&diags, 5);
    let label = POSES[run.idx].label.to_string();
    info!(
        "PROFILE {label}: {median:.2} ms median ({:.0} fps), p95 {p95:.2} ms | {passes}",
        1000.0 / median.max(0.01)
    );
    run.results.push((label, median, p95, passes));
    run.idx += 1;
    run.elapsed = 0.0;

    if run.idx >= POSES.len() {
        info!("──────── PROFILE SUMMARY ────────");
        for (label, median, p95, passes) in &run.results {
            info!(
                "{label:<16} {median:>6.2} ms  ({:>5.0} fps)  p95 {p95:>6.2} ms  {passes}",
                1000.0 / median.max(0.01)
            );
        }
        let worst = run
            .results
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((label, median, _, _)) = worst {
            info!("worst pose: {label} at {median:.2} ms");
        }
        exit.write(AppExit::Success);
    }
}
