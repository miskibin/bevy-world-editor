//! Ambient particle emitters — pollen motes (day), fireflies (night), falling leaves,
//! rain and snow (weather). Each emitter is ONE entity with a static mesh of quads;
//! every particle's motion is a pure function of `globals.time` inside
//! `shaders/particles.wgsl`, wrapped in a camera-centred box — zero per-frame CPU, one
//! draw call per emitter (the wind lesson: never mutate materials or transforms per
//! frame). The CPU side only retunes each emitter's `strength` when the day clock or
//! the weather actually moves, and those writes are quantized.

use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::daycycle::{DayClock, sun_dir};
use crate::weather::{Weather, WeatherMode};

#[derive(Clone, Copy, ShaderType, Debug)]
pub struct ParticleUniform {
    /// x = mode, y = strength, z = box extent (m), w = fall speed (m/s).
    pub a: Vec4,
    /// xyz = wind drift (m/s), w = base half-size (m).
    pub b: Vec4,
    /// rgb = tint (scene-referred), w = brightness boost.
    pub tint: Vec4,
}

#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct ParticleMat {
    #[uniform(0)]
    pub params: ParticleUniform,
}

impl Material for ParticleMat {
    fn vertex_shader() -> ShaderRef {
        "shaders/particles.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/particles.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Quads are viewed from any side while the camera flies through the field.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// Emitter kinds, in shader-mode order.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Pollen = 0,
    Firefly = 1,
    Leaf = 2,
    Rain = 3,
    Snow = 4,
}

struct EmitterSpec {
    kind: Kind,
    count: usize,
    ext: f32,
    fall: f32,
    wind: Vec3,
    size: f32,
    tint: Vec4,
}

fn specs() -> [EmitterSpec; 5] {
    [
        // Sunlit dust/pollen: tiny warm motes, most visible in god-ray shafts.
        EmitterSpec {
            kind: Kind::Pollen,
            count: 900,
            ext: 46.0,
            fall: 0.0,
            wind: Vec3::new(1.3, 0.0, -0.6),
            size: 0.014,
            tint: Vec4::new(9000.0, 8200.0, 6200.0, 1.0),
        },
        // Fireflies: green-amber points blinking in the dark near the ground.
        EmitterSpec {
            kind: Kind::Firefly,
            count: 320,
            ext: 60.0,
            fall: 0.0,
            wind: Vec3::ZERO,
            size: 0.05,
            tint: Vec4::new(90.0, 160.0, 30.0, 1.0),
        },
        // Falling leaves: sparse, always on a little, stronger in wind/weather.
        EmitterSpec {
            kind: Kind::Leaf,
            count: 220,
            ext: 55.0,
            fall: 0.55,
            wind: Vec3::new(1.6, 0.0, -0.8),
            size: 0.09,
            // r doubles as the leaf-palette luminance scale (see shader).
            tint: Vec4::new(9000.0, 0.0, 0.0, 1.0),
        },
        // Rain: fast thin streaks.
        EmitterSpec {
            kind: Kind::Rain,
            count: 4200,
            ext: 55.0,
            fall: 11.0,
            wind: Vec3::new(2.0, 0.0, -1.0),
            size: 0.014,
            tint: Vec4::new(3500.0, 3800.0, 4200.0, 1.0),
        },
        // Snow: slow fluttering flakes.
        EmitterSpec {
            kind: Kind::Snow,
            count: 5200,
            ext: 55.0,
            fall: 1.0,
            wind: Vec3::new(1.1, 0.0, -0.5),
            size: 0.045,
            tint: Vec4::new(7000.0, 7200.0, 7600.0, 1.0),
        },
    ]
}

#[derive(Component)]
struct Emitter {
    kind: Kind,
    /// Last strength written to the material — write-gate (the wind lesson).
    applied: f32,
}

#[derive(Component)]
struct EmitterMat(Handle<ParticleMat>);

pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<ParticleMat>::default())
            .add_systems(Startup, spawn_emitters)
            .add_systems(Update, drive_emitters);
    }
}

/// Static quad-cloud mesh: seed positions in [0, ext)^3, per-particle randoms in COLOR.
fn emitter_mesh(count: usize, ext: f32, seed: u32) -> Mesh {
    let mut rng = worldgen::rng::Rng::new(seed);
    let mut positions = Vec::with_capacity(count * 4);
    let mut uvs = Vec::with_capacity(count * 4);
    let mut colors = Vec::with_capacity(count * 4);
    let mut indices = Vec::with_capacity(count * 6);
    for _ in 0..count {
        let p = [rng.range(0.0, ext), rng.range(0.0, ext), rng.range(0.0, ext)];
        let r = [
            rng.range(0.0, 1.0),
            rng.range(0.0, 1.0),
            rng.range(0.0, 1.0),
            rng.range(0.0, 1.0),
        ];
        let base = positions.len() as u32;
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            positions.push(p);
            uvs.push([u as f32, v as f32]);
            colors.push(r);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn spawn_emitters(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<ParticleMat>>,
) {
    for (i, spec) in specs().into_iter().enumerate() {
        let mat = mats.add(ParticleMat {
            params: ParticleUniform {
                a: Vec4::new(spec.kind as u32 as f32, 0.0, spec.ext, spec.fall),
                b: spec.wind.extend(spec.size),
                tint: spec.tint,
            },
        });
        commands.spawn((
            Mesh3d(meshes.add(emitter_mesh(spec.count, spec.ext, 0xF00D + i as u32))),
            MeshMaterial3d(mat.clone()),
            Transform::default(),
            // Positions are generated in the shader around the camera — the mesh AABB is
            // meaningless, so frustum culling must not run on it.
            NoFrustumCulling,
            NotShadowCaster,
            Emitter { kind: spec.kind, applied: -1.0 },
            EmitterMat(mat),
        ));
    }
}

/// Retune emitter strengths from the day clock + weather. Material writes are gated on
/// an actual change so a paused clock costs nothing.
fn drive_emitters(
    clock: Res<DayClock>,
    weather: Res<Weather>,
    mut mats: ResMut<Assets<ParticleMat>>,
    mut emitters: Query<(&mut Emitter, &EmitterMat)>,
) {
    let elev = sun_dir(clock.t).y;
    let daylight = (elev * 4.0).clamp(0.0, 1.0);
    let night = 1.0 - ((elev + 0.14) * 7.0).clamp(0.0, 1.0);
    let (rain, snow) = match weather.mode {
        WeatherMode::Rain => (weather.intensity, 0.0),
        WeatherMode::Snow => (0.0, weather.intensity),
        WeatherMode::Clear => (0.0, 0.0),
    };
    let precip = (rain + snow).min(1.0);
    for (mut em, mat) in &mut emitters {
        let strength = match em.kind {
            // Pollen wants sun; a downpour washes it out.
            Kind::Pollen => daylight * (1.0 - precip),
            // Fireflies come out on clear-ish nights.
            Kind::Firefly => (night - 0.2).clamp(0.0, 1.0) * 1.25 * (1.0 - precip),
            // A few leaves always drift; wind-lashing rain shakes more loose.
            Kind::Leaf => 0.55 + 0.45 * rain,
            Kind::Rain => rain,
            Kind::Snow => snow,
        };
        if (strength - em.applied).abs() < 1.0 / 128.0 {
            continue;
        }
        em.applied = strength;
        if let Some(mut m) = mats.get_mut(&mat.0) {
            m.params.a.y = strength;
        }
    }
}
