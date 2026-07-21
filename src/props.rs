//! Undergrowth props: bushes (sprig-card hemispheres, leaf material — they sway),
//! fallen logs + stumps (bark tubes). Merged per 64 m chunk, two entities max per chunk.

use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::{NoCpuCulling, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use worldgen::Species;
use worldgen::rng::Rng;
use worldgen::scatter::{PROP_BUSH_BIRCH, PROP_LOG, PROP_MUSHROOM, PROP_STUMP};

use crate::foliage;
use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};
use crate::trees_mesh::{MeshData, TreeAssets};

const CHUNK_M: f32 = 64.0;
const PROP_FAR: f32 = 280.0;

pub struct PropsPlugin;

/// Chunk meshes waiting to be spawned — built a few per frame so a big map fills in
/// smoothly instead of freezing (a 2 km map has thousands of prop chunks).
#[derive(Resource, Default)]
struct PropQueue(Vec<(MeshData, u8)>);

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PropQueue>()
            .add_systems(Update, (rebuild_on_ready, drain_props).chain());
    }
}

fn drain_props(
    mut queue: ResMut<PropQueue>,
    assets: Option<Res<TreeAssets>>,
    plain: Option<Res<PlainPropMaterial>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(assets), Some(plain)) = (assets, plain) else { return };
    let range = VisibilityRange {
        start_margin: 0.0..0.0,
        end_margin: PROP_FAR..PROP_FAR + 40.0,
        use_aabb: true,
    };
    for _ in 0..4 {
        let Some((data, kind)) = queue.0.pop() else { break };
        let mesh = data.to_mesh();
        let aabb = mesh.compute_aabb();
        let this_range = if kind == 2 {
            VisibilityRange { start_margin: 0.0..0.0, end_margin: 90.0..110.0, use_aabb: true }
        } else {
            range.clone()
        };
        let mut e = commands.spawn((
            Mesh3d(meshes.add(mesh)),
            Transform::default(),
            WorldEntity,
            NoCpuCulling,
            NotShadowCaster,
            this_range,
        ));
        match kind {
            0 => e.insert(MeshMaterial3d(assets.leaf_mat.clone())),
            1 => e.insert(MeshMaterial3d(assets.bark_mats[2].clone())),
            _ => e.insert(MeshMaterial3d(plain.0.clone())),
        };
        if let Some(aabb) = aabb {
            e.insert(aabb);
        }
    }
}

/// One shared material for the vertex-coloured props (mushrooms).
#[derive(Resource)]
struct PlainPropMaterial(Handle<StandardMaterial>);

