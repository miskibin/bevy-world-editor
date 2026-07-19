//! Camera + lighting: single HDR camera (never add a second Camera3d — Warbell rule),
//! procedural `Atmosphere` sky driven by a fixed morning sun, cascade shadows, SSAO,
//! SMAA, restrained bloom, distance fog for aerial perspective, and the gradient-cubemap
//! IBL with the freeze trick (drop `GeneratedEnvironmentMapLight` once filtered — the
//! source cubemap is static, refiltering it every frame is pure waste).

use bevy::anti_alias::smaa::{Smaa, SmaaPreset};
use bevy::asset::RenderAssetUsages;
use bevy::camera::{Exposure, Hdr};
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::atmosphere::ScatteringMedium;
use bevy::light::{
    Atmosphere, CascadeShadowConfigBuilder, DirectionalLightShadowMap, ShadowFilteringMethod,
    SunDisk,
};
use bevy::pbr::{
    AtmosphereSettings, ContactShadows, DistanceFog, FogFalloff, ScreenSpaceAmbientOcclusion,
    ScreenSpaceAmbientOcclusionQualityLevel,
};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use crate::flycam::FlyCam;

const IBL_INTENSITY: f32 = 1500.0;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.62, 0.75, 0.92)))
            .insert_resource(GlobalAmbientLight {
                color: Color::srgb(0.85, 0.92, 1.0),
                // Forest-interior fill: under a dense canopy almost everything is lit by
                // sky bounce — 140 read as pitch-black boughs.
                brightness: 340.0,
                affects_lightmapped_meshes: true,
            })
            .insert_resource(DirectionalLightShadowMap { size: 4096 })
            .add_systems(Startup, (setup_camera, setup_sun))
            .add_systems(Update, freeze_ibl_filtering);
    }
}

/// `WED_CAM="x,y,z,tx,ty,tz"` override for staged screenshots.
fn env_cam() -> Option<Transform> {
    let s = std::env::var("WED_CAM").ok()?;
    let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    (v.len() == 6).then(|| {
        Transform::from_xyz(v[0], v[1], v[2]).looking_at(Vec3::new(v[3], v[4], v[5]), Vec3::Y)
    })
}

fn setup_camera(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut media: ResMut<Assets<ScatteringMedium>>,
) {
    let env = images.add(gradient_env_cubemap());
    let medium = media.add(ScatteringMedium::default());

    let cam_tf = env_cam().unwrap_or_else(|| {
        Transform::from_xyz(0.0, 120.0, 350.0).looking_at(Vec3::new(0.0, 60.0, 0.0), Vec3::Y)
    });
    let (yaw, pitch, _) = cam_tf.rotation.to_euler(EulerRot::YXZ);

    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: 55f32.to_radians(),
            near: 0.1,
            far: 6000.0,
            ..default()
        }),
        cam_tf,
        Hdr,
        Exposure { ev100: 11.7 },
        Tonemapping::TonyMcMapface, // neutral filmic — realism, not a stylized grade
        Msaa::Off,
        Smaa { preset: SmaaPreset::High },
        ScreenSpaceAmbientOcclusion {
            quality_level: ScreenSpaceAmbientOcclusionQualityLevel::Medium,
            ..default()
        },
        (DepthPrepass, NormalPrepass, ContactShadows::default()),
        Bloom { intensity: 0.12, ..Bloom::NATURAL },
        // Gentle atmospheric fog — aerial perspective over the 2 km map, not a wall.
        DistanceFog {
            color: Color::srgba(0.75, 0.82, 0.92, 1.0),
            directional_light_color: Color::srgba(1.0, 0.95, 0.85, 0.6),
            directional_light_exponent: 30.0,
            falloff: FogFalloff::from_visibility_colors(
                3800.0,
                Color::srgb(0.42, 0.48, 0.55),
                Color::srgb(0.68, 0.76, 0.88),
            ),
        },
        (
            GeneratedEnvironmentMapLight {
                environment_map: env,
                intensity: IBL_INTENSITY,
                ..default()
            },
            AtmosphereSettings::default(),
            ShadowFilteringMethod::Gaussian,
            FlyCam::new(yaw, pitch),
        ),
    ));

    // The procedural sky is its own entity (0.19); the camera opts in via AtmosphereSettings.
    commands.spawn(Atmosphere::earth(medium));
}

