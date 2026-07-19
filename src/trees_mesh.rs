//! Skeleton → renderable tree meshes with a 3-level LOD chain.
//!
//! Per (species, variant-seed): LOD0 full tubes + leaf cards, LOD1 pruned twigs / fewer
//! bigger cards, LOD2 a one-material impostor (3-sided trunk sampling the atlas bark
//! strip + a handful of huge cards) whose CPU mesh data is kept so `forest.rs` can merge
//! whole chunks of far trees into single meshes.
//!
//! Leaf-card normals use the skeleton's outward "spherical" directions so a canopy
//! lights as one soft volume, not a heap of flat quads (Warbell facet-bake lesson,
//! realistic edition).

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use worldgen::tree::{LeafAnchor, Segment, TreeSkeleton};
use worldgen::{ALL_SPECIES, Species};

use crate::foliage;

pub const VARIANTS: usize = 4;

/// Plain CPU mesh accumulator — also the merge unit for far-field chunks.
#[derive(Default, Clone)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn to_mesh(&self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs.clone());
        mesh.insert_indices(Indices::U32(self.indices.clone()));
        mesh
    }

    /// Append `other`, transformed by yaw+scale+translation (the far-merge path).
    pub fn append_transformed(&mut self, other: &MeshData, pos: Vec3, yaw: f32, scale: f32) {
        let rot = Quat::from_rotation_y(yaw);
        let base = self.positions.len() as u32;
        for p in &other.positions {
            let v = rot * (Vec3::from_array(*p) * scale) + pos;
            self.positions.push(v.to_array());
        }
        for n in &other.normals {
            self.normals.push((rot * Vec3::from_array(*n)).to_array());
        }
        self.uvs.extend_from_slice(&other.uvs);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }
}

fn ortho_frame(d: Vec3) -> (Vec3, Vec3) {
    let helper = if d.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let u = d.cross(helper).normalize();
    (u, u.cross(d).normalize())
}

/// Sweep tapered tubes along segments. `max_level` prunes twigs for lower LODs.
/// `bark_uv`: None = plain cylindrical UVs (near bark material), Some(rect) = squeeze
/// into the atlas bark strip (LOD2 impostor).
fn build_wood(
    sk: &TreeSkeleton,
    sides: u32,
    max_level: u8,
    bark_uv: Option<(f32, f32, f32, f32)>,
) -> MeshData {
    let mut md = MeshData::default();
    for seg in &sk.segments {
        if seg.level > max_level {
            continue;
        }
        tube(&mut md, seg, sides, bark_uv);
    }
    md
}

fn tube(md: &mut MeshData, seg: &Segment, sides: u32, bark_uv: Option<(f32, f32, f32, f32)>) {
    let a = Vec3::from_array(seg.a);
    let b = Vec3::from_array(seg.b);
    let d = (b - a).normalize_or_zero();
    if d == Vec3::ZERO {
        return;
    }
    let (u, v) = ortho_frame(d);
    let base = md.positions.len() as u32;
    let len = (b - a).length();
    for (end, centre, r) in [(0.0f32, a, seg.ra), (1.0, b, seg.rb)] {
        for s in 0..=sides {
            let ang = s as f32 / sides as f32 * std::f32::consts::TAU;
            let n = u * ang.cos() + v * ang.sin();
            md.positions.push((centre + n * r).to_array());
            md.normals.push(n.to_array());
            let (uu, vv) = (s as f32 / sides as f32, end * len * 0.35);
            match bark_uv {
                None => md.uvs.push([uu, vv]),
                Some((u0, v0, u1, v1)) => {
                    md.uvs.push([u0 + (u1 - u0) * uu, v0 + (v1 - v0) * (vv % 1.0)])
                }
            }
        }
    }
    let ring = sides + 1;
    for s in 0..sides {
        let a0 = base + s;
        let a1 = base + s + 1;
        let b0 = a0 + ring;
        let b1 = a1 + ring;
        md.indices.extend_from_slice(&[a0, b0, a1, a1, b0, b1]);
    }
}

