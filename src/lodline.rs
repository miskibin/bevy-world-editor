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
) {
    let (Some(world), Some(assets)) = (world, assets) else { return };
    if !world.is_changed() {
        return;
    }
    let hf = &world.0.height;
    let off = world_offset(hf);
    let ext = hf.extent();
    // Map-space anchor, default centre.
    let (ax, az) = std::env::var("WED_LODLINE")
        .ok()
        .and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            (v.len() == 2).then(|| (v[0], v[1]))
        })
        .unwrap_or((ext * 0.5, ext * 0.5));

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

    // Frame the whole grid from the front-left, slightly above crown height.
    let centre = Vec3::new(ax + COL_X + off, mean_h + 10.0, az + ROW_Z * 1.5 + off);
    let eye = centre + Vec3::new(-30.0, 14.0, -52.0);
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }
    info!("LOD line staged at map ({ax:.0},{az:.0})");
}
