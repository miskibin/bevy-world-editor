//! Lake water material (see assets/shaders/water.wgsl). Sun/sky uniforms mirror the
//! fixed morning light in `sky.rs` — if the sun ever becomes dynamic, drive these from
//! the same system.

use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

#[derive(Clone, Copy, ShaderType, Debug)]
pub struct WaterParams {
    pub sun_dir: Vec4,
    /// rgb = glint colour, w = Blinn-Phong exponent.
    pub sun_glint: Vec4,
    pub sky_zenith: Vec4,
    pub sky_horizon: Vec4,
}

#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct WaterExtension {
    #[uniform(100)]
    pub params: WaterParams,
}

impl MaterialExtension for WaterExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/water.wgsl".into()
    }
}

#[derive(Resource)]
pub struct LakeMaterial(pub Handle<WaterMaterial>);

pub struct WaterMatPlugin;

impl Plugin for WaterMatPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default())
            .add_systems(PreStartup, setup);
    }
}

fn setup(mut commands: Commands, mut mats: ResMut<Assets<WaterMaterial>>) {
    let sun = Vec3::new(0.8, 0.7, 0.5).normalize();
    let handle = mats.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.05,
            cull_mode: None,
            ..default()
        },
        extension: WaterExtension {
            params: WaterParams {
                sun_dir: sun.extend(0.0),
                sun_glint: Vec4::new(0.98, 0.95, 0.72, 600.0),
                sky_zenith: Vec4::new(0.12, 0.24, 0.52, 0.0),
                sky_horizon: Vec4::new(0.40, 0.48, 0.56, 0.0),
            },
        },
    });
    commands.insert_resource(LakeMaterial(handle));
}