/// Leaf cards: two crossed quads per anchor (at LOD0) or one (lower LODs), UV = the
/// species' atlas leaf region, normals = the anchor's outward direction.
fn build_leaves(
    sk: &TreeSkeleton,
    sp: Species,
    every: usize,
    size_mul: f32,
    crossed: bool,
) -> MeshData {
    build_leaves_varied(sk, sp, every, size_mul, crossed, false)
}

fn build_leaves_varied(
    sk: &TreeSkeleton,
    sp: Species,
    every: usize,
    size_mul: f32,
    crossed: bool,
    vary: bool,
) -> MeshData {
    let mut md = MeshData::default();
    let region = foliage::leaf_uv(sp);
    for (i, l) in sk.leaves.iter().enumerate() {
        if i % every != 0 {
            continue;
        }
        // Near LODs sample a random HALF-SIZE window of the cluster texture per card:
        // smaller cards + varied crops read as individual leafy boughs up close, where a
        // full identical cluster repeated on every card reads as wallpaper.
        let uv = if vary {
            let h1 = (l.pos[0] * 12.9898 + l.pos[1] * 78.233).sin().abs().fract() * 0.5;
            let h2 = (l.pos[2] * 39.425 + l.pos[1] * 11.135).sin().abs().fract() * 0.5;
            let (u0, v0, u1, v1) = region;
            let (du, dv) = (u1 - u0, v1 - v0);
            (u0 + du * h1, v0 + dv * h2, u0 + du * (h1 + 0.5), v0 + dv * (h2 + 0.5))
        } else {
            region
        };
        let n_quads = if crossed { 2 } else { 1 };
        for q in 0..n_quads {
            card(&mut md, l, size_mul, q as f32 * std::f32::consts::FRAC_PI_2, uv);
        }
    }
    md
}

fn card(md: &mut MeshData, l: &LeafAnchor, size_mul: f32, roll: f32, uv: (f32, f32, f32, f32)) {
    let dir = Vec3::from_array(l.dir).normalize_or_zero();
    let dir = if dir == Vec3::ZERO { Vec3::Y } else { dir };
    let (mut u, mut v) = ortho_frame(dir);
    // Roll the card frame around its normal for the crossed second quad.
    let rot = Quat::from_axis_angle(dir, roll + l.pos[0] * 1.7 + l.pos[2] * 2.3);
    u = rot * u;
    v = rot * v;
    let half = l.size * size_mul * 0.5;
    let c = Vec3::from_array(l.pos);
    let base = md.positions.len() as u32;
    for (su, sv, uu, vv) in [
        (-1.0f32, -1.0f32, uv.0, uv.3),
        (1.0, -1.0, uv.2, uv.3),
        (1.0, 1.0, uv.2, uv.1),
        (-1.0, 1.0, uv.0, uv.1),
    ] {
        md.positions.push((c + u * su * half + v * sv * half).to_array());
        md.normals.push(dir.to_array());
        md.uvs.push([uu, vv]);
    }
    md.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// LOD2: 3-sided atlas-bark trunk + ≤12 huge cards. One material → one entity, mergeable.
fn build_lod2(sk: &TreeSkeleton, sp: Species) -> MeshData {
    let mut md = build_wood(sk, 3, 0, Some(foliage::bark_uv(sp)));
    let every = (sk.leaves.len() / 11).max(1);
    let leaves = build_leaves(sk, sp, every, 3.9, false);
    let base = md.positions.len() as u32;
    md.positions.extend_from_slice(&leaves.positions);
    md.normals.extend_from_slice(&leaves.normals);
    md.uvs.extend_from_slice(&leaves.uvs);
    md.indices.extend(leaves.indices.iter().map(|i| i + base));
    md
}

pub struct VariantMeshes {
    pub lod0_wood: Handle<Mesh>,
    pub lod0_leaf: Handle<Mesh>,
    pub lod1_wood: Handle<Mesh>,
    pub lod1_leaf: Handle<Mesh>,
    pub lod2: Handle<Mesh>,
    /// CPU copy of the LOD2 mesh — the far-field per-chunk merge source.
    pub lod2_data: MeshData,
}

#[derive(Resource)]
pub struct TreeAssets {
    /// [species][variant]
    pub variants: Vec<Vec<VariantMeshes>>,
    pub bark_mats: [Handle<StandardMaterial>; 4],
    pub leaf_mat: Handle<StandardMaterial>,
}

pub fn species_index(sp: Species) -> usize {
    match sp {
        Species::Pine => 0,
        Species::Spruce => 1,
        Species::Broadleaf => 2,
        Species::Birch => 3,
    }
}

pub struct TreeAssetsPlugin;

impl Plugin for TreeAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, build_tree_assets);
    }
}

