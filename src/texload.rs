//! CPU-side texture loading: reads the CC0 JPGs with the `image` crate (sync, no asset
//! server — full control over format/mips), builds texture ARRAYS with a hand-computed
//! mip chain (runtime-created `Image`s get no auto-mips in Bevy, and un-mipped 1K ground
//! textures shimmer badly at distance).
//!
//! wgpu data order for arrays+mips is LayerMajor: layer0[mip0..mipN], layer1[mip0..mipN].

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use image::imageops::FilterType;

pub const TEX_SIZE: u32 = 1024;

pub fn repeat_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    })
}

/// Load + resize one map to RGBA at `TEX_SIZE`. None (with a log line) if missing.
fn load_rgba(path: &str) -> Option<image::RgbaImage> {
    match image::open(path) {
        Ok(img) => Some(
            image::imageops::resize(&img.to_rgba8(), TEX_SIZE, TEX_SIZE, FilterType::Triangle),
        ),
        Err(e) => {
            warn!("texture missing: {path} ({e}) — run tools/fetch_textures.ps1");
            None
        }
    }
}

/// Box-filter mip chain for an RGBA image; returns level data incl. mip 0.
fn mip_chain_rgba(mut data: Vec<u8>, mut w: u32, mut h: u32) -> Vec<Vec<u8>> {
    let mut levels = vec![data.clone()];
    while w > 1 || h > 1 {
        let nw = (w / 2).max(1);
        let nh = (h / 2).max(1);
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4usize {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        sum += data[((sy * w + sx) * 4) as usize + c] as u32;
                    }
                    next[((y * nw + x) * 4) as usize + c] = (sum / 4) as u8;
                }
            }
        }
        levels.push(next.clone());
        data = next;
        w = nw;
        h = nh;
    }
    levels
}

fn mip_count(size: u32) -> u32 {
    32 - size.leading_zeros()
}

/// Build an RGBA texture array (layer-major mip data) from per-layer images.
fn make_array(layers: Vec<image::RgbaImage>, srgb: bool) -> Image {
    let n_layers = layers.len() as u32;
    let mut data = Vec::new();
    for img in layers {
        for level in mip_chain_rgba(img.into_raw(), TEX_SIZE, TEX_SIZE) {
            data.extend_from_slice(&level);
        }
    }
    let mut image = Image::new(
        Extent3d { width: TEX_SIZE, height: TEX_SIZE, depth_or_array_layers: n_layers },
        TextureDimension::D2,
        data,
        if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm },
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = mip_count(TEX_SIZE);
    image.sampler = repeat_sampler();
    image
}

/// Build a plain 2D texture (with mips) from one image file. Used for bark maps.
pub fn load_single(path: &str, srgb: bool) -> Option<Image> {
    let img = load_rgba(path)?;
    let mut data = Vec::new();
    for level in mip_chain_rgba(img.into_raw(), TEX_SIZE, TEX_SIZE) {
        data.extend_from_slice(&level);
    }
    let mut image = Image::new(
        Extent3d { width: TEX_SIZE, height: TEX_SIZE, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm },
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = mip_count(TEX_SIZE);
    image.sampler = repeat_sampler();
    Some(image)
}

/// The four ground layers, in shader-layer order.
const GROUND_LAYERS: [&str; 4] = ["grass", "forest_floor", "rock", "dirt"];

pub struct GroundArrays {
    pub albedo: Image,
    pub normal: Image,
    pub rough: Image,
}

/// Load all ground maps into the three arrays. None if any albedo is missing (the app
/// then falls back to a flat-colour material and still runs).
pub fn load_ground_arrays() -> Option<GroundArrays> {
    let mut albedos = Vec::new();
    let mut normals = Vec::new();
    let mut roughs = Vec::new();
    for layer in GROUND_LAYERS {
        let dir = format!("assets/textures/ground/{layer}");
        albedos.push(load_rgba(&format!("{dir}/albedo.jpg"))?);
        normals.push(load_rgba(&format!("{dir}/normal.jpg"))?);
        roughs.push(load_rgba(&format!("{dir}/roughness.jpg"))?);
    }
    Some(GroundArrays {
        albedo: make_array(albedos, true),
        normal: make_array(normals, false),
        rough: make_array(roughs, false),
    })
}
