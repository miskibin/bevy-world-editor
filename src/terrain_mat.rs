//! Terrain material: `ExtendedMaterial<StandardMaterial, TerrainExtension>` binding the
//! ground texture arrays + splat params (see `assets/shaders/terrain.wgsl`).

use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

use crate::texload;

pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExtension>;

#[derive(Clone, Copy, ShaderType, Debug)]
pub struct TerrainParams {
    /// x = planar UV scale (1/m), y = second-scale factor, z = water level, w = normal strength
    pub params: Vec4,
    /// x = micro-relief strength, y = cavity-AO strength, z/w spare. Live-tunable (panel).
    pub params2: Vec4,
}

#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct TerrainExtension {
    #[uniform(100)]
    pub params: TerrainParams,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub albedo: Handle<Image>,
    #[texture(103, dimension = "2d_array")]
    #[sampler(104)]
    pub normal: Handle<Image>,
    #[texture(105, dimension = "2d_array")]
    #[sampler(106)]
    pub rough: Handle<Image>,
}

impl MaterialExtension for TerrainExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into()
    }
}

/// The one terrain material handle (splatted if textures loaded, flat fallback if not).
#[derive(Resource)]
pub enum GroundMaterial {
    Splat(Handle<TerrainMaterial>),
    Fallback(Handle<StandardMaterial>),
}

pub struct TerrainMatPlugin;

impl Plugin for TerrainMatPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default())
            .add_systems(PreStartup, setup_material);
    }
}

fn setup_material(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<TerrainMaterial>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
) {
    match texload::load_ground_arrays() {
        Some(arrays) => {
            let handle = mats.add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color: Color::WHITE,
                    perceptual_roughness: 0.95,
                    reflectance: 0.20,
                    ..default()
                },
                extension: TerrainExtension {
                    params: TerrainParams {
                        // 1/3m base tile (denser = sharper mid-range), ×0.23 macro second scale, water level filled by
                        // genrun once params are known, normal strength 0.85.
                        params: Vec4::new(1.0 / 3.0, 0.23, crate::genrun::WATER_LEVEL, 0.85),
                        params2: Vec4::new(1.6, 1.0, 0.0, 0.0),
                    },
                    albedo: images.add(arrays.albedo),
                    normal: images.add(arrays.normal),
                    rough: images.add(arrays.rough),
                },
            });
            commands.insert_resource(GroundMaterial::Splat(handle));
        }
        None => {
            warn!("ground textures missing — flat-colour fallback (run tools/fetch_textures.ps1)");
            let handle = std_mats.add(StandardMaterial {
                base_color: Color::srgb(0.35, 0.42, 0.24),
                perceptual_roughness: 0.95,
                ..default()
            });
            commands.insert_resource(GroundMaterial::Fallback(handle));
        }
    }
}
