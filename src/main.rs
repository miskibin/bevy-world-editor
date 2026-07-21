//! Bevy World Editor — phase 1: realistic forest generator + fly-cam.
//!
//! `worldgen` (pure crate) produces heightfield/maps/tree instances; the modules here
//! turn them into a rendered world. Env hooks: `WED_SEED`, `WED_CAM="x,y,z,tx,ty,tz"`,
//! `WED_SHOT=path.png` (+`WED_SHOT_WARMUP`).

mod ambience;
mod atmospherics;
mod capture;
mod daycycle;
mod godrays;
mod dof;
mod flycam;
mod foliage;
mod forest;
mod genrun;
mod grass;
mod lodline;
mod profile;
mod props;
mod rocks;
mod sky;
mod stats;
mod water_mat;
mod terrain_mat;
mod terrain_mesh;
mod texload;
mod trees_mesh;
mod ui;

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "World Editor — Forest Generator".into(),
                // Vsync caps the frame rate, which silently floors every benchmark at the
                // refresh interval — the profile harness must run uncapped.
                present_mode: if std::env::var("WED_PROFILE").is_ok() {
                    bevy::window::PresentMode::AutoNoVsync
                } else {
                    bevy::window::PresentMode::AutoVsync
                },
                ..default()
            }),
            ..default()
        }))
        // Two tuples — the `Plugins` impl caps at 15 elements per tuple.
        .add_plugins((
            sky::SkyPlugin,               // camera + sun + atmosphere + IBL
            flycam::FlyCamPlugin,         // free camera controls
            genrun::GenPlugin,            // async worldgen pipeline + regenerate
            terrain_mat::TerrainMatPlugin, // splat ExtendedMaterial + texture arrays
            water_mat::WaterMatPlugin,    // lake shader material
            terrain_mesh::TerrainMeshPlugin, // chunked terrain + LOD + lake meshes
            rocks::RocksPlugin,           // chunk-merged boulders (terrain material)
            grass::GrassPlugin,           // streamed swaying grass ring
            props::PropsPlugin,           // bushes + logs + stumps
            trees_mesh::TreeAssetsPlugin, // tree meshes, foliage atlas, bark materials
            forest::ForestPlugin,         // near streaming + far merged impostors
        ))
        .add_plugins((
            ui::UiPlugin,                 // egui parameter panel
            capture::CapturePlugin,       // WED_SHOT harness
            lodline::LodLinePlugin,       // WED_LODLINE model-review grid
            ambience::AmbiencePlugin,     // birds/water/wind ambient loops
            atmospherics::AtmosphericsPlugin, // cinematic height-fog post pass (Warbell port)
            dof::DofPlugin,               // far-distance bokeh soften (Warbell port)
            stats::StatsPlugin,           // F2 stats overlay + WED_GPUSTRESS/WED_PERFLOG
            profile::ProfilePlugin,       // WED_PROFILE scripted benchmark
            daycycle::DayCyclePlugin,     // sun/moon clock, stars, mood driving
            godrays::GodRaysPlugin,       // screen-space light shafts from the sun
        ))
        .run();
}
