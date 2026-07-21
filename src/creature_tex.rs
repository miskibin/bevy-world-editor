//! Procedural creature textures — fur, feathers, butterfly wing patterns. Same
//! philosophy as `foliage.rs` (the leaf atlas + birch bark): no CC0 library has a
//! deer coat, so we synthesize one. The loft meshes carry real UVs (u = around the
//! body, v = along the spine), so fur strands and feather rows read as directional.
//!
//! Fur and feathers are DETAIL maps (warm neutral grey, mean ~0.8) — the coat colour
//! itself lives in the mesh vertex colours, and StandardMaterial multiplies the two.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use worldgen::noise::vnoise;

fn make_image(size: u32, px: Vec<u8>) -> Image {
    let mut img = Image::new(
        Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        TextureDimension::D2,
        px,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = crate::texload::repeat_sampler();
    img
}

fn put(px: &mut [u8], size: u32, x: u32, y: u32, r: f32, g: f32, b: f32) {
    let i = ((y * size + x) * 4) as usize;
    px[i] = (r.clamp(0.0, 1.0) * 255.0) as u8;
    px[i + 1] = (g.clamp(0.0, 1.0) * 255.0) as u8;
    px[i + 2] = (b.clamp(0.0, 1.0) * 255.0) as u8;
    px[i + 3] = 255;
}

/// Deer coat: fur strands elongated along v (the spine direction maps to texture Y),
/// two scales of clumping plus fine grain. Warm neutral so vertex coat colours tint it.
pub fn fur_image() -> Image {
    const S: u32 = 512;
    let mut px = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let fx = x as f32 / S as f32;
            let fy = y as f32 / S as f32;
            // Strand noise: squashed hard in x → vertical streaks. Two octaves of
            // clumping + per-strand grain. All sampled tileably-ish (vnoise repeats
            // softly; the seams sit on fur so they never read).
            let clump = vnoise(fx * 26.0, fy * 5.0, 71) * 0.6 + vnoise(fx * 9.0, fy * 2.0, 72) * 0.4;
            let strand = vnoise(fx * 150.0, fy * 22.0, 73);
            let grain = vnoise(fx * 300.0, fy * 90.0, 74);
            let v = 0.62 + 0.20 * clump + 0.14 * strand + 0.08 * grain;
            // Warm cast: red a hair above, blue below.
            put(&mut px, S, x, y, v * 1.06, v * 0.99, v * 0.88);
        }
    }
    make_image(S, px)
}

/// Feather coat: overlapping scallop rows pointing tailward (+v), darker rims, fine
/// barb streaks. Neutral so plumage vertex colours tint it.
pub fn feather_image() -> Image {
    const S: u32 = 256;
    const ROW: f32 = 26.0;
    const CELL: f32 = 30.0;
    let mut px = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let row = (y as f32 / ROW).floor();
            let fy = (y as f32 / ROW).fract();
            // Alternate rows offset half a feather.
            let cx = (x as f32 / CELL + row * 0.5).fract() - 0.5;
            // Feather tip: rounded lower edge of each cell.
            let tip = (1.0 - fy) + cx * cx * 2.2;
            let rim = if tip < 0.22 { 0.72 } else { 1.0 };
            let barbs = vnoise(x as f32 * 0.9, y as f32 * 0.12, 91) * 0.10;
            let sheen = vnoise(x as f32 * 0.02, y as f32 * 0.02, 92) * 0.12;
            let v = (0.74 + barbs + sheen) * rim;
            put(&mut px, S, x, y, v, v, v * 1.04);
        }
    }
    make_image(S, px)
}

/// Butterfly wing patterns, painted in the wing polygon's normalized bounding box
/// (root at u≈0, v≈0.5). Full colour — the wing mesh's vertex colours stay white.
pub fn wing_images() -> [Image; 3] {
    const S: u32 = 256;
    let mut out = Vec::new();
    for variant in 0..3u32 {
        let mut px = vec![0u8; (S * S * 4) as usize];
        for y in 0..S {
            for x in 0..S {
                let u = x as f32 / S as f32;
                let v = y as f32 / S as f32;
                let du = u;
                let dv = v - 0.5;
                let r = (du * du + dv * dv * 1.7).sqrt();
                let ang = dv.atan2(du.max(1e-3));
                let (mut cr, mut cg, mut cb);
                match variant {
                    0 => {
                        // Monarch: orange field darkening toward the root, black veins
                        // radiating from the root, black border with white spots.
                        let field = 0.55 + 0.45 * (r * 1.4).clamp(0.0, 1.0);
                        cr = 0.90 * field;
                        cg = 0.42 * field;
                        cb = 0.06 * field;
                        // Veins: 7 rays.
                        let vein = (ang * 4.5).sin().abs();
                        if vein > 0.965 && r > 0.12 {
                            cr = 0.06;
                            cg = 0.04;
                            cb = 0.03;
                        }
                        if r > 0.80 {
                            // Border band with two rings of white dots.
                            cr = 0.07;
                            cg = 0.05;
                            cb = 0.04;
                            let dot = (ang * 11.0).sin() * ((r - 0.88) * 28.0).cos();
                            if dot > 0.80 {
                                cr = 0.95;
                                cg = 0.93;
                                cb = 0.88;
                            }
                        }
                    }
                    1 => {
                        // Cabbage white: cream field, dusty grey root, charcoal wingtip,
                        // one round spot mid-wing.
                        let dust = (1.0 - (r * 2.2).clamp(0.0, 1.0)) * 0.25;
                        cr = 0.93 - dust * 0.4;
                        cg = 0.93 - dust * 0.35;
                        cb = 0.88 - dust * 0.2;
                        if r > 0.86 && ang > -0.5 {
                            cr = 0.22;
                            cg = 0.22;
                            cb = 0.24;
                        }
                        let sd = ((u - 0.55) * (u - 0.55) + (v - 0.32) * (v - 0.32)).sqrt();
                        if sd < 0.055 {
                            cr = 0.15;
                            cg = 0.15;
                            cb = 0.17;
                        }
                    }
                    _ => {
                        // Morpho: iridescent blue, brightest mid-wing, near-black border
                        // and root, faint concentric sheen.
                        let mid = 1.0 - ((r - 0.45).abs() * 2.2).clamp(0.0, 1.0);
                        let sheen = 0.85 + 0.15 * ((r * 26.0).sin() * 0.5 + 0.5);
                        cr = (0.05 + 0.20 * mid) * sheen;
                        cg = (0.15 + 0.40 * mid) * sheen;
                        cb = (0.45 + 0.55 * mid) * sheen;
                        if r > 0.82 || r < 0.10 {
                            cr = 0.03;
                            cg = 0.04;
                            cb = 0.08;
                        }
                    }
                }
                put(&mut px, S, x, y, cr, cg, cb);
            }
        }
        out.push(make_image(S, px));
    }
    [out.remove(0), out.remove(0), out.remove(0)]
}
