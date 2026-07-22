//! Parameter panel (egui): seed, terrain, erosion, forest sliders + Regenerate.

use bevy::camera::Exposure;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::light::DirectionalLightShadowMap;
use bevy::pbr::{
    ContactShadows, DistanceFog, FogFalloff, ScreenSpaceAmbientOcclusion,
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
    pub relief: f32,
    pub cavity: f32,
    /// Supersampling factor for the 3D main pass (1.0 = native). >1 is the cheapest real
    /// cure for jagged alpha-masked foliage edges; SMAA alone can't fix cutout edges.
    pub ssaa: f32,
    /// Master weak-GPU switch: fast terrain shader, small shadow map, no SSAO/contact
    /// shadows/haze/DoF. Also settable at boot via `WED_LOWGFX=1`.
    pub low: bool,
}

impl Default for GfxSettings {
    fn default() -> Self {
        GfxSettings {
            fog: true,
            // 1400 m: 3800 was further than the whole map, i.e. no distance fog at all.
            visibility: 1400.0,
            bloom: 0.12,
            ev100: 11.7,
            ssao: true,
            relief: 1.6,
            cavity: 1.0,
            ssaa: 1.35,
            low: std::env::var("WED_LOWGFX").is_ok(),
        }
    }
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GfxSettings>()
            .add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(Update, apply_ssaa)
            .add_systems(EguiPrimaryContextPass, panel_ui);
    }
}

/// Render the 3D main pass at `ssaa`x the window and let the blit downsample it —
/// genuine supersampling, the only thing that actually smooths alpha-cutout foliage
/// edges (SMAA works on geometric edges). Skipped while WED_GPUSTRESS drives the
/// override itself.
fn apply_ssaa(
    gfx: Res<GfxSettings>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cam: Query<Entity, With<Camera3d>>,
    mut commands: Commands,
    mut last: Local<(f32, UVec2)>,
) {
    if std::env::var("WED_GPUSTRESS").is_ok() {
        return;
    }
    let (Ok(window), Ok(cam)) = (window.single(), cam.single()) else { return };
    let win = window.physical_size();
    if (gfx.ssaa - last.0).abs() < 0.001 && win == last.1 {
        return;
    }
    *last = (gfx.ssaa, win);
    if gfx.ssaa <= 1.001 {
        commands.entity(cam).remove::<bevy::camera::MainPassResolutionOverride>();
    } else {
        let size = (win.as_vec2() * gfx.ssaa).as_uvec2().max(UVec2::ONE);
        commands.entity(cam).insert(bevy::camera::MainPassResolutionOverride(size));
    }
}

