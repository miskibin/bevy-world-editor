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

/// Which biome's textures the material currently holds. Regenerating into a different
/// biome has to reload the arrays — the handles are baked into one shared material.
#[derive(Resource)]
struct LoadedBiome(worldgen::Biome);

impl Plugin for TerrainMatPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default())
            .add_systems(PreStartup, setup_material)
            .add_systems(Update, resync_biome_textures);
    }
}

fn setup_material(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<TerrainMaterial>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    params: Res<crate::genrun::GenParams>,
) {
    let biome = params.0.biome;
    commands.insert_resource(LoadedBiome(biome));
    match texload::load_ground_arrays(biome) {
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
                        // z = HQ flag (quality preset), w = arid flag (biome branch).
                        params2: Vec4::new(1.6, 1.0, 0.0, arid_flag(biome)),
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

fn arid_flag(b: worldgen::Biome) -> f32 {
    if b == worldgen::Biome::Arid { 1.0 } else { 0.0 }
}

/// Swap the ground texture arrays when a regenerate lands in a different biome.
///
/// Driven off the finished world rather than the params so the textures change on the same
/// frame the terrain meshes do — flipping on the param edit instead would repaint the OLD
/// terrain with the NEW biome's textures for the whole generation, which is seconds of a
/// sand-coloured forest.
fn resync_biome_textures(
    world: Option<Res<crate::genrun::GeneratedWorld>>,
    mat: Option<Res<GroundMaterial>>,
    mut loaded: Option<ResMut<LoadedBiome>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<TerrainMaterial>>,
) {
    let (Some(world), Some(GroundMaterial::Splat(handle)), Some(loaded)) =
        (world, mat.as_deref(), loaded.as_mut())
    else {
        return;
    };
    if !world.is_changed() || world.0.biome == loaded.0 {
        return;
    }
    let biome = world.0.biome;
    let Some(arrays) = texload::load_ground_arrays(biome) else {
        warn!("no ground textures for biome {:?} — keeping the previous set", biome);
        return;
    };
    let Some(mut m) = mats.get_mut(handle.id()) else { return };
    m.extension.albedo = images.add(arrays.albedo);
    m.extension.normal = images.add(arrays.normal);
    m.extension.rough = images.add(arrays.rough);
    m.extension.params.params2.w = arid_flag(biome);
    loaded.0 = biome;
    info!("ground textures swapped to {:?}", biome);
}