/// Bush: dome of sprig cards fanning up/outward from the root.
pub(crate) fn bush_data(species: Species, seed: u32) -> MeshData {
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
pub(crate) fn log_data(seed: u32, stump: bool) -> MeshData {
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

/// Mushroom cluster: 3–5 toadstools (4-side stem + 6-side flattened cone cap), colours
/// in vertex colours (rendered with a plain white material).
pub(crate) fn mushroom_data(seed: u32) -> MeshData {
    let mut md = MeshData::default();
    let mut rng = Rng::new(seed);
    let n = 4 + (rng.next_u32() % 4);
    for _ in 0..n {
        let base = Vec3::new(rng.signed() * 0.45, 0.0, rng.signed() * 0.45);
        let h = rng.range(0.14, 0.34); // chunky enough to read at a walking glance
        let cap_r = h * rng.range(0.9, 1.4);
        let stem_r = cap_r * 0.28;
        let cap_col = if rng.chance(0.4) {
            [0.62, 0.18, 0.10, 1.0] // red-brown toadstool
        } else if rng.chance(0.5) {
            [0.55, 0.40, 0.24, 1.0] // tan
        } else {
            [0.80, 0.74, 0.62, 1.0] // cream
        };
        let stem_col = [0.85, 0.80, 0.70, 1.0];
        // Stem: 4-side tube.
        let s0 = md.positions.len() as u32;
        for (y, r) in [(0.0f32, stem_r * 1.15), (h, stem_r)] {
            for k in 0..=4u32 {
                let a = k as f32 / 4.0 * std::f32::consts::TAU;
                let nrm = Vec3::new(a.cos(), 0.0, a.sin());
                md.positions.push((base + nrm * r + Vec3::Y * y).to_array());
                md.normals.push(nrm.to_array());
                md.uvs.push([0.0, 0.0]);
                md.colors.push(stem_col);
            }
        }
        for k in 0..4u32 {
            md.indices.extend_from_slice(&[
                s0 + k, s0 + 5 + k, s0 + k + 1,
                s0 + k + 1, s0 + 5 + k, s0 + 5 + k + 1,
            ]);
        }
        // Cap: 6-side flattened cone (rim ring → apex), slight overhang.
        let c0 = md.positions.len() as u32;
        let apex = base + Vec3::Y * (h + cap_r * 0.55);
        for k in 0..=6u32 {
            let a = k as f32 / 6.0 * std::f32::consts::TAU;
            let nrm = Vec3::new(a.cos() * 0.8, 0.6, a.sin() * 0.8);
            md.positions.push((base + Vec3::new(a.cos(), 0.0, a.sin()) * cap_r + Vec3::Y * h * 0.95).to_array());
            md.normals.push(nrm.to_array());
            md.uvs.push([0.0, 0.0]);
            md.colors.push(cap_col);
        }
        let ai = md.positions.len() as u32;
        md.positions.push(apex.to_array());
        md.normals.push([0.0, 1.0, 0.0]);
        md.uvs.push([0.0, 0.0]);
        md.colors.push([cap_col[0] * 1.15, cap_col[1] * 1.15, cap_col[2] * 1.1, 1.0]);
        for k in 0..6u32 {
            md.indices.extend_from_slice(&[c0 + k, ai, c0 + k + 1]);
        }
    }
    md
}

fn rebuild_on_ready(
    world: Option<Res<GeneratedWorld>>,
    assets: Option<Res<TreeAssets>>,
    mut commands: Commands,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut queue: ResMut<PropQueue>,
    plain_res: Option<Res<PlainPropMaterial>>,
) {
    let (Some(world), Some(_assets)) = (world, assets) else { return };
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
    let shrooms = [mushroom_data(2), mushroom_data(6), mushroom_data(14)];

    let mut leafy: HashMap<(i32, i32), MeshData> = HashMap::default();
    let mut woody: HashMap<(i32, i32), MeshData> = HashMap::default();
    let mut plain: HashMap<(i32, i32), MeshData> = HashMap::default();
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
            k if k == PROP_MUSHROOM => plain.entry(key).or_default().append_transformed(
                &shrooms[(p.x as u32 % 3) as usize],
                Vec3::new(p.x + off, p.y, p.z + off),
                p.yaw,
                p.scale,
            ),
            k => {
                let idx = if k == PROP_BUSH_BIRCH { 2 } else { 0 } + (p.x as u32 % 2) as usize;
                leafy.entry(key).or_default().append_transformed(&bushes[idx], pos, p.yaw, p.scale)
            }
        }
    }

    if plain_res.is_none() {
        commands.insert_resource(PlainPropMaterial(mats.add(StandardMaterial {
            perceptual_roughness: 0.85,
            reflectance: 0.15,
            ..default()
        })));
    }
    let mut count = 0;
    for (data, kind) in leafy
        .into_values()
        .map(|d| (d, 0u8))
        .chain(woody.into_values().map(|d| (d, 1)))
        .chain(plain.into_values().map(|d| (d, 2)))
    {
        if data.positions.is_empty() {
            continue;
        }
        count += 1;
        queue.0.push((data, kind));
    }
    info!("props: {} instances in {count} chunk meshes", world.0.props.len());
    // Staging aids: one world coord per prop family.
    for (kind, label) in [(0u8, "bush"), (PROP_LOG, "log"), (PROP_MUSHROOM, "mushroom")] {
        if let Some(p) = world.0.props.iter().find(|p| p.kind == kind || (kind == 0 && p.kind == PROP_BUSH_BIRCH)) {
            info!("{label} sample at world ({:.0}, {:.0})", p.x + off, p.z + off);
        }
    }
}
