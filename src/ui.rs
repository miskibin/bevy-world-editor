//! The editor UI: an egui panel layout (top toolbar + right inspector + bottom status bar)
//! wrapping the 3D viewport. Drives the flat/procedural worldgen, the brush tools, save/load,
//! and the post-processing knobs (`GfxSettings`/SSAA). Runs in `EguiPrimaryContextPass`.

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

/// Per-frame snapshot of whether egui is capturing input, mirrored from bevy_egui's
/// `EguiWantsInput`. The fly-cam and the brush read this so keystrokes typed into a panel
/// field (or clicks/drags on a widget) don't also drive the camera or paint the terrain.
#[derive(Resource, Default)]
pub struct UiInputCapture {
    pub pointer: bool,
    pub keyboard: bool,
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GfxSettings>()
            .init_resource::<UiInputCapture>()
            .add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(Update, (apply_ssaa, update_input_capture))
            .add_systems(EguiPrimaryContextPass, panel_ui);
    }
}

/// Mirror bevy_egui's `EguiWantsInput` into the project-owned capture resource once per frame.
fn update_input_capture(
    wants: Res<bevy_egui::input::EguiWantsInput>,
    mut cap: ResMut<UiInputCapture>,
) {
    cap.pointer = wants.wants_any_pointer_input();
    cap.keyboard = wants.wants_any_keyboard_input();
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

#[allow(clippy::too_many_arguments)]
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
    editor: (
        Option<ResMut<crate::editor::EditorState>>,
        Option<Res<crate::editor::UndoStack>>,
        Option<Res<crate::editor::EditLayers>>,
    ),
    mut dof_q: Query<(Entity, &mut crate::dof::Dof), With<Camera3d>>,
    mut ter_mats: ResMut<Assets<crate::terrain_mat::TerrainMaterial>>,
    ground: Res<crate::terrain_mat::GroundMaterial>,
    mut shadow_map: ResMut<DirectionalLightShadowMap>,
    mut low_applied: Local<bool>,
    mut cam: Query<(Entity, &mut DistanceFog, &mut Bloom, &mut Exposure), With<Camera3d>>,
    mut commands: Commands,
) -> Result {
    let (mut clock, mut rays, mut weather) = mood;
    let (mut ed_state, ed_undo, ed_layers) = editor;
    let ctx = contexts.ctx_mut()?;
    // egui 0.35 shows panels on a Ui, not the Context — build a screen-covering root Ui in
    // the background layer, then dock panels into it (bevy_egui's documented pattern). No
    // CentralPanel is added, so the leftover middle stays transparent — the 3D view shows.
    let mut viewport_ui = egui::Ui::new(
        ctx.clone(),
        "viewport".into(),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()),
    );

    // Regen is deferred to after every panel closure releases its `&mut params` borrow.
    let mut gen_procedural = false;
    let mut new_flat = false;

    // ---------------------------------------------------------------- top toolbar
    egui::Panel::top("toolbar").show(&mut viewport_ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let name = ed_state
                .as_deref()
                .map(|e| e.file_path.clone())
                .unwrap_or_else(|| "untitled".into());
            ui.strong(format!("World Editor — {name}"));
            ui.separator();

            if ui.button("New flat map").clicked() {
                new_flat = true;
            }
            if let Some(ed) = ed_state.as_deref_mut() {
                if ui.button("Open").clicked() {
                    ed.load_clicked = true;
                }
                if ui.button("Save").clicked() {
                    ed.save_clicked = true;
                }
                ui.label("as");
                ui.add(egui::TextEdit::singleline(&mut ed.file_path).desired_width(140.0));
                ui.separator();
                if ui.button("Apply").clicked() {
                    ed.apply_clicked = true;
                }
                let (u, r) = ed_undo.as_deref().map(|s| s.depth()).unwrap_or((0, 0));
                if ui
                    .add_enabled(u > 0, egui::Button::new("Undo"))
                    .on_hover_text(format!("{u} step(s) — Ctrl+Z"))
                    .clicked()
                {
                    ed.undo_clicked = true;
                }
                if ui
                    .add_enabled(r > 0, egui::Button::new("Redo"))
                    .on_hover_text(format!("{r} step(s) — Ctrl+Y"))
                    .clicked()
                {
                    ed.redo_clicked = true;
                }
                ui.separator();

                // Tool palette — the current tool stays highlighted (selectable_value).
                use crate::editor::{MaskCh, Tool};
                for (t, label) in [
                    (Tool::Off, "off"),
                    (Tool::Raise, "raise"),
                    (Tool::Lower, "lower"),
                    (Tool::Smooth, "smooth"),
                    (Tool::Flatten, "flatten"),
                ] {
                    ui.selectable_value(&mut ed.tool, t, label);
                }
                ui.separator();
                for ch in MaskCh::ALL {
                    ui.selectable_value(&mut ed.tool, Tool::Paint(ch), ch.label());
                }
            }
        });
        // Loud missing-textures banner: without the CC0 sets the terrain silently falls back
        // to flat green, which reads as "the game looks broken" (it did).
        if matches!(&*ground, crate::terrain_mat::GroundMaterial::Fallback(_)) {
            ui.colored_label(
                egui::Color32::from_rgb(240, 120, 90),
                "⚠ ground textures missing — flat-colour fallback! Run: tools/fetch_textures.ps1",
            );
        }
    });

    // ---------------------------------------------------------------- bottom status bar
    egui::Panel::bottom("status").show(&mut viewport_ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if let Some(ed) = ed_state.as_deref() {
                match ed.cursor_hit {
                    Some(h) => ui.label(format!("cursor {:.0},{:.0}  h={:.1} m", h.x, h.z, h.y)),
                    None => ui.label("cursor —"),
                };
                ui.separator();
                ui.label(format!("{}  r={:.0}", tool_name(ed.tool), ed.radius));
            }
            ui.separator();
            let (u, r) = ed_undo.as_deref().map(|s| s.depth()).unwrap_or((0, 0));
            ui.label(format!("undo {u}/{r}"));
            ui.separator();
            let dirty = ed_layers.as_deref().map(|l| l.dirty_since_apply).unwrap_or(false);
            if dirty {
                ui.colored_label(egui::Color32::from_rgb(240, 200, 90), "● unsaved edits");
            } else {
                ui.label("○ clean");
            }
            ui.separator();
            let fps = diagnostics
                .get(&FrameTimeDiagnosticsPlugin::FPS)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0);
            ui.label(format!("{fps:.0} fps"));

            if regen.running() {
                ui.separator();
                ui.add(egui::ProgressBar::new(regen.fraction()).desired_width(140.0).text(regen.stage()));
            }
            if let Some(ed) = ed_state.as_deref() {
                if !ed.status.is_empty() {
                    ui.separator();
                    ui.label(ed.status.clone());
                }
            }
        });
    });

    // ---------------------------------------------------------------- right inspector
    egui::Panel::right("inspector").resizable(true).default_size(300.0).show(&mut viewport_ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::CollapsingHeader::new("Brush").default_open(true).show(ui, |ui| {
                if let Some(ed) = ed_state.as_deref_mut() {
                    ui.add(egui::Slider::new(&mut ed.radius, 2.0..=80.0).text("radius"));
                    ui.add(egui::Slider::new(&mut ed.strength, 0.1..=4.0).text("strength"));
                    ui.small("LMB paint · RMB inverse · Ctrl+Z/Y undo");
                }
            });

            egui::CollapsingHeader::new("Map").default_open(true).show(ui, |ui| {
                let p = &mut params.0;
                let mut sz = p.terrain.size as u32;
                if ui
                    .add(egui::Slider::new(&mut sz, 256..=4096).step_by(64.0).text("map size (m)"))
                    .changed()
                {
                    // Keep it chunk-aligned (64-cell terrain chunks).
                    p.terrain.size = (sz as usize / 64 * 64).max(256);
                }
                if ui.button("New / Resize flat map").clicked() {
                    new_flat = true;
                }
                ui.small("resize starts a fresh flat map — clears all edits");
                ui.add(egui::Slider::new(&mut p.forest.water_level, 0.0..=16.0).text("water level"));
            });

            egui::CollapsingHeader::new("Procedural").default_open(false).show(ui, |ui| {
                let p = &mut params.0;
                ui.horizontal(|ui| {
                    ui.label("seed");
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
                ui.add(egui::Slider::new(&mut p.terrain.mountainousness, 0.0..=1.0).text("mountains"));
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
                ui.add(egui::Slider::new(&mut p.forest.density, 0.0..=1.0).text("forest density"));
                for (i, name) in ["pine", "spruce", "broadleaf", "birch"].iter().enumerate() {
                    ui.add(egui::Slider::new(&mut p.forest.species_weights[i], 0.0..=2.0).text(*name));
                }
                ui.add(egui::Slider::new(&mut p.forest.treeline, 100.0..=300.0).text("treeline"));
                if ui
                    .add_sized([ui.available_width(), 26.0], egui::Button::new("⟳ Generate procedural"))
                    .clicked()
                {
                    gen_procedural = true;
                }
            });

            egui::CollapsingHeader::new("Environment").default_open(false).show(ui, |ui| {
                let mut hours = clock.t * 24.0;
                if ui.add(egui::Slider::new(&mut hours, 0.0..=24.0).text("time of day")).changed() {
                    clock.t = (hours / 24.0).rem_euclid(1.0);
                }
                let mut mins = clock.day_secs / 60.0;
                if ui
                    .add(egui::Slider::new(&mut mins, 0.0..=60.0).text("day length (min, 0 = paused)"))
                    .changed()
                {
                    clock.day_secs = mins * 60.0;
                }
                ui.separator();
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
                let mut low_changed = ui.checkbox(&mut gfx.low, "LOW graphics (weak GPU)").changed();
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
                changed |= ui.add(egui::Slider::new(&mut gfx.bloom, 0.0..=0.5).text("bloom")).changed();
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

            ui.separator();
            ui.small("RMB drag — look · WASD+QE — move · scroll — speed · Shift — boost");
        });
    });

    // Fire deferred regen now that all `&mut params` borrows are released.
    if new_flat {
        params.0.terrain.flat = true;
        // Blank canvas: scatter off until the user paints density or generates procedurally.
        params.0.forest.density = 0.0;
        if let Some(ed) = ed_state.as_deref_mut() {
            ed.status = format!("new flat map — {} m — edits cleared", params.0.terrain.size);
        }
        let p = params.clone();
        regen.fire(&p);
    } else if gen_procedural {
        params.0.terrain.flat = false;
        // Coming from the blank-canvas default (density 0), seed a real forest so
        // "Generate procedural" produces the classic wooded map, not bare terrain.
        if params.0.forest.density <= 0.0 {
            params.0.forest.density = worldgen::ForestParams::default().density;
        }
        let p = params.clone();
        regen.fire(&p);
    }
    Ok(())
}

fn tool_name(tool: crate::editor::Tool) -> String {
    use crate::editor::Tool;
    match tool {
        Tool::Off => "off".into(),
        Tool::Raise => "raise".into(),
        Tool::Lower => "lower".into(),
        Tool::Smooth => "smooth".into(),
        Tool::Flatten => "flatten".into(),
        Tool::Paint(ch) => ch.label().to_string(),
    }
}
