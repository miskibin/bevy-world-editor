//! Parameter panel (egui): seed, terrain, erosion, forest sliders + Regenerate.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};

use crate::genrun::{GenParams, GenProgress, Regen};

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(EguiPrimaryContextPass, panel_ui);
    }
}

fn panel_ui(
    mut contexts: EguiContexts,
    mut params: ResMut<GenParams>,
    progress: Res<GenProgress>,
    mut regen: Regen,
    diagnostics: Res<DiagnosticsStore>,
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
        if progress.running {
            ui.add(egui::ProgressBar::new(progress.fraction).text(progress.stage.clone()));
        } else if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::new("⟳ Regenerate"))
            .clicked()
        {
            regen.fire();
        }
        ui.separator();
        ui.small("RMB drag — look · WASD+QE — move\nscroll — speed · Shift — boost");
    });
    Ok(())
}
