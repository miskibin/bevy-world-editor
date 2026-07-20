//! **F2 stats overlay** (ported from Warbell's debug_stats.rs) — read-only
//! instrumentation: FPS + frame-time sparkline, entity/asset counts, and the per-render-
//! pass GPU timing table (the bottleneck finder). Plus two perf-lab hooks:
//!
//! - `WED_GPUSTRESS=<mult>` — renders the main pass at `mult`× window resolution via
//!   `MainPassResolutionOverride`, emulating a weak GPU's fill-rate on a fast card
//!   (2.0 ≈ 4× the pixels ≈ a card ~4× slower).
//! - `WED_PERFLOG=1` — logs the sorted GPU-pass table to the console every ~2 s (pair
//!   with `WED_SHOT` runs so a headless capture doubles as a profile).

use bevy::camera::MainPassResolutionOverride;
use bevy::diagnostic::{DiagnosticsStore, EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};

#[derive(Resource, Default)]
struct StatsPanel {
    open: bool,
}

pub struct StatsPlugin;

impl Plugin for StatsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((EntityCountDiagnosticsPlugin::default(), RenderDiagnosticsPlugin))
            .init_resource::<StatsPanel>()
            .add_systems(Update, toggle_panel)
            .add_systems(EguiPrimaryContextPass, stats_ui);
        if let Some(mult) = env_f32("WED_GPUSTRESS") {
            app.insert_resource(GpuStress(mult)).add_systems(Update, apply_gpu_stress);
        }
        if std::env::var("WED_PERFLOG").is_ok() {
            app.add_systems(Update, perf_log);
        }
    }
}

fn env_f32(k: &str) -> Option<f32> {
    std::env::var(k).ok()?.trim().parse().ok()
}

#[derive(Resource)]
struct GpuStress(f32);

/// Emulate a weak GPU: render the 3D main pass at N× the window resolution.
fn apply_gpu_stress(
    stress: Res<GpuStress>,
    window: Query<&Window, With<PrimaryWindow>>,
    cam: Query<Entity, With<Camera3d>>,
    mut commands: Commands,
) {
    let (Ok(window), Ok(cam)) = (window.single(), cam.single()) else { return };
    let size = window.physical_size().as_vec2() * stress.0;
    commands
        .entity(cam)
        .insert(MainPassResolutionOverride(size.as_uvec2().max(UVec2::ONE)));
}

fn toggle_panel(keys: Res<ButtonInput<KeyCode>>, mut panel: ResMut<StatsPanel>) {
    if keys.just_pressed(KeyCode::F2) {
        panel.open = !panel.open;
    }
}

fn perf_log(diags: Res<DiagnosticsStore>, time: Res<Time<Real>>, mut last: Local<f32>) {
    if time.elapsed_secs() - *last < 2.0 {
        return;
    }
    *last = time.elapsed_secs();
    let mut rows = collect_passes(&diags, "/elapsed_gpu");
    if rows.is_empty() {
        rows = collect_passes(&diags, "/elapsed_cpu");
    }
    let total: f64 = rows.iter().map(|r| r.1).sum();
    let top: Vec<String> =
        rows.iter().take(10).map(|(n, ms)| format!("{n}={ms:.2}ms")).collect();
    info!("PERF total={total:.2}ms | {}", top.join(" "));
}

/// "name=1.23ms" for the N heaviest GPU passes — used by the profile harness report.
pub fn top_passes(diags: &DiagnosticsStore, n: usize) -> String {
    let mut rows = collect_passes(diags, "/elapsed_gpu");
    if rows.is_empty() {
        rows = collect_passes(diags, "/elapsed_cpu");
    }
    rows.truncate(n);
    rows.iter().map(|(k, v)| format!("{k}={v:.2}ms")).collect::<Vec<_>>().join(" ")
}

