//! Near-camera grass: procedural blade tufts merged per 16 m chunk, streamed in a ~55 m
//! ring around the camera, swaying via the same LeafSway vertex stage as the foliage.
//! Untextured — colour lives in vertex colours (moisture-tinted), blades are plain
//! triangles, so the whole ring is a handful of draw calls.

use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::{NoCpuCulling, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::mesh::PrimitiveTopology;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use worldgen::rng::{Rng, lowbias32};

use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::trees_mesh::{LeafMaterial, LeafSway};

const CHUNK_M: f32 = 16.0;
const RING: f32 = 55.0;
const FADE: f32 = 10.0;
/// Candidate sites per chunk axis (16 m / ~0.85 m spacing).
const SITES: i32 = 19;

#[derive(Resource, Default)]
struct GrassState {
    live: HashMap<(i32, i32), Entity>,
    generation: u32,
}

#[derive(Resource)]
pub struct GrassMaterial(pub Handle<LeafMaterial>);

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GrassState>()
            .add_systems(PreStartup, setup_material)
            .add_systems(Update, stream_grass);
    }
}

fn setup_material(mut commands: Commands, mut mats: ResMut<Assets<LeafMaterial>>) {
    commands.insert_resource(GrassMaterial(mats.add(bevy::pbr::ExtendedMaterial {
        base: StandardMaterial {
            perceptual_roughness: 0.95,
            reflectance: 0.10,
            double_sided: true,
            cull_mode: None,
            ..default()
        },
        extension: LeafSway {},
    })));
}

fn stream_grass(
    world: Option<Res<GeneratedWorld>>,
    mat: Option<Res<GrassMaterial>>,
    mut state: ResMut<GrassState>,
    cam: Query<&Transform, With<Camera3d>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut last_run: Local<f32>,
    time: Res<Time>,
) {
    if std::env::var("WED_NOGRASS").is_ok() {
        return;
    }
    let (Some(world), Some(mat), Ok(cam)) = (world, mat, cam.single()) else { return };
    // World swapped → all grass entities were despawned with WorldEntity; reset books.
    if world.is_changed() {
        state.live.clear();
        state.generation += 1;
    }
    *last_run += time.delta_secs();
    if *last_run < 0.25 {
        return;
    }
    *last_run = 0.0;

    let cp = cam.translation;
    let r_chunks = (RING / CHUNK_M).ceil() as i32 + 1;
    let centre = ((cp.x / CHUNK_M).floor() as i32, (cp.z / CHUNK_M).floor() as i32);

    // Drop chunks that left the ring (hysteresis one chunk).
    let drop: Vec<(i32, i32)> = state
        .live
        .keys()
        .filter(|k| {
            let cx = (k.0 as f32 + 0.5) * CHUNK_M;
            let cz = (k.1 as f32 + 0.5) * CHUNK_M;
            Vec2::new(cx - cp.x, cz - cp.z).length() > RING + CHUNK_M * 1.6
        })
        .copied()
        .collect();
    for k in drop {
        if let Some(e) = state.live.remove(&k) {
            commands.entity(e).try_despawn();
        }
    }

    // Spawn missing chunks in range (bounded per tick).
    let mut budget = 5;
    for dz in -r_chunks..=r_chunks {
        for dx in -r_chunks..=r_chunks {
            if budget == 0 {
                return;
            }
            let key = (centre.0 + dx, centre.1 + dz);
            if state.live.contains_key(&key) {
                continue;
            }
            let cx = (key.0 as f32 + 0.5) * CHUNK_M;
            let cz = (key.1 as f32 + 0.5) * CHUNK_M;
            if Vec2::new(cx - cp.x, cz - cp.z).length() > RING {
                continue;
            }
            if let Some(mesh) = build_grass_chunk(&world.0, key) {
                let aabb = mesh.compute_aabb();
                let mut e = commands.spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(mat.0.clone()),
                    Transform::default(),
                    WorldEntity,
            NoCpuCulling,
                    NotShadowCaster,
                    VisibilityRange {
                        start_margin: 0.0..0.0,
                        end_margin: RING - FADE..RING,
                        use_aabb: true,
                    },
                ));
                if let Some(aabb) = aabb {
                    e.insert(aabb);
                }
                state.live.insert(key, e.id());
                budget -= 1;
                if state.live.len() == 1 {
                    info!("grass: first chunk spawned");
                }
            } else {
                // Nothing grows here — remember with a placeholder so we don't rebuild.
                state.live.insert(key, Entity::PLACEHOLDER);
            }
        }
    }
}

