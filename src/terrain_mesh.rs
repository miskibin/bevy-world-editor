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
use bevy::camera::visibility::{NoCpuCulling, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::terrain_mat::GroundMaterial;

pub const CHUNK: usize = 64;
const LOD_DIST: f32 = 260.0;
const LOD_BAND: f32 = 40.0;
const COARSE_STRIDE: usize = 4;
const SKIRT_DROP: f32 = 3.0;

/// Chunks still waiting to be meshed. Building every chunk the frame the world lands
/// freezes the app for seconds on a big map (4096 chunks × 2 meshes at 2 km) — the queue
/// spreads it over frames so the world fills in while you fly.
#[derive(Resource, Default)]
pub struct TerrainQueue {
    ground: Vec<(usize, usize)>,
    water: Vec<(usize, usize)>,
    shore: Vec<f32>,
}

/// Chunk pairs meshed per frame. 3 keeps the hitch under ~2 ms on the reference machine.
const BUILD_BUDGET: usize = 3;

pub struct TerrainMeshPlugin;

impl Plugin for TerrainMeshPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainQueue>()
            .add_systems(Update, (enqueue_on_ready, drain_queue).chain());
    }
}

fn enqueue_on_ready(
    world: Option<Res<GeneratedWorld>>,
    mut queue: ResMut<TerrainQueue>,
) {
    // Change-detection trigger, NOT a message: a message written the same frame the
    // resource is queued via commands gets consumed by a reader that then early-returns
    // on the missing resource — and is gone the next frame. is_changed() covers both the
    // first insert and every regenerate overwrite.
    let Some(world) = world else { return };
    if !world.is_changed() {
        return;
    }
    let w = &world.0;
    let size = w.height.size;
    let n_chunks = size / CHUNK;
    queue.ground.clear();
    queue.water.clear();
    for cz in 0..n_chunks {
        for cx in 0..n_chunks {
            queue.ground.push((cx * CHUNK, cz * CHUNK));
            queue.water.push((cx * CHUNK, cz * CHUNK));
        }
    }
    // Shore distance is one BFS over the whole map — do it once here, not per chunk.
    queue.shore = shore_distance(w);
    info!(
        "terrain queued: {} ground + {} water chunks (lakes: {})",
        queue.ground.len(),
        queue.water.len(),
        w.lake_count
    );
    for k in 1..6usize {
        let start = size * size * k / 6;
        if let Some(i) = w.trails[start..].iter().position(|&t| t > 0.9) {
            let i = i + start;
            let off = world_offset(&w.height);
            info!(
                "trail sample {k} at world ({:.0}, {:.0})",
                (i % size) as f32 + off,
                (i / size) as f32 + off
            );
        }
    }
}