fn panel_ui(
    mut contexts: EguiContexts,
    mut params: ResMut<GenParams>,
    mut regen: Regen,
    diagnostics: Res<DiagnosticsStore>,
    mut gfx: ResMut<GfxSettings>,
    mut audio: ResMut<crate::ambience::AudioSettings>,
    mut atmo: ResMut<crate::atmospherics::AtmoSettings>,
    // Bundled: egui panel systems bump into the 16-param SystemParam cap.
    mood: (
        ResMut<crate::daycycle::DayClock>,
        ResMut<crate::godrays::GodRaySettings>,
        ResMut<crate::weather::Weather>,
    ),
    editor: (Option<ResMut<crate::editor::EditorState>>, Option<Res<crate::editor::UndoStack>>),
    mut dof_q: Query<(Entity, &mut crate::dof::Dof), With<Camera3d>>,
    mut ter_mats: ResMut<Assets<crate::terrain_mat::TerrainMaterial>>,
    ground: Res<crate::terrain_mat::GroundMaterial>,
    mut shadow_map: ResMut<DirectionalLightShadowMap>,
    mut low_applied: Local<bool>,
    mut cam: Query<(Entity, &mut DistanceFog, &mut Bloom, &mut Exposure), With<Camera3d>>,
    mut commands: Commands,
) -> Result {
    let (mut clock, mut rays, mut weather) = mood;
    let (ed_state, ed_undo) = editor;
    let ctx = contexts.ctx_mut()?;
    egui::Window::new("Forest Generator").default_width(250.0).show(ctx, |ui| {
        // Loud missing-textures banner: without the CC0 sets the terrain silently falls
        // back to flat green, which reads as "the game looks broken" (it did).
        if matches!(&*ground, crate::terrain_mat::GroundMaterial::Fallback(_)) {
            ui.colored_label(
                egui::Color32::from_rgb(240, 120, 90),
                "⚠ ground textures missing — flat-colour fallback!\nRun: tools/fetch_textures.ps1  (or pwsh -File …)\nThey are gitignored; a fresh clone needs this once.",
            );
            ui.separator();
        }
        let fps = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|d| d.smoothed())
            .unwrap_or(0.0);
        ui.label(format!("{fps:.0} fps"));
        ui.separator();

        // Seed stays outside the scroll area — it's the panel's anchor control.
        {
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
        }
        ui.separator();

        // Everything between Seed and Regenerate scrolls, so the panel never runs off the
        // bottom of the window (usable at 1280×720). `auto_shrink([false, true])` keeps the
        // panel a fixed width but lets it grow to fit its content; `max_height` caps it to the
        // window (reserving room for the pinned top + Regenerate footer) so it scrolls instead
        // of pushing the footer off-screen when every section is open.
        let screen_h =
            ui.ctx().input(|i| i.raw.screen_rect.map(|r| r.height()).unwrap_or(720.0));
        let scroll_max = (screen_h - 220.0).max(120.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .max_height(scroll_max)
            .show(ui, |ui| {
            egui::CollapsingHeader::new("Terrain").default_open(true).show(ui, |ui| {
                let p = &mut params.0;
                let mut sz = p.terrain.size as u32;
                if ui
                    .add(egui::Slider::new(&mut sz, 256..=4096).step_by(64.0).text("map size (m)"))
                    .changed()
                {
                    // Keep it chunk-aligned (64-cell terrain chunks).
                    p.terrain.size = (sz as usize / 64 * 64).max(256);
                }
                ui.add(
                    egui::Slider::new(&mut p.terrain.mountainousness, 0.0..=1.0).text("mountains"),
                );
                ui.add(
                    egui::Slider::new(&mut p.terrain.mountain_height, 60.0..=320.0)
                        .text("peak height"),
                );
                ui.add(egui::Slider::new(&mut p.terrain.warp, 0.0..=1.2).text("warp"));
                let mut droplets = p.erosion.droplets as f32 / 1000.0;
                if ui
                    .add(egui::Slider::new(&mut droplets, 0.0..=600.0).text("erosion (k droplets)"))
                    .changed()
                {
                    p.erosion.droplets = (droplets * 1000.0) as u32;
                }
            });

            egui::CollapsingHeader::new("Forest").default_open(true).show(ui, |ui| {
                let p = &mut params.0;
                ui.add(egui::Slider::new(&mut p.forest.density, 0.0..=1.0).text("density"));
                for (i, name) in ["pine", "spruce", "broadleaf", "birch"].iter().enumerate() {
                    ui.add(
                        egui::Slider::new(&mut p.forest.species_weights[i], 0.0..=2.0).text(*name),
                    );
                }
                ui.add(egui::Slider::new(&mut p.forest.treeline, 100.0..=300.0).text("treeline"));
                ui.add(egui::Slider::new(&mut p.forest.water_level, 0.0..=16.0).text("water level"));
            });

            egui::CollapsingHeader::new("Time").default_open(false).show(ui, |ui| {
                let mut hours = clock.t * 24.0;
                if ui
                    .add(egui::Slider::new(&mut hours, 0.0..=24.0).text("time of day"))
                    .changed()
                {
                    clock.t = (hours / 24.0).rem_euclid(1.0);
                }
                let mut mins = clock.day_secs / 60.0;
                if ui
                    .add(
                        egui::Slider::new(&mut mins, 0.0..=60.0)
                            .text("day length (min, 0 = paused)"),
                    )
                    .changed()
                {
                    clock.day_secs = mins * 60.0;
                }
            });

            // Edit is open by default — it's the tool users live in now.
            egui::CollapsingHeader::new("Edit").default_open(true).show(ui, |ui| {
                if let (Some(mut ed), Some(undo)) = (ed_state, ed_undo) {
                    use crate::editor::{MaskCh, Tool};
                    ui.horizontal_wrapped(|ui| {
                        for (t, label) in [
                            (Tool::Off, "off"),
                            (Tool::Raise, "raise"),
                            (Tool::Lower, "lower"),
                            (Tool::Smooth, "smooth"),
                            (Tool::Flatten, "flatten"),
                        ] {
                            ui.selectable_value(&mut ed.tool, t, label);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for ch in MaskCh::ALL {
                            ui.selectable_value(&mut ed.tool, Tool::Paint(ch), ch.label());
                        }
                    });
                    let mut radius = ed.radius;
                    if ui.add(egui::Slider::new(&mut radius, 2.0..=80.0).text("brush radius")).changed()
                    {
                        ed.radius = radius;
                    }
                    let mut strength = ed.strength;
                    if ui.add(egui::Slider::new(&mut strength, 0.1..=4.0).text("strength")).changed() {
                        ed.strength = strength;
                    }
                    let (u, r) = undo.depth();
                    ui.label(format!("undo {u} / redo {r}  (Ctrl+Z / Ctrl+Y, RMB = inverse)"));
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            ed.apply_clicked = true;
                        }
                        if ui.button("Save").clicked() {
                            ed.save_clicked = true;
                        }
                        if ui.button("Load").clicked() {
                            ed.load_clicked = true;
                        }
                    });
                    let mut path = ed.file_path.clone();
                    if ui.text_edit_singleline(&mut path).changed() {
                        ed.file_path = path;
                    }
                    if !ed.status.is_empty() {
                        ui.label(ed.status.clone());
                    }
                }
            });

            egui::CollapsingHeader::new("Weather").default_open(false).show(ui, |ui| {
                use crate::weather::WeatherMode;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut weather.mode, WeatherMode::Clear, "clear");
                    ui.selectable_value(&mut weather.mode, WeatherMode::Rain, "rain");
                    ui.selectable_value(&mut weather.mode, WeatherMode::Snow, "snow");
                });
                ui.add(egui::Slider::new(&mut weather.intensity, 0.0..=1.0).text("intensity"));
            });

            egui::CollapsingHeader::new("Graphics").default_open(false).show(ui, |ui| {
                let mut changed = false;
                // Weak-GPU master switch. Applied on toggle AND once at boot (WED_LOWGFX).
                let mut low_changed =
                    ui.checkbox(&mut gfx.low, "LOW graphics (weak GPU)").changed();
                if !*low_applied {
                    *low_applied = true;
                    low_changed = true;
                }
                if low_changed {
                    for (_, mat) in ter_mats.iter_mut() {
                        mat.extension.params.params2.z = if gfx.low { 0.0 } else { 1.0 };
                    }
                    shadow_map.size = if gfx.low { 1024 } else { 4096 };
                    gfx.ssao = !gfx.low;
                    atmo.enabled = !gfx.low;
                    if let Ok((_, mut dof)) = dof_q.single_mut() {
                        dof.max_radius = if gfx.low { 0.0 } else { 2.5 };
                    }
                    if let Ok((entity, _, _, _)) = cam.single_mut() {
                        if gfx.low {
                            commands
                                .entity(entity)
                                .remove::<ScreenSpaceAmbientOcclusion>()
                                .remove::<ContactShadows>();
                        } else {
                            commands.entity(entity).insert((
                                ScreenSpaceAmbientOcclusion {
                                    quality_level: ScreenSpaceAmbientOcclusionQualityLevel::Medium,
                                    ..default()
                                },
                                ContactShadows::default(),
                            ));
                        }
                    }
                }
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
                changed |=
                    ui.add(egui::Slider::new(&mut gfx.bloom, 0.0..=0.5).text("bloom")).changed();
                changed |=
                    ui.add(egui::Slider::new(&mut gfx.ev100, 9.5..=13.5).text("exposure")).changed();
                changed |= ui.checkbox(&mut gfx.ssao, "SSAO").changed();
                ui.add(egui::Slider::new(&mut gfx.ssaa, 1.0..=2.0).text("supersampling (edges)"));
                {
                    let mut edited = false;
                    edited |= ui
                        .add(egui::Slider::new(&mut gfx.relief, 0.0..=3.0).text("ground relief"))
                        .changed();
                    edited |= ui
                        .add(egui::Slider::new(&mut gfx.cavity, 0.0..=2.0).text("cavity AO"))
                        .changed();
                    if edited {
                        for (_, mat) in ter_mats.iter_mut() {
                            mat.extension.params.params2.x = gfx.relief;
                            mat.extension.params.params2.y = gfx.cavity;
                        }
                    }
                }
                ui.checkbox(&mut atmo.enabled, "cinematic haze");
                if atmo.enabled {
                    ui.add(egui::Slider::new(&mut atmo.strength, 0.0..=2.0).text("haze strength"));
                    ui.checkbox(&mut rays.enabled, "god rays");
                }
                if let Ok((_, mut dof)) = dof_q.single_mut() {
                    ui.add(egui::Slider::new(&mut dof.max_radius, 0.0..=12.0).text("far blur"));
                }
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
            });

            egui::CollapsingHeader::new("Audio").default_open(false).show(ui, |ui| {
                ui.add(egui::Slider::new(&mut audio.master, 0.0..=1.0).text("master"));
                ui.add(egui::Slider::new(&mut audio.birds, 0.0..=1.0).text("birds"));
                ui.add(egui::Slider::new(&mut audio.water, 0.0..=1.0).text("water"));
                ui.add(egui::Slider::new(&mut audio.wind, 0.0..=1.0).text("wind"));
                ui.add(egui::Slider::new(&mut audio.forest, 0.0..=1.0).text("forest bed"));
            });
        });

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
