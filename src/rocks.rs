//! Boulders/outcrops: 4 displaced-icosphere base shapes, instances merged into one mesh
//! per 64 m chunk (rocks never LOD-swap — flat-shaded 3D reads fine at every range).
//! They render with the TERRAIN splat material: steep faces pick up triplanar rock,
//! up-facing facets catch the grass/moss layer — free mossy tops, and the boulders can
//! never colour-mismatch the cliffs they sit on.

use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::{NoCpuCulling, VisibilityRange};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::terrain_mat::GroundMaterial;

const CHUNK_M: f32 = 64.0;
const ROCK_FAR: f32 = 950.0;

pub struct RocksPlugin;

impl Plugin for RocksPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, rebuild_on_ready);
    }
}

struct RockBase {
    positions: Vec<Vec3>,
    indices: Vec<u32>,
}

/// Displaced, squashed icosphere — deterministic per kind.
fn rock_base(kind: u32) -> RockBase {
    let mesh = Mesh::from(Sphere::new(1.0).mesh().ico(2).unwrap());
    let Some(VertexAttributeValues::Float32x3(pos)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("icosphere without positions")
    };
    let indices: Vec<u32> = match mesh.indices() {
        Some(Indices::U32(v)) => v.clone(),
        Some(Indices::U16(v)) => v.iter().map(|&i| i as u32).collect(),
        None => (0..pos.len() as u32).collect(),
    };
    // Per-kind anisotropic stretch + two rotated value-noise lobes.
    let stretch = match kind % 4 {
        0 => Vec3::new(1.25, 0.62, 0.95),
        1 => Vec3::new(1.0, 0.80, 1.35),
        2 => Vec3::new(1.55, 0.55, 1.05),
        _ => Vec3::new(0.9, 1.05, 0.9), // upright outcrop tooth
    };
    let k = kind as f32 * 13.7;
    let positions = pos
        .iter()
        .map(|p| {
            let v = Vec3::from_array(*p);
            let n1 = worldgen::noise::vnoise(v.x * 1.6 + k, v.y * 1.6 + v.z * 0.9, 40 + kind);
            let n2 = worldgen::noise::vnoise(v.z * 2.3 - k, v.x * 2.1 + v.y * 1.3, 90 + kind);
            let r = 1.0 + (n1 - 0.5) * 0.55 + (n2 - 0.5) * 0.30;
            v * r * stretch
        })
        .collect();
    RockBase { positions, indices }
}

fn rebuild_on_ready(
    world: Option<Res<GeneratedWorld>>,
    ground: Option<Res<GroundMaterial>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(world), Some(ground)) = (world, ground) else { return };
    if !world.is_changed() {
        return;
    }
    let off = world_offset(&world.0.height);
    let bases: Vec<RockBase> = (0..4).map(rock_base).collect();

    // Bucket instances per chunk, accumulate transformed triangles.
    let mut chunks: HashMap<(i32, i32), (Vec<[f32; 3]>, Vec<u32>)> = HashMap::default();
    for r in &world.0.rocks {
        let base = &bases[r.kind as usize % 4];
        let key = (
            ((r.x + off) / CHUNK_M).floor() as i32,
            ((r.z + off) / CHUNK_M).floor() as i32,
        );
        let (verts, idx) = chunks.entry(key).or_default();
        let start = verts.len() as u32;
        let rot = Quat::from_rotation_y(r.yaw);
        for p in &base.positions {
            let v = rot * (*p * r.scale) + Vec3::new(r.x + off, r.y - 0.30 * r.scale, r.z + off);
            verts.push(v.to_array());
        }
        idx.extend(base.indices.iter().map(|i| i + start));
    }

    let mut total = 0usize;
    for (_, (verts, idx)) in chunks {
        total += 1;
        let n = verts.len();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, verts);
        // Splat-shader data lanes: modest moisture (allows moss), zero flow.
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.35f32, 0.0]; n]);
        mesh.insert_indices(Indices::U32(idx));
        // Faceted look: duplicate FIRST (flat normals panic on indexed meshes).
        mesh.duplicate_vertices();
        mesh.compute_flat_normals();
        let aabb = mesh.compute_aabb();
        let mut e = commands.spawn((
            Mesh3d(meshes.add(mesh)),
            Transform::default(),
            WorldEntity,
            NoCpuCulling,
            VisibilityRange {
                start_margin: 0.0..0.0,
                end_margin: ROCK_FAR..ROCK_FAR + 80.0,
                use_aabb: true,
            },
        ));
        match &*ground {
            GroundMaterial::Splat(h) => {
                e.insert(MeshMaterial3d(h.clone()));
            }
            GroundMaterial::Fallback(h) => {
                e.insert(MeshMaterial3d(h.clone()));
            }
        }
        if let Some(aabb) = aabb {
            e.insert(aabb);
        }
    }
    info!("rocks: {} instances in {total} chunks", world.0.rocks.len());
}