/// One merged tuft mesh per chunk; None if the chunk grows nothing.
fn build_grass_chunk(w: &worldgen::World, key: (i32, i32)) -> Option<Mesh> {
    let hf = &w.height;
    let size = hf.size;
    let off = world_offset(hf);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();

    for sz in 0..SITES {
        for sx in 0..SITES {
            let seed = lowbias32(
                (key.0 as u32)
                    .wrapping_mul(0x9E37_79B9)
                    .wrapping_add((key.1 as u32).wrapping_mul(0x85EB_CA6B))
                    .wrapping_add((sz * SITES + sx) as u32 * 0x0068_E31D),
            );
            let mut rng = Rng::new(seed);
            let wx = key.0 as f32 * CHUNK_M + (sx as f32 + rng.f32()) * (CHUNK_M / SITES as f32);
            let wz = key.1 as f32 * CHUNK_M + (sz as f32 + rng.f32()) * (CHUNK_M / SITES as f32);
            // Map-space lookup.
            let mx = wx - off;
            let mz = wz - off;
            if mx < 1.0 || mz < 1.0 || mx >= (size - 2) as f32 || mz >= (size - 2) as f32 {
                continue;
            }
            let i = (mz as usize) * size + mx as usize;
            let moist = w.moisture[i];
            // Meadow patches (matches the terrain shader's macro sward variation): tall,
            // dense, flower-speckled swards vs ordinary short turf.
            let meadow = worldgen::noise::fbm(wx / 135.0, wz / 135.0, 2, 777);
            let meadowness = ((meadow - 0.45) / 0.25).clamp(0.0, 1.0);
            if w.water[i].is_finite()
                || w.slope[i] > 0.55
                || w.trails[i] > 0.35
                || hf.h[i] < crate::genrun::WATER_LEVEL + 0.6
                || moist < 0.12
            {
                continue;
            }
            // Stocking rises with moisture; trails and dry ridges stay sparse. Open
            // meadows read bald below ~0.5 — a lawn is thousands of tufts, not dozens.
            if !rng.chance((0.5 + moist * 0.45) * (0.75 + 0.45 * meadowness)) {
                continue;
            }
            let base = Vec3::new(wx, hf.sample_world(mx, mz) - 0.02, wz);
            // Colour: dry straw → lush green by moisture, slight per-tuft jitter.
            let lush = (moist * 1.3).min(1.0);
            let jitter = 0.85 + rng.f32() * 0.3;
            let col = [
                (0.32 - 0.20 * lush) * jitter,
                (0.34 + 0.08 * lush) * jitter,
                (0.10 + 0.02 * lush) * jitter,
            ];
            let scale = 1.0 + meadowness * 0.9; // meadow grass stands taller
            tuft(&mut positions, &mut normals, &mut colors, &mut rng, base, col, scale);
            // Wildflowers: only in meadows, sparse enough to read as speckles.
            if meadowness > 0.35 && rng.chance(0.06 * meadowness) {
                let petal = match rng.next_u32() % 4 {
                    0 => [0.95, 0.92, 0.80],  // white
                    1 => [0.95, 0.83, 0.25],  // yellow
                    2 => [0.72, 0.55, 0.90],  // violet
                    _ => [0.90, 0.45, 0.42],  // red
                };
                flower(&mut positions, &mut normals, &mut colors, &mut rng, base, col, petal);
            }
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
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    Some(mesh)
}

/// 7 single-triangle blades leaning outward from a common root — non-indexed.
/// `scale` stretches the whole tuft (meadow swards stand taller than turf).
fn tuft(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rng: &mut Rng,
    base: Vec3,
    col: [f32; 3],
    scale: f32,
) {
    let blades = 7;
    for b in 0..blades {
        let ang = b as f32 / blades as f32 * std::f32::consts::TAU + rng.f32();
        let lean = Vec2::new(ang.cos(), ang.sin()) * rng.range(0.06, 0.22);
        let h = rng.range(0.22, 0.5) * scale;
        let hw = rng.range(0.015, 0.035);
        let side = Vec3::new(-lean.y, 0.0, lean.x).normalize_or_zero() * hw;
        let tip = base + Vec3::new(lean.x, h, lean.y);
        // Up-ish normal so blades take the ground's soft lighting, not black backsides.
        let n = [lean.x * 0.4, 1.0, lean.y * 0.4];
        let dark = [col[0] * 0.55, col[1] * 0.55, col[2] * 0.55, 1.0];
        let light = [col[0], col[1], col[2], 1.0];
        for (p, c) in [
            (base - side, dark),
            (base + side, dark),
            (tip, light),
        ] {
            positions.push(p.to_array());
            normals.push(n);
            colors.push(c);
        }
    }
}

/// A single wildflower: a tall stem blade capped by a 3-triangle petal star.
fn flower(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rng: &mut Rng,
    base: Vec3,
    stem_col: [f32; 3],
    petal: [f32; 3],
) {
    let h = rng.range(0.35, 0.62);
    let head = base + Vec3::new(rng.signed() * 0.05, h, rng.signed() * 0.05);
    // Stem.
    let side = Vec3::new(0.012, 0.0, 0.0);
    for (p, c) in [
        (base - side, [stem_col[0] * 0.6, stem_col[1] * 0.6, stem_col[2] * 0.6, 1.0]),
        (base + side, [stem_col[0] * 0.6, stem_col[1] * 0.6, stem_col[2] * 0.6, 1.0]),
        (head, [stem_col[0], stem_col[1], stem_col[2], 1.0]),
    ] {
        positions.push(p.to_array());
        normals.push([0.0, 1.0, 0.0]);
        colors.push(c);
    }
    // Petal star: 3 small triangles fanning around the head.
    let r = rng.range(0.035, 0.065);
    let pc = [petal[0], petal[1], petal[2], 1.0];
    for k in 0..3 {
        let a = k as f32 / 3.0 * std::f32::consts::TAU + rng.f32();
        let (s0, c0) = a.sin_cos();
        let (s1, c1) = (a + 1.2).sin_cos();
        for p in [
            head,
            head + Vec3::new(c0 * r, 0.02, s0 * r),
            head + Vec3::new(c1 * r, 0.02, s1 * r),
        ] {
            positions.push(p.to_array());
            normals.push([0.0, 1.0, 0.0]);
            colors.push(pc);
        }
    }
}
