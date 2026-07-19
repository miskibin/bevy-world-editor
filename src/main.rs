//! Bevy World Editor — phase 1: realistic forest generator + fly-cam.
//!
//! `worldgen` (pure crate) produces heightfield/maps/tree instances; the modules here
//! turn them into a rendered world. Env hooks: `WED_SEED`, `WED_CAM="x,y,z,tx,ty,tz"`,
//! `WED_SHOT=path.png` (+`WED_SHOT_WARMUP`).

mod ambience;
mod atmospherics;
mod capture;
mod dof;
mod flycam;
mod foliage;
mod forest;
mod genrun;
mod lodline;
mod rocks;
mod sky;
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
                ..default()
            }),
            ..default()
        }))
        .add_plugins((
            sky::SkyPlugin,               // camera + sun + atmosphere + IBL
            flycam::FlyCamPlugin,         // free camera controls
            genrun::GenPlugin,            // async worldgen pipeline + regenerate
            terrain_mat::TerrainMatPlugin, // splat ExtendedMaterial + texture arrays
            water_mat::WaterMatPlugin,    // lake shader material
            terrain_mesh::TerrainMeshPlugin, // chunked terrain + LOD + lake meshes
            rocks::RocksPlugin,           // chunk-merged boulders (terrain material)
            trees_mesh::TreeAssetsPlugin, // tree meshes, foliage atlas, bark materials
            forest::ForestPlugin,         // near streaming + far merged impostors
            ui::UiPlugin,                 // egui parameter panel
            capture::CapturePlugin,       // WED_SHOT harness
            lodline::LodLinePlugin,       // WED_LODLINE model-review grid
            ambience::AmbiencePlugin,     // birds/water/wind ambient loops
            atmospherics::AtmosphericsPlugin, // cinematic height-fog post pass (Warbell port)
            dof::DofPlugin,               // far-distance bokeh soften (Warbell port)
        ))
        .run();
}
