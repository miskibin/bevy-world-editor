//! Parameter panel (egui): seed, terrain, erosion, forest sliders + Regenerate.

use bevy::camera::Exposure;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::pbr::{
    DistanceFog, FogFalloff, ScreenSpaceAmbientOcclusion,
    ScreenSpaceAmbientOcclusionQualityLevel,
};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};

use crate::genrun::{GenParams, Regen};

/// Post-processing knobs surfaced in the panel. Mirrors the camera components set up in
/// `sky.rs`; `apply` in `panel_ui` writes them through every frame the panel changes them.
#[derive(Resource)]
pub struct GfxSettings {
    pub fog: bool,
    /// Fog visibility distance in metres (smaller = thicker haze).
    pub visibility: f32,
    pub bloom: f32,
    pub ev100: f32,
    pub ssao: bool,
}

impl Default for GfxSettings {
    fn default() -> Self {
        GfxSettings { fog: true, visibility: 3800.0, bloom: 0.12, ev100: 11.7, ssao: true }
    }
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GfxSettings>()
            .add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(EguiPrimaryContextPass, panel_ui);
    }
}

fn panel_ui(
    mut contexts: EguiContexts,
    mut params: ResMut<GenParams>,
    mut regen: Regen,
    diagnostics: Res<DiagnosticsStore>,
    mut gfx: ResMut<GfxSettings>,
    mut cam: Query<(Entity, &mut DistanceFog, &mut Bloom, &mut Exposure), With<Camera3d>>,
    mut commands: Commands,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    egui::Window::new("Forest Generator").default_width(250.0).show(ctx, |ui| {
        let fps = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|d| d.smoothed())
            .unwrap_or(0.0);
        ui.label(format!("{fps:.0} fps"));
        ui.separator();

        let p = &mut params.0;
        ui.label("Seed");
        ui.horizontal(|ui| {
            let mut seed = p.terrain.seed as i64;
            if ui.add(egui::DragValue::new(&mut seed)).changed() {
                p.terrain.seed = seed.rem_euclid(u32::MAX as i64) as u32;
                p.forest.seed = p.terrain.seed;
            }
            if ui.button("🎲").clicked() {
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(1);
                p.terrain.seed = t;
                p.forest.seed = t;
            }
        });

        ui.separator();
        ui.label("Terrain");
        ui.add(
            egui::Slider::new(&mut p.terrain.mountainousness, 0.0..=1.0).text("mountains"),
        );
        ui.add(
            egui::Slider::new(&mut p.terrain.mountain_height, 60.0..=320.0).text("peak height"),
        );
        ui.add(egui::Slider::new(&mut p.terrain.warp, 0.0..=1.2).text("warp"));
        let mut droplets = p.erosion.droplets as f32 / 1000.0;
        if ui
            .add(egui::Slider::new(&mut droplets, 0.0..=600.0).text("erosion (k droplets)"))
            .changed()
        {
            p.erosion.droplets = (droplets * 1000.0) as u32;
        }

        ui.separator();
        ui.label("Forest");
        ui.add(egui::Slider::new(&mut p.forest.density, 0.0..=1.0).text("density"));
        for (i, name) in ["pine", "spruce", "broadleaf", "birch"].iter().enumerate() {
            ui.add(egui::Slider::new(&mut p.forest.species_weights[i], 0.0..=2.0).text(*name));
        }
        ui.add(egui::Slider::new(&mut p.forest.treeline, 100.0..=300.0).text("treeline"));

        ui.separator();
        ui.label("Graphics");
        let mut changed = false;
        changed |= ui.checkbox(&mut gfx.fog, "fog").changed();
        if gfx.fog {
            changed |= ui
                .add(
                    egui::Slider::new(&mut gfx.visibility, 400.0..=6000.0)
                        .logarithmic(true)
                        .text("visibility (m)"),
                )
                .changed();
        }
        changed |= ui.add(egui::Slider::new(&mut gfx.bloom, 0.0..=0.5).text("bloom")).changed();
        changed |=
            ui.add(egui::Slider::new(&mut gfx.ev100, 9.5..=13.5).text("exposure")).changed();
        changed |= ui.checkbox(&mut gfx.ssao, "SSAO").changed();
        if changed {
            if let Ok((entity, mut fog, mut bloom, mut exposure)) = cam.single_mut() {
                let vis = if gfx.fog { gfx.visibility } else { 1.0e6 };
                fog.falloff = FogFalloff::from_visibility_colors(
                    vis,
                    Color::srgb(0.42, 0.48, 0.55),
                    Color::srgb(0.68, 0.76, 0.88),
                );
                bloom.intensity = gfx.bloom;
                exposure.ev100 = gfx.ev100;
                if gfx.ssao {
                    commands.entity(entity).insert(ScreenSpaceAmbientOcclusion {
                        quality_level: ScreenSpaceAmbientOcclusionQualityLevel::Medium,
                        ..default()
                    });
                } else {
                    commands.entity(entity).remove::<ScreenSpaceAmbientOcclusion>();
                }
            }
        }

        ui.separator();
        if regen.running() {
            ui.add(egui::ProgressBar::new(regen.fraction()).text(regen.stage()));
        } else if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::new("⟳ Regenerate"))
            .clicked()
        {
            regen.fire(&params);
        }
        ui.separator();
        ui.small("RMB drag — look · WASD+QE — move\nscroll — speed · Shift — boost");
    });
    Ok(())
}