fn bark_material(
    dir: &str,
    tint: Color,
    images: &mut Assets<Image>,
    mats: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    let albedo = crate::texload::load_single(&format!("assets/textures/bark/{dir}/albedo.jpg"), true);
    let normal = crate::texload::load_single(&format!("assets/textures/bark/{dir}/normal.jpg"), false);
    let rough =
        crate::texload::load_single(&format!("assets/textures/bark/{dir}/roughness.jpg"), false);
    mats.add(StandardMaterial {
        base_color: tint,
        base_color_texture: albedo.map(|i| images.add(i)),
        normal_map_texture: normal.map(|i| images.add(i)),
        metallic_roughness_texture: rough.map(|i| images.add(i)),
        perceptual_roughness: 1.0,
        reflectance: 0.18,
        ..default()
    })
}

fn build_tree_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let atlas = images.add(foliage::build_atlas());
    let leaf_mat = mats.add(StandardMaterial {
        base_color_texture: Some(atlas),
        alpha_mode: AlphaMode::Mask(0.42),
        perceptual_roughness: 0.9,
        reflectance: 0.12,
        double_sided: true,
        cull_mode: None,
        // Light leaking through the canopy — cards lit from behind glow instead of
        // going black, the single biggest "real foliage" cue.
        diffuse_transmission: 0.4,
        ..default()
    });

    let (birch_albedo, birch_rough) = foliage::build_birch_bark();
    let birch_mat = mats.add(StandardMaterial {
        base_color_texture: Some(images.add(birch_albedo)),
        metallic_roughness_texture: Some(images.add(birch_rough)),
        perceptual_roughness: 1.0,
        reflectance: 0.18,
        ..default()
    });
    let bark_mats = [
        bark_material("pine", Color::WHITE, &mut images, &mut mats),
        // Spruce: same conifer plates, tinted darker/greyer.
        bark_material("pine", Color::srgb(0.62, 0.58, 0.55), &mut images, &mut mats),
        bark_material("broadleaf", Color::WHITE, &mut images, &mut mats),
        birch_mat,
    ];

    let mut variants = Vec::with_capacity(4);
    for sp in ALL_SPECIES {
        let mut per_variant = Vec::with_capacity(VARIANTS);
        for var in 0..VARIANTS {
            let sk = worldgen::tree::grow(sp, var as u32 * 131 + 7);
            let lod2_data = build_lod2(&sk, sp);
            per_variant.push(VariantMeshes {
                lod0_wood: meshes.add(build_wood(&sk, 6, 2, None).to_mesh()),
                lod0_leaf: meshes.add(build_leaves_varied(&sk, sp, 1, 0.9, true, true).to_mesh()),
                lod1_wood: meshes.add(build_wood(&sk, 4, 1, None).to_mesh()),
                // Uncrossed + every-3rd: the LOD1 ring holds the most trees on screen, so
                // its per-tree quad count decides the frame budget.
                lod1_leaf: meshes.add(build_leaves_varied(&sk, sp, 3, 2.1, false, true).to_mesh()),
                lod2: meshes.add(lod2_data.to_mesh()),
                lod2_data,
            });
        }
        variants.push(per_variant);
    }

    commands.insert_resource(TreeAssets { variants, bark_mats, leaf_mat });
    info!("tree assets built: 4 species x {VARIANTS} variants x 3 LODs");
}
