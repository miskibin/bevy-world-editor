//! `WED_LODLINE=1` — staging hook: parks a review grid of tree models on the terrain
//! (rows = species: pine, spruce, broadleaf, birch; columns = variant 0 as LOD0, LOD1,
//! LOD2) and aims the camera at it. For model iteration + "show me the LODs" asks.
//! `WED_LODLINE="x,z"` picks the grid's map-space anchor (default map centre).

use bevy::prelude::*;

use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::trees_mesh::TreeAssets;

pub struct LodLinePlugin;

impl Plugin for LodLinePlugin {
    fn build(&self, app: &mut App) {
        if std::env::var("WED_LODLINE").is_ok() {
            app.add_systems(Update, spawn_lodline);
        }
    }
}

const COL_X: f32 = 17.0;
const ROW_Z: f32 = 24.0;

fn spawn_lodline(
    world: Option<Res<GeneratedWorld>>,
    assets: Option<Res<TreeAssets>>,
    mut commands: Commands,
    mut cam: Query<(&mut Transform, &mut crate::flycam::FlyCam)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
) {
    let (Some(world), Some(assets)) = (world, assets) else { return };
    if !world.is_changed() {
        return;
    }
    let hf = &world.0.height;
    let off = world_offset(hf);
    let ext = hf.extent();
    // Map-space anchor: explicit "x,z", else auto-pick a dry, flat stage for the grid
    // (the map centre is frequently a lake).
    let (ax, az) = std::env::var("WED_LODLINE")
        .ok()
        .and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            (v.len() == 2).then(|| (v[0], v[1]))
        })
        .unwrap_or_else(|| {
            let size = hf.size;
            let need_w = (3.0 * COL_X) as usize + 8;
            let need_h = (5.0 * ROW_Z) as usize + 8;
            let mut best = (ext * 0.5, ext * 0.5, f32::MAX);
            for gz in (size / 8..size * 7 / 8).step_by(16) {
                for gx in (size / 8..size * 7 / 8).step_by(16) {
                    // Height spread + water check over the grid's footprint.
                    let (mut lo, mut hi, mut wet) = (f32::MAX, f32::MIN, false);
                    for sz in (0..need_h).step_by(8) {
                        for sx in (0..need_w).step_by(8) {
                            let x = (gx + sx).min(size - 1);
                            let z = (gz + sz).min(size - 1);
                            let i = z * size + x;
                            lo = lo.min(hf.h[i]);
                            hi = hi.max(hf.h[i]);
                            wet |= world.0.water[i].is_finite();
                        }
                    }
                    let spread = if wet { f32::MAX } else { hi - lo };
                    if spread < best.2 {
                        best = (gx as f32, gz as f32, spread);
                    }
                }
            }
            (best.0, best.1)
        });

    let mut mean_h = 0.0;
    for (si, _) in worldgen::ALL_SPECIES.iter().enumerate() {
        for (li, _) in [0; 3].iter().enumerate() {
            let mx = ax + li as f32 * COL_X;
            let mz = az + si as f32 * ROW_Z;
            let h = hf.sample_world(mx, mz);
            mean_h += h;
            let tf = Transform::from_xyz(mx + off, h - 0.15, mz + off);
            let vm = &assets.variants[si][0];
            match li {
                0 => {
                    commands.spawn((
                        Mesh3d(vm.lod0_wood.clone()),
                        MeshMaterial3d(assets.bark_mats[si].clone()),
                        tf,
                        WorldEntity,
                    ));
                    commands.spawn((
                        Mesh3d(vm.lod0_leaf.clone()),
                        MeshMaterial3d(assets.leaf_mat.clone()),
                        tf,
                        WorldEntity,
                    ));
                }
                1 => {
                    commands.spawn((
                        Mesh3d(vm.lod1_wood.clone()),
                        MeshMaterial3d(assets.bark_mats[si].clone()),
                        tf,
                        WorldEntity,
                    ));
                    commands.spawn((
                        Mesh3d(vm.lod1_leaf.clone()),
                        MeshMaterial3d(assets.leaf_mat.clone()),
                        tf,
                        WorldEntity,
                    ));
                }
                _ => {
                    commands.spawn((
                        Mesh3d(vm.lod2.clone()),
                        MeshMaterial3d(assets.leaf_mat.clone()),
                        tf,
                        WorldEntity,
                    ));
                }
            }
        }
    }
    mean_h /= 12.0;

    // Prop review row (bushes ×4, log, stump, mushroom clusters ×2) one row past the trees.
    {
        let pz = az + 4.2 * ROW_Z;
        let mut plain_mat: Option<Handle<StandardMaterial>> = None;
        let items: Vec<(crate::trees_mesh::MeshData, u8)> = vec![
            (crate::props::bush_data(worldgen::Species::Broadleaf, 11), 0),
            (crate::props::bush_data(worldgen::Species::Broadleaf, 23), 0),
            (crate::props::bush_data(worldgen::Species::Birch, 31), 0),
            (crate::props::bush_data(worldgen::Species::Birch, 47), 0),
            (crate::props::log_data(5, false), 1),
            (crate::props::log_data(3, true), 1),
            (crate::props::mushroom_data(2), 2),
            (crate::props::mushroom_data(6), 2),
        ];
        for (i, (data, kind)) in items.into_iter().enumerate() {
            let mx = ax + i as f32 * 6.5;
            let h = hf.sample_world(mx, pz);
            let tf = Transform::from_xyz(mx + off, h - 0.03, pz + off)
                // Mushrooms big enough to review from the framing camera.
                .with_scale(Vec3::splat(if kind == 2 { 2.0 } else { 1.0 }));
            let mut e = commands.spawn((
                Mesh3d(meshes.add(data.to_mesh())),
                tf,
                WorldEntity,
            ));
            match kind {
                0 => e.insert(MeshMaterial3d(assets.leaf_mat.clone())),
                1 => e.insert(MeshMaterial3d(assets.bark_mats[2].clone())),
                _ => {
                    let m = plain_mat
                        .get_or_insert_with(|| {
                            std_mats.add(StandardMaterial {
                                perceptual_roughness: 0.85,
                                ..default()
                            })
                        })
                        .clone();
                    e.insert(MeshMaterial3d(m))
                }
            };
        }
    }

    // Frame the whole grid (trees + the prop row behind) from the front-left.
    let centre = Vec3::new(ax + COL_X * 1.3 + off, mean_h + 8.0, az + ROW_Z * 2.4 + off);
    let eye = centre + Vec3::new(-36.0, 20.0, -70.0);
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }
    info!("LOD line staged at map ({ax:.0},{az:.0})");
}
