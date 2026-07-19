//! Forest spawning: near-field streamed individual trees (LOD0/1/2 via `VisibilityRange`)
//! + always-on far-field per-chunk merged impostor meshes.
//!
//! Entity budget is the constraint (30–60k instances × 5 near entities would be 300k):
//! individual tree entities exist only for 64 m chunks within `NEAR_RADIUS` of the
//! camera (a slow streamer adds/removes whole chunks); beyond that a single merged
//! LOD2 mesh per chunk (one atlas material draw) carries the canopy to `FAR_CULL`.

use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use worldgen::TreeInstance;

use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::trees_mesh::{MeshData, TreeAssets, species_index};

const CHUNK_M: f32 = 64.0;
/// LOD swap distances (camera → tree).
const LOD0_END: f32 = 70.0;
const LOD1_END: f32 = 200.0;
const LOD2_END: f32 = 480.0;
const LOD_BAND: f32 = 18.0;
/// Chunks fully inside this radius get individual trees.
const NEAR_RADIUS: f32 = LOD2_END + 90.0;
const FAR_CULL: f32 = 1500.0;

pub struct ForestPlugin;

impl Plugin for ForestPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ForestIndex>()
            .add_systems(Update, (rebuild_on_ready, stream_near_chunks).chain());
    }
}

/// Trees bucketed by 64 m chunk, in world coordinates; plus the streamer's live set.
#[derive(Resource, Default)]
pub struct ForestIndex {
    pub chunks: HashMap<(i32, i32), Vec<TreeInstance>>,
    live: HashMap<(i32, i32), Vec<Entity>>,
    generation: u32,
}

fn chunk_key(x: f32, z: f32) -> (i32, i32) {
    ((x / CHUNK_M).floor() as i32, (z / CHUNK_M).floor() as i32)
}

fn rebuild_on_ready(
    world: Option<Res<GeneratedWorld>>,
    assets: Option<Res<TreeAssets>>,
    mut index: ResMut<ForestIndex>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    // Change-detection trigger (see terrain_mesh::rebuild_on_ready for why not a message).
    let (Some(world), Some(assets)) = (world, assets) else { return };
    if !world.is_changed() {
        return;
    }
    let off = world_offset(&world.0.height);

    // Old world's entities were swept by genrun; reset the streamer's book-keeping.
    index.chunks.clear();
    index.live.clear();
    index.generation += 1;

    for t in &world.0.trees {
        let mut t = *t;
        t.x += off;
        t.z += off;
        index.chunks.entry(chunk_key(t.x, t.z)).or_default().push(t);
    }

    // Far field: one merged LOD2 mesh per chunk (single material, one draw each).
    for (key, trees) in &index.chunks {
        let mut merged = MeshData::default();
        for t in trees {
            let vm = &assets.variants[species_index(t.species)][t.variant as usize];
            merged.append_transformed(
                &vm.lod2_data,
                Vec3::new(t.x, t.y - 0.25 * t.scale, t.z),
                t.yaw,
                t.scale,
            );
        }
        if merged.positions.is_empty() {
            continue;
        }
        let mesh = merged.to_mesh();
        let aabb = mesh.compute_aabb();
        let mut e = commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(assets.leaf_mat.clone()),
            Transform::default(),
            WorldEntity,
            NotShadowCaster,
            VisibilityRange {
                start_margin: LOD2_END..LOD2_END + LOD_BAND,
                end_margin: FAR_CULL..FAR_CULL + 200.0,
                use_aabb: true,
            },
        ));
        if let Some(aabb) = aabb {
            e.insert(aabb);
        }
        let _ = key;
    }
    info!("forest indexed: {} chunks", index.chunks.len());
}

/// Marker on streamed near-field tree entities (for sanity/debug queries).
#[derive(Component)]
pub struct NearTree;