fn collect_passes(diags: &DiagnosticsStore, field: &str) -> Vec<(String, f64)> {
    // A pass that stopped running keeps its last EMA forever — skip stale measurements.
    let stale = std::time::Duration::from_millis(500);
    let mut v: Vec<(String, f64)> = diags
        .iter()
        .filter_map(|d| {
            let p = d.path().as_str();
            let name = p.strip_prefix("render/")?.strip_suffix(field)?;
            if d.measurement().is_none_or(|m| m.time.elapsed() > stale) {
                return None;
            }
            let ms = d.smoothed().filter(|m| *m > 0.0)?;
            Some((name.trim_end_matches('/').replace('/', " › "), ms))
        })
        .collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

fn stats_ui(
    mut contexts: EguiContexts,
    panel: Res<StatsPanel>,
    diags: Res<DiagnosticsStore>,
    stress: Option<Res<GpuStress>>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    if !panel.open {
        return Ok(());
    }

    egui::Window::new("Stats (F2)").default_width(280.0).resizable(false).show(ctx, |ui| {
        let fps = diags
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|d| d.smoothed())
            .unwrap_or(0.0);
        let frame_ms = diags
            .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
            .and_then(|d| d.smoothed())
            .unwrap_or(0.0);
        let fps_col = if fps >= 55.0 {
            egui::Color32::from_rgb(120, 220, 120)
        } else if fps >= 30.0 {
            egui::Color32::from_rgb(230, 200, 100)
        } else {
            egui::Color32::from_rgb(230, 110, 110)
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{fps:.0}")).size(28.0).strong().color(fps_col));
            ui.label(format!("FPS   ({frame_ms:.2} ms/frame)"));
        });
        if let Some(s) = stress.as_ref() {
            ui.colored_label(
                egui::Color32::from_rgb(230, 160, 90),
                format!("GPU-stress ×{:.1} ({}× pixels)", s.0, (s.0 * s.0) as u32),
            );
        }
        if let Some(d) = diags.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME) {
            let history: Vec<f32> = d.values().map(|v| *v as f32).collect();
            frame_graph(ui, &history);
        }

        ui.separator();
        let entities = diags
            .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
            .and_then(|d| d.value())
            .unwrap_or(0.0);
        egui::Grid::new("counts").num_columns(2).striped(true).show(ui, |ui| {
            ui.label("entities");
            ui.label(format!("{entities:.0}"));
            ui.end_row();
            ui.label("meshes");
            ui.label(format!("{}", meshes.len()));
            ui.end_row();
            ui.label("images");
            ui.label(format!("{}", images.len()));
            ui.end_row();
        });

        ui.separator();
        egui::CollapsingHeader::new("GPU passes (ms)").default_open(true).show(ui, |ui| {
            let (rows, cpu_only) = match collect_passes(&diags, "/elapsed_gpu") {
                v if !v.is_empty() => (v, false),
                _ => (collect_passes(&diags, "/elapsed_cpu"), true),
            };
            if rows.is_empty() {
                ui.weak("no render-pass timings yet…");
                return;
            }
            if cpu_only {
                ui.weak("GPU timestamps unavailable — CPU span times");
            }
            let max = rows.iter().map(|r| r.1).fold(0.0_f64, f64::max).max(0.01);
            let total: f64 = rows.iter().map(|r| r.1).sum();
            for (name, ms) in rows.iter().take(14) {
                let bar = egui::ProgressBar::new((ms / max) as f32)
                    .desired_width(ui.available_width())
                    .text(format!("{name}  {ms:.2}"));
                ui.add(bar);
            }
            ui.weak(format!("Σ listed passes: {total:.2} ms"));
        });
    });
    Ok(())
}

/// Frame-time sparkline with 60/30 fps reference lines (Warbell port).
fn frame_graph(ui: &mut egui::Ui, history: &[f32]) {
    let (rect, painter) =
        ui.allocate_painter(egui::vec2(ui.available_width(), 48.0), egui::Sense::hover());
    let rect = rect.rect;
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(24, 26, 30));
    if history.len() < 2 {
        return;
    }
    let peak = history.iter().copied().fold(0.0_f32, f32::max).max(33.3);
    let y_at = |ms: f32| rect.bottom() - (ms / peak).clamp(0.0, 1.0) * rect.height();
    for (ms, col) in [
        (1000.0 / 60.0, egui::Color32::from_rgb(70, 120, 70)),
        (1000.0 / 30.0, egui::Color32::from_rgb(130, 70, 70)),
    ] {
        let y = y_at(ms);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, col),
        );
    }
    let n = history.len();
    let pts: Vec<egui::Pos2> = history
        .iter()
        .enumerate()
        .map(|(i, &ms)| {
            let x = rect.left() + (i as f32 / (n - 1) as f32) * rect.width();
            egui::pos2(x, y_at(ms))
        })
        .collect();
    painter.line(pts, egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 200, 240)));
}