fn setup_sun(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(1.0, 0.96, 0.88),
            illuminance: 32_000.0, // bright late-morning sun
            shadow_maps_enabled: true,
            contact_shadows_enabled: true,
            shadow_depth_bias: 0.05,
            shadow_normal_bias: 2.2,
            ..default()
        },
        SunDisk::EARTH, // physically-sized disk — realism, not the stylized ball
        CascadeShadowConfigBuilder {
            num_cascades: 4,
            maximum_distance: 700.0,
            first_cascade_far_bound: 40.0,
            ..default()
        }
        .build(),
        // Fixed pleasant morning angle (~35° up, from the south-east).
        Transform::from_xyz(0.8, 0.7, 0.5).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// Drop `GeneratedEnvironmentMapLight` once the filtered IBL exists — the static gradient
/// cubemap never changes, so per-frame refiltering (~2 ms) buys nothing.
fn freeze_ibl_filtering(
    mut commands: Commands,
    q: Query<Entity, (With<GeneratedEnvironmentMapLight>, With<EnvironmentMapLight>)>,
    mut settle: Local<u32>,
) {
    if q.is_empty() {
        return;
    }
    *settle += 1;
    if *settle < 12 {
        return;
    }
    for e in &q {
        commands.entity(e).remove::<GeneratedEnvironmentMapLight>();
    }
}

/// 3-stop gradient cubemap (sky / horizon / ground) — cheap, stable ambient IBL.
fn gradient_env_cubemap() -> Image {
    const FACE: u32 = 64;
    let sky = Color::srgb_u8(0xcf, 0xdd, 0xf2).to_linear();
    let ground = Color::srgb_u8(0x49, 0x52, 0x39).to_linear();
    let horizon = Color::srgb_u8(0xb9, 0xc2, 0xc4).to_linear();

    let mut data: Vec<u8> = Vec::with_capacity((FACE * FACE * 6 * 8) as usize);
    for face in 0..6u32 {
        for y in 0..FACE {
            for x in 0..FACE {
                let u = (x as f32 + 0.5) / FACE as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / FACE as f32 * 2.0 - 1.0;
                let dir = match face {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                }
                .normalize();
                let h = dir.y;
                let lin = if h >= 0.0 {
                    let s = h.clamp(0.0, 1.0);
                    mix_linear(horizon, sky, s * s * (3.0 - 2.0 * s))
                } else {
                    let s = (-h).clamp(0.0, 1.0);
                    mix_linear(horizon, ground, s * s * (3.0 - 2.0 * s))
                };
                for c in [lin.red, lin.green, lin.blue, 1.0] {
                    data.extend_from_slice(&half::f16_bits(c));
                }
            }
        }
    }

    let mut image = Image::new(
        Extent3d { width: FACE, height: FACE, depth_or_array_layers: 6 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor =
        Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    image
}

fn mix_linear(a: LinearRgba, b: LinearRgba, s: f32) -> LinearRgba {
    let s = s.clamp(0.0, 1.0);
    LinearRgba {
        red: a.red + (b.red - a.red) * s,
        green: a.green + (b.green - a.green) * s,
        blue: a.blue + (b.blue - a.blue) * s,
        alpha: 1.0,
    }
}

/// Minimal f32→f16 (little-endian bytes) — enough for well-behaved colour values.
mod half {
    pub fn f16_bits(v: f32) -> [u8; 2] {
        let bits = v.to_bits();
        let sign = ((bits >> 16) & 0x8000) as u16;
        let exp = ((bits >> 23) & 0xff) as i32;
        let frac = bits & 0x007f_ffff;
        let half = if exp <= 112 {
            sign // flush tiny/denormal to signed zero
        } else if exp >= 143 {
            sign | 0x7c00 // overflow → inf
        } else {
            sign | (((exp - 112) as u16) << 10) | ((frac >> 13) as u16)
        };
        half.to_le_bytes()
    }
}