fn stream_near_chunks(
    mut index: ResMut<ForestIndex>,
    assets: Option<Res<TreeAssets>>,
    cam: Query<&Transform, With<Camera3d>>,
    mut commands: Commands,
    mut last_run: Local<f32>,
    time: Res<Time>,
) {
    let Some(assets) = assets else { return };
    let Ok(cam_tf) = cam.single() else { return };
    // Re-evaluate twice a second — chunk granularity makes per-frame checks pointless.
    *last_run += time.delta_secs();
    if *last_run < 0.5 {
        return;
    }
    *last_run = 0.0;

    let cp = cam_tf.translation;
    let r_chunks = (NEAR_RADIUS / CHUNK_M).ceil() as i32 + 1;
    let centre = chunk_key(cp.x, cp.z);

    // Wanted set: chunks whose centre is within NEAR_RADIUS (+ hysteresis on removal).
    let mut wanted: Vec<(i32, i32)> = Vec::new();
    for dz in -r_chunks..=r_chunks {
        for dx in -r_chunks..=r_chunks {
            let key = (centre.0 + dx, centre.1 + dz);
            if !index.chunks.contains_key(&key) {
                continue;
            }
            let cx = (key.0 as f32 + 0.5) * CHUNK_M;
            let cz = (key.1 as f32 + 0.5) * CHUNK_M;
            if Vec2::new(cx - cp.x, cz - cp.z).length() <= NEAR_RADIUS {
                wanted.push(key);
            }
        }
    }

    // Despawn chunks that drifted out (with margin so the boundary doesn't thrash).
    let drop_keys: Vec<(i32, i32)> = index
        .live
        .keys()
        .filter(|k| {
            let cx = (k.0 as f32 + 0.5) * CHUNK_M;
            let cz = (k.1 as f32 + 0.5) * CHUNK_M;
            Vec2::new(cx - cp.x, cz - cp.z).length() > NEAR_RADIUS + CHUNK_M * 1.5
        })
        .copied()
        .collect();
    for k in drop_keys {
        if let Some(ents) = index.live.remove(&k) {
            for e in ents {
                commands.entity(e).try_despawn();
            }
        }
    }

    // Spawn missing wanted chunks (bounded per tick to spread the cost).
    let mut budget = 6;
    for key in wanted {
        if index.live.contains_key(&key) || budget == 0 {
            continue;
        }
        budget -= 1;
        let mut ents = Vec::new();
        let trees = index.chunks[&key].clone();
        for t in &trees {
            let vm = &assets.variants[species_index(t.species)][t.variant as usize];
            let tf = Transform {
                translation: Vec3::new(t.x, t.y - 0.25 * t.scale, t.z),
                rotation: Quat::from_rotation_y(t.yaw),
                scale: Vec3::splat(t.scale),
            };
            let bark = assets.bark_mats[species_index(t.species)].clone();
            let range = |start: f32, end: f32| VisibilityRange {
                start_margin: if start == 0.0 { 0.0..0.0 } else { start..start + LOD_BAND },
                end_margin: end..end + LOD_BAND,
                use_aabb: false,
            };
            ents.push(
                commands
                    .spawn((
                        Mesh3d(vm.lod0_wood.clone()),
                        MeshMaterial3d(bark.clone()),
                        tf,
                        WorldEntity,
                        NearTree,
                        range(0.0, LOD0_END),
                    ))
                    .id(),
            );
            ents.push(
                commands
                    .spawn((
                        Mesh3d(vm.lod0_leaf.clone()),
                        MeshMaterial3d(assets.leaf_mat.clone()),
                        tf,
                        WorldEntity,
                        NearTree,
                        range(0.0, LOD0_END),
                    ))
                    .id(),
            );
            ents.push(
                commands
                    .spawn((
                        Mesh3d(vm.lod1_wood.clone()),
                        MeshMaterial3d(bark),
                        tf,
                        WorldEntity,
                        NearTree,
                        range(LOD0_END, LOD1_END),
                    ))
                    .id(),
            );
            ents.push(
                commands
                    .spawn((
                        Mesh3d(vm.lod1_leaf.clone()),
                        MeshMaterial3d(assets.leaf_mat.clone()),
                        tf,
                        WorldEntity,
                        NearTree,
                        // Shadow casting only from the LOD0 ring — the mid-ring canopy
                        // shadow pass was a large chunk of the 17-fps regression.
                        NotShadowCaster,
                        range(LOD0_END, LOD1_END),
                    ))
                    .id(),
            );
            ents.push(
                commands
                    .spawn((
                        Mesh3d(vm.lod2.clone()),
                        MeshMaterial3d(assets.leaf_mat.clone()),
                        tf,
                        WorldEntity,
                        NearTree,
                        NotShadowCaster,
                        range(LOD1_END, LOD2_END),
                    ))
                    .id(),
            );
        }
        index.live.insert(key, ents);
    }
}