/// Mesh a bounded number of queued chunks per frame.
fn drain_queue(
    world: Option<Res<GeneratedWorld>>,
    ground: Res<GroundMaterial>,
    water: Res<crate::water_mat::LakeMaterial>,
    mut queue: ResMut<TerrainQueue>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(world) = world else { return };
    let w = &world.0;
    for _ in 0..BUILD_BUDGET {
        let Some((x0, z0)) = queue.ground.pop() else { break };
        let full = build_chunk(w, x0, z0, 1);
        let coarse = build_chunk(w, x0, z0, COARSE_STRIDE);
        let aabb = full.compute_aabb();
        let caabb = coarse.compute_aabb();
        let mut e = commands.spawn((
            Mesh3d(meshes.add(full)),
            Transform::default(),
            WorldEntity,
            NoCpuCulling,
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
            NoCpuCulling,
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
    for _ in 0..BUILD_BUDGET * 2 {
        let Some((x0, z0)) = queue.water.pop() else { break };
        if let Some(mesh) = build_water_chunk(w, &queue.shore, x0, z0) {
            let aabb = mesh.compute_aabb();
            let mut e = commands.spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(water.0.clone()),
                Transform::default(),
                WorldEntity,
                NoCpuCulling,
                NotShadowCaster,
            ));
            if let Some(aabb) = aabb {
                e.insert(aabb);
            }
        }
    }
}

/// Multi-source BFS over water cells: distance (in metres) to the nearest dry cell.
fn shore_distance(w: &worldgen::World) -> Vec<f32> {
    let size = w.height.size;
    let mut dist = vec![f32::MAX; size * size];
    let mut queue = std::collections::VecDeque::new();
    for z in 0..size {
        for x in 0..size {
            let i = z * size + x;
            if w.water[i].is_finite() {
                // Water cell adjacent to dry land seeds at ~half a cell.
                let dry_neighbour = [(x.wrapping_sub(1), z), (x + 1, z), (x, z.wrapping_sub(1)), (x, z + 1)]
                    .into_iter()
                    .any(|(nx, nz)| {
                        nx < size && nz < size && !w.water[nz * size + nx].is_finite()
                    });
                if dry_neighbour {
                    dist[i] = 0.5;
                    queue.push_back((x, z));
                }
            }
        }
    }
    while let Some((x, z)) = queue.pop_front() {
        let d = dist[z * size + x];
        if d > 12.0 {
            continue; // foam only cares about the first few metres
        }
        for (nx, nz) in [(x.wrapping_sub(1), z), (x + 1, z), (x, z.wrapping_sub(1)), (x, z + 1)] {
            if nx >= size || nz >= size {
                continue;
            }
            let ni = nz * size + nx;
            if w.water[ni].is_finite() && dist[ni] > d + 1.0 {
                dist[ni] = d + 1.0;
                queue.push_back((nx, nz));
            }
        }
    }
    dist
}

/// One quad per submerged cell at its lake's surface height. UV1 = (depth, shore dist).
fn build_water_chunk(
    w: &worldgen::World,
    shore: &[f32],
    x0: usize,
    z0: usize,
) -> Option<Mesh> {
    let hf = &w.height;
    let size = hf.size;
    let off = world_offset(hf);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uv0: Vec<[f32; 2]> = Vec::new();
    let mut uv1: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Highest lake surface among a cell and its 8 neighbours — lets the sheet extend one
    // ring PAST the flagged cells, so the shader's depth-driven alpha fade reaches zero
    // smoothly instead of chopping off in axis-aligned cell steps (the "blocky shore").
    let surf_near = |x: usize, z: usize| -> f32 {
        let mut best = f32::NEG_INFINITY;
        for dz in -1i32..=1 {
            for dx in -1i32..=1 {
                let nx = x as i32 + dx;
                let nz = z as i32 + dz;
                if nx >= 0 && nz >= 0 && (nx as usize) < size && (nz as usize) < size {
                    best = best.max(w.water[nz as usize * size + nx as usize]);
                }
            }
        }
        best
    };
    // Per-CORNER shore distance (average of the 4 touching cells, dry = 0) — a per-cell
    // constant here is what painted the foam as zigzag cell blocks.
    let corner_shore = |gx: usize, gz: usize| -> f32 {
        let mut sum = 0.0f32;
        let mut n = 0.0f32;
        for (cx, cz) in [
            (gx.wrapping_sub(1), gz.wrapping_sub(1)),
            (gx, gz.wrapping_sub(1)),
            (gx.wrapping_sub(1), gz),
            (gx, gz),
        ] {
            if cx < size && cz < size {
                let i = cz * size + cx;
                sum += if w.water[i].is_finite() { shore[i].min(12.0) } else { 0.0 };
                n += 1.0;
            }
        }
        sum / n.max(1.0)
    };

    for z in z0..(z0 + CHUNK).min(size - 1) {
        for x in x0..(x0 + CHUNK).min(size - 1) {
            let surf = surf_near(x, z);
            if !surf.is_finite() {
                continue;
            }
            // Skip cells fully above the sheet (ring cells on rising ground).
            let submerged = [(x, z), (x + 1, z), (x, z + 1), (x + 1, z + 1)]
                .into_iter()
                .any(|(gx, gz)| surf - hf.get(gx, gz) > 0.0);
            if !submerged {
                continue;
            }
            let base = positions.len() as u32;
            for (dx, dz) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
                let gx = x + dx;
                let gz = z + dz;
                let wx = gx as f32 * hf.cell + off;
                let wz = gz as f32 * hf.cell + off;
                let depth = (surf - hf.get(gx, gz)).max(0.0);
                positions.push([wx, surf, wz]);
                normals.push([0.0, 1.0, 0.0]);
                uv0.push([wx, wz]);
                uv1.push([depth, corner_shore(gx, gz)]);
            }
            indices.extend_from_slice(&[base, base + 2, base + 1, base + 1, base + 2, base + 3]);
        }
    }
    if positions.is_empty() {
        return None;
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
    Some(mesh)
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
            let i = gz * size + gx;
            // UV0.x = trail wear (the splat shader's beaten-path lane); y spare.
            uv0.push([w.trails[i], 0.0]);
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
