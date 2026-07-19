//! Chunked terrain meshing + discrete LOD.
//!
//! 64×64-cell chunks, two sibling entities each: full-res grid and a stride-4 coarse
//! drape with a perimeter skirt, swapped by `VisibilityRange` with a dithered crossfade.
//! AABBs are computed and inserted UP FRONT (Warbell lesson: Bevy's `calculate_bounds`
//! fills them a frame late, and until then the LOD check falls back to the entity
//! translation — the origin — pinning far-from-origin chunks to the wrong LOD).
//!
//! Vertex layout: positions in world space (map centred on the origin), smooth normals
//! from the heightfield, UV0 = world xz (unused by the splat shader but cheap), UV1 =
//! (moisture, normalised flow) — the splat shader's per-vertex data lane.

use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::genrun::{GeneratedWorld, WATER_LEVEL, WorldEntity, WorldReady, world_offset};
use crate::terrain_mat::GroundMaterial;

pub const CHUNK: usize = 64;
const LOD_DIST: f32 = 260.0;
const LOD_BAND: f32 = 40.0;
const COARSE_STRIDE: usize = 4;
const SKIRT_DROP: f32 = 3.0;

pub struct TerrainMeshPlugin;

impl Plugin for TerrainMeshPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, rebuild_on_ready);
    }
}

fn rebuild_on_ready(
    mut ready: MessageReader<WorldReady>,
    world: Option<Res<GeneratedWorld>>,
    ground: Res<GroundMaterial>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
) {
    if ready.read().next().is_none() {
        return;
    }
    let Some(world) = world else { return };
    let w = &world.0;
    let size = w.height.size;
    let n_chunks = size / CHUNK;

    for cz in 0..n_chunks {
        for cx in 0..n_chunks {
            let full = build_chunk(w, cx * CHUNK, cz * CHUNK, 1);
            let coarse = build_chunk(w, cx * CHUNK, cz * CHUNK, COARSE_STRIDE);
            let aabb = full.compute_aabb();
            let caabb = coarse.compute_aabb();

            let mut e = commands.spawn((
                Mesh3d(meshes.add(full)),
                Transform::default(),
                WorldEntity,
                VisibilityRange {
                    start_margin: 0.0..0.0,
                    end_margin: LOD_DIST..LOD_DIST + LOD_BAND,
                    use_aabb: true,
                },
            ));
            attach_ground_material(&mut e, &ground);
            if let Some(aabb) = aabb {
                e.insert(aabb);
            }

            let mut ce = commands.spawn((
                Mesh3d(meshes.add(coarse)),
                Transform::default(),
                WorldEntity,
                NotShadowCaster,
                VisibilityRange {
                    start_margin: LOD_DIST..LOD_DIST + LOD_BAND,
                    end_margin: 1.0e30..1.0e30, // terrain never culls out entirely
                    use_aabb: true,
                },
            ));
            attach_ground_material(&mut ce, &ground);
            if let Some(caabb) = caabb {
                ce.insert(caabb);
            }
        }
    }

    // Water: one plane over the whole map at the lake level.
    let ext = w.height.extent();
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(ext * 1.2, ext * 1.2))),
        MeshMaterial3d(std_mats.add(StandardMaterial {
            base_color: Color::srgba(0.10, 0.22, 0.24, 0.86),
            perceptual_roughness: 0.08,
            metallic: 0.0,
            reflectance: 0.45,
            alpha_mode: AlphaMode::Blend,
            ..default()
        })),
        Transform::from_xyz(0.0, WATER_LEVEL, 0.0),
        WorldEntity,
        NotShadowCaster,
    ));
}

fn attach_ground_material(e: &mut EntityCommands, ground: &GroundMaterial) {
    match ground {
        GroundMaterial::Splat(h) => {
            e.insert(MeshMaterial3d(h.clone()));
        }
        GroundMaterial::Fallback(h) => {
            e.insert(MeshMaterial3d(h.clone()));
        }
    }
}

/// Build one chunk mesh at the given stride. Stride > 1 adds a perimeter skirt so the
/// coarse drape hides its seam against full-res neighbours.
fn build_chunk(w: &worldgen::World, x0: usize, z0: usize, stride: usize) -> Mesh {
    let hf = &w.height;
    let size = hf.size;
    let off = world_offset(hf);
    let cells = CHUNK / stride;
    let verts_side = cells + 1;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(verts_side * verts_side);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(verts_side * verts_side);
    let mut uv0: Vec<[f32; 2]> = Vec::with_capacity(verts_side * verts_side);
    let mut uv1: Vec<[f32; 2]> = Vec::with_capacity(verts_side * verts_side);
    let mut indices: Vec<u32> = Vec::with_capacity(cells * cells * 6);

    let flow_max = 60.0f32; // log-ish normalisation ceiling for the shader's flow lane

    for vz in 0..verts_side {
        for vx in 0..verts_side {
            let gx = (x0 + vx * stride).min(size - 1);
            let gz = (z0 + vz * stride).min(size - 1);
            let wx = gx as f32 * hf.cell + off;
            let wz = gz as f32 * hf.cell + off;
            let h = hf.get(gx, gz);
            positions.push([wx, h, wz]);
            normals.push(hf.normal_world(gx as f32 * hf.cell, gz as f32 * hf.cell));
            uv0.push([wx, wz]);
            let i = gz * size + gx;
            let flow = (1.0 + w.flow[i]).ln() / flow_max.ln();
            uv1.push([w.moisture[i], flow.clamp(0.0, 1.0)]);
        }
    }
    for vz in 0..cells {
        for vx in 0..cells {
            let a = (vz * verts_side + vx) as u32;
            let b = a + 1;
            let c = a + verts_side as u32;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }

    // Skirt for the coarse drape: duplicate the perimeter ring, dropped straight down.
    if stride > 1 {
        let ring: Vec<u32> = (0..verts_side as u32)
            .map(|i| i) // north edge
            .chain((1..verts_side as u32).map(|i| i * verts_side as u32 + (verts_side as u32 - 1))) // east
            .chain(
                (0..verts_side as u32 - 1)
                    .rev()
                    .map(|i| (verts_side as u32 - 1) * verts_side as u32 + i),
            ) // south
            .chain((1..verts_side as u32 - 1).rev().map(|i| i * verts_side as u32)) // west
            .collect();
        let base = positions.len() as u32;
        for &ri in &ring {
            let p = positions[ri as usize];
            positions.push([p[0], p[1] - SKIRT_DROP, p[2]]);
            normals.push(normals[ri as usize]);
            uv0.push(uv0[ri as usize]);
            uv1.push(uv1[ri as usize]);
        }
        let n = ring.len() as u32;
        for k in 0..n {
            let a = ring[k as usize];
            let b = ring[((k + 1) % n) as usize];
            let a2 = base + k;
            let b2 = base + (k + 1) % n;
            indices.extend_from_slice(&[a, a2, b, b, a2, b2]);
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv0);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}
