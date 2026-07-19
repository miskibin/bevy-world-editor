//! Undergrowth props: bushes (sprig-card hemispheres, leaf material — they sway),
//! fallen logs + stumps (bark tubes). Merged per 64 m chunk, two entities max per chunk.

use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use worldgen::Species;
use worldgen::rng::Rng;
use worldgen::scatter::{PROP_BUSH_BIRCH, PROP_LOG, PROP_STUMP};

use crate::foliage;
use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::trees_mesh::{MeshData, TreeAssets};

const CHUNK_M: f32 = 64.0;
const PROP_FAR: f32 = 280.0;

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, rebuild_on_ready);
    }
}

/// Bush: dome of sprig cards fanning up/outward from the root.
fn bush_data(species: Species, seed: u32) -> MeshData {
    let mut md = MeshData::default();
    let mut rng = Rng::new(seed);
    let (u0, v0, u1, v1) = foliage::leaf_uv(species);
    let tint = match species {
        Species::Birch => [1.15, 1.1, 0.65],
        _ => [0.95, 1.0, 0.8],
    };
    let cards = 9;
    for c in 0..cards {
        let az = c as f32 / cards as f32 * std::f32::consts::TAU + rng.f32();
        let pitch = rng.range(0.5, 1.15); // up-and-out
        let dir = Vec3::new(
            az.cos() * pitch.cos(),
            pitch.sin(),
            az.sin() * pitch.cos(),
        )
        .normalize();
        let len = rng.range(0.9, 1.5);
        let side = dir.cross(Vec3::Y).normalize_or_zero();
        let side = if side == Vec3::ZERO { Vec3::X } else { side } * (len * 0.5);
        let base_p = Vec3::new(rng.signed() * 0.15, 0.02, rng.signed() * 0.15);
        let j = 0.85 + rng.f32() * 0.3;
        let col = [tint[0] * j, tint[1] * j, tint[2] * j * 0.95, 1.0];
        let start = md.positions.len() as u32;
        for (s, a, uu, vv) in [
            (-1.0f32, 0.0f32, u0, v1),
            (1.0, 0.0, u1, v1),
            (1.0, 1.0, u1, v0),
            (-1.0, 1.0, u0, v0),
        ] {
            let p = base_p + side * s + dir * a * len;
            md.positions.push(p.to_array());
            md.normals.push(dir.to_array());
            md.uvs.push([uu, vv]);
            md.colors.push(col);
        }
        md.indices.extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    }
    md
}

/// Fallen log: tapered 6-side tube lying along +X with a slight tilt; bark UVs.
fn log_data(seed: u32, stump: bool) -> MeshData {
    let mut md = MeshData::default();
    let mut rng = Rng::new(seed);
    let (len, r0, r1, dir) = if stump {
        (rng.range(0.35, 0.7), 0.28, 0.30, Vec3::Y)
    } else {
        let tilt = rng.range(-0.06, 0.10);
        (rng.range(3.5, 6.5), 0.30, 0.16, Vec3::new(1.0, tilt, rng.signed() * 0.2).normalize())
    };
    let sides = 6u32;
    let u = dir.cross(Vec3::Y).normalize_or_zero();
    let u = if u == Vec3::ZERO { Vec3::X } else { u };
    let v = u.cross(dir);
    let start_p = if stump { Vec3::ZERO } else { Vec3::new(-len * 0.5, r0 * 0.7, 0.0) };
    for (end, r) in [(0.0f32, r0), (1.0, r1)] {
        let centre = start_p + dir * end * len;
        for s in 0..=sides {
            let ang = s as f32 / sides as f32 * std::f32::consts::TAU;
            let n = u * ang.cos() + v * ang.sin();
            md.positions.push((centre + n * r).to_array());
            md.normals.push(n.to_array());
            md.uvs.push([s as f32 / sides as f32, end * len * 0.35]);
            md.colors.push([0.9, 0.85, 0.78, 1.0]);
        }
    }
    let ring = sides + 1;
    for s in 0..sides {
        let a0 = s;
        let b0 = a0 + ring;
        md.indices.extend_from_slice(&[a0, b0, a0 + 1, a0 + 1, b0, b0 + 1]);
    }
    md
}

fn rebuild_on_ready(
    world: Option<Res<GeneratedWorld>>,
    assets: Option<Res<TreeAssets>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(world), Some(assets)) = (world, assets) else { return };
    if !world.is_changed() {
        return;
    }
    let off = world_offset(&world.0.height);

    // Base variants (2 per bush species, 2 log seeds, 1 stump).
    let bushes = [
        bush_data(Species::Broadleaf, 11),
        bush_data(Species::Broadleaf, 23),
        bush_data(Species::Birch, 31),
        bush_data(Species::Birch, 47),
    ];
    let logs = [log_data(5, false), log_data(9, false)];
    let stump = log_data(3, true);

    let mut leafy: HashMap<(i32, i32), MeshData> = HashMap::default();
    let mut woody: HashMap<(i32, i32), MeshData> = HashMap::default();
    for p in &world.0.props {
        let key = (((p.x + off) / CHUNK_M).floor() as i32, ((p.z + off) / CHUNK_M).floor() as i32);
        let pos = Vec3::new(p.x + off, p.y - 0.05, p.z + off);
        match p.kind {
            k if k == PROP_LOG => woody.entry(key).or_default().append_transformed(
                &logs[(p.x as u32 % 2) as usize],
                pos,
                p.yaw,
                p.scale,
            ),
            k if k == PROP_STUMP => {
                woody.entry(key).or_default().append_transformed(&stump, pos, p.yaw, p.scale)
            }
            k => {
                let idx = if k == PROP_BUSH_BIRCH { 2 } else { 0 } + (p.x as u32 % 2) as usize;
                leafy.entry(key).or_default().append_transformed(&bushes[idx], pos, p.yaw, p.scale)
            }
        }
    }

    let range = VisibilityRange {
        start_margin: 0.0..0.0,
        end_margin: PROP_FAR..PROP_FAR + 40.0,
        use_aabb: true,
    };
    let mut count = 0;
    for (data, leaf) in leafy
        .values()
        .map(|d| (d, true))
        .chain(woody.values().map(|d| (d, false)))
    {
        if data.positions.is_empty() {
            continue;
        }
        count += 1;
        let mesh = data.to_mesh();
        let aabb = mesh.compute_aabb();
        let mut e = commands.spawn((
            Mesh3d(meshes.add(mesh)),
            Transform::default(),
            WorldEntity,
            NotShadowCaster,
            range.clone(),
        ));
        if leaf {
            e.insert(MeshMaterial3d(assets.leaf_mat.clone()));
        } else {
            e.insert(MeshMaterial3d(assets.bark_mats[2].clone()));
        }
        if let Some(aabb) = aabb {
            e.insert(aabb);
        }
    }
    info!("props: {} instances in {count} chunk meshes", world.0.props.len());
}
