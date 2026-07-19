//! Procedural foliage atlas + birch bark — drawn on the CPU at startup, deterministic.
//!
//! One 1024² RGBA atlas, four 512² species quadrants (pine, spruce, broadleaf, birch).
//! Each quadrant: a leaf/needle CLUSTER texture in the top region (alpha-masked cards
//! sample it) and a 64 px opaque bark strip along the bottom (the LOD2 impostor trunk
//! samples that, so a whole far tree renders with the single atlas material).
//!
//! CC0 bark photos cover pine/broadleaf, but ambientCG has no white birch bark — so the
//! birch trunk texture is generated here too (white base, dark horizontal lenticels).

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use worldgen::Species;

pub const ATLAS: u32 = 1024;
const Q: u32 = 512;
/// Leaf region height inside a quadrant; below it sits the bark strip.
pub const LEAF_H: u32 = 432;
pub const BARK_Y0: u32 = 448;

/// Quadrant origin for a species (pine, spruce, broadleaf, birch → 2×2).
pub fn quad_origin(sp: Species) -> (u32, u32) {
    match sp {
        Species::Pine => (0, 0),
        Species::Spruce => (Q, 0),
        Species::Broadleaf => (0, Q),
        Species::Birch => (Q, Q),
    }
}

/// UV rect (u0, v0, u1, v1) of a species' LEAF region.
pub fn leaf_uv(sp: Species) -> (f32, f32, f32, f32) {
    let (qx, qy) = quad_origin(sp);
    let a = ATLAS as f32;
    (qx as f32 / a, qy as f32 / a, (qx + Q) as f32 / a, (qy + LEAF_H) as f32 / a)
}

/// UV rect of a species' BARK strip.
pub fn bark_uv(sp: Species) -> (f32, f32, f32, f32) {
    let (qx, qy) = quad_origin(sp);
    let a = ATLAS as f32;
    (qx as f32 / a, (qy + BARK_Y0) as f32 / a, (qx + Q) as f32 / a, (qy + Q) as f32 / a)
}

struct Canvas {
    px: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        Canvas { px: vec![0u8; (ATLAS * ATLAS * 4) as usize] }
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, c: [f32; 3], a: f32) {
        if x < 0 || y < 0 || x >= ATLAS as i32 || y >= ATLAS as i32 || a <= 0.0 {
            return;
        }
        let i = ((y as u32 * ATLAS + x as u32) * 4) as usize;
        let da = self.px[i + 3] as f32 / 255.0;
        let oa = a + da * (1.0 - a);
        if oa <= 0.0 {
            return;
        }
        for c_i in 0..3 {
            let dst = self.px[i + c_i] as f32 / 255.0;
            let out = (c[c_i] * a + dst * da * (1.0 - a)) / oa;
            self.px[i + c_i] = (out * 255.0) as u8;
        }
        self.px[i + 3] = (oa * 255.0) as u8;
    }

    /// Rotated soft-edged ellipse with a darker midrib — one leaf.
    fn leaf(&mut self, cx: f32, cy: f32, ang: f32, len: f32, wid: f32, col: [f32; 3]) {
        let (s, c) = ang.sin_cos();
        let r = len.max(wid) + 2.0;
        let (x0, x1) = ((cx - r) as i32, (cx + r) as i32);
        let (y0, y1) = ((cy - r) as i32, (cy + r) as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let u = c * dx + s * dy; // along the leaf
                let v = -s * dx + c * dy; // across
                let d = (u / len).powi(2) + (v / wid).powi(2);
                if d < 1.0 {
                    let edge = ((1.0 - d) * 4.0).clamp(0.0, 1.0);
                    // Midrib + slight base-to-tip darkening for depth.
                    let rib = 1.0 - 0.35 * (1.0 - (v.abs() / (wid * 0.14)).clamp(0.0, 1.0));
                    let shade = rib * (0.82 + 0.18 * (u / len + 1.0) * 0.5);
                    self.blend(x, y, [col[0] * shade, col[1] * shade, col[2] * shade], edge);
                }
            }
        }
    }

    /// Thin anti-aliased line — one needle.
    fn needle(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, col: [f32; 3]) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let steps = (len * 1.5) as i32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let px = x0 + dx * t;
            let py = y0 + dy * t;
            let shade = 0.8 + 0.2 * t; // tips lighter
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let d = ((ox * ox + oy * oy) as f32).sqrt();
                    let a = (w - d + 0.5).clamp(0.0, 1.0) * 0.9;
                    self.blend(
                        px as i32 + ox,
                        py as i32 + oy,
                        [col[0] * shade, col[1] * shade, col[2] * shade],
                        a,
                    );
                }
            }
        }
    }
}

fn srgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

fn jitter(rng: &mut worldgen::rng::Rng, col: [f32; 3], amt: f32) -> [f32; 3] {
    let j = 1.0 + rng.signed() * amt;
    [
        (col[0] * j).clamp(0.0, 1.0),
        (col[1] * (1.0 + rng.signed() * amt)).clamp(0.0, 1.0),
        (col[2] * j * 0.9).clamp(0.0, 1.0),
    ]
}

/// Draw a broadleaf-style cluster: many overlapping oval leaves fanning from the centre.
fn cluster_leaves(
    cv: &mut Canvas,
    qx: u32,
    qy: u32,
    rng: &mut worldgen::rng::Rng,
    n: u32,
    len_range: (f32, f32),
    base: [f32; 3],
) {
    let cx = qx as f32 + Q as f32 / 2.0;
    let cy = qy as f32 + LEAF_H as f32 / 2.0;
    for _ in 0..n {
        // Position biased outward — hollow-ish middle reads as a real leaf mass.
        let ang = rng.range(0.0, std::f32::consts::TAU);
        let rad = rng.f32().sqrt() * (Q as f32 * 0.44);
        let lx = cx + ang.cos() * rad;
        let ly = cy + ang.sin() * rad * (LEAF_H as f32 / Q as f32);
        let len = rng.range(len_range.0, len_range.1);
        // Leaves point loosely away from the cluster centre.
        let la = ang + rng.signed() * 0.9;
        cv.leaf(lx, ly, la, len, len * rng.range(0.42, 0.6), jitter(rng, base, 0.16));
    }
}

/// Draw a conifer frond: twig stems fanning upward, needle pairs along each.
fn cluster_needles(
    cv: &mut Canvas,
    qx: u32,
    qy: u32,
    rng: &mut worldgen::rng::Rng,
    stems: u32,
    needle_len: (f32, f32),
    spread: f32,
    base: [f32; 3],
) {
    let cx = qx as f32 + Q as f32 / 2.0;
    let cy = qy as f32 + LEAF_H as f32 * 0.92;
    let twig = srgb(96, 74, 52);
    for s in 0..stems {
        let sa = -std::f32::consts::FRAC_PI_2
            + (s as f32 / (stems - 1).max(1) as f32 - 0.5) * spread
            + rng.signed() * 0.1;
        let slen = LEAF_H as f32 * rng.range(0.62, 0.85);
        let (sx1, sy1) = (cx + sa.cos() * slen, cy + sa.sin() * slen);
        cv.needle(cx, cy, sx1, sy1, 1.6, twig);
        let n_needles = (slen / 4.0) as u32;
        for i in 0..n_needles {
            let t = 0.12 + 0.88 * i as f32 / n_needles as f32;
            let bx = cx + (sx1 - cx) * t;
            let by = cy + (sy1 - cy) * t;
            for side in [-1.0f32, 1.0] {
                let na = sa + side * rng.range(0.55, 0.95);
                let nl = rng.range(needle_len.0, needle_len.1) * (1.0 - t * 0.35);
                cv.needle(
                    bx,
                    by,
                    bx + na.cos() * nl,
                    by + na.sin() * nl,
                    1.0,
                    jitter(rng, base, 0.12),
                );
            }
        }
    }
}

/// Opaque bark strip: vertical (or horizontal for birch) noise banding.
fn bark_strip(cv: &mut Canvas, qx: u32, qy: u32, sp: Species, rng: &mut worldgen::rng::Rng) {
    for y in BARK_Y0..Q {
        for x in 0..Q {
            let fx = x as f32;
            let fy = y as f32;
            let col = match sp {
                Species::Birch => {
                    // White bark, dark horizontal lenticel dashes.
                    let band = worldgen::noise::vnoise(fx * 0.11, fy * 0.5, 77);
                    let dash = worldgen::noise::vnoise(fx * 0.35, fy * 0.06, 12);
                    if band > 0.72 && dash > 0.55 {
                        srgb(40, 36, 32)
                    } else {
                        let v = 0.86 + 0.10 * worldgen::noise::vnoise(fx * 0.05, fy * 0.05, 5);
                        [v, v, v * 0.97]
                    }
                }
                Species::Pine => {
                    let plate = worldgen::noise::vnoise(fx * 0.06, fy * 0.02, 31);
                    let crack = worldgen::noise::vnoise(fx * 0.30, fy * 0.08, 44);
                    let v = 0.45 + plate * 0.5 - (crack > 0.7) as i32 as f32 * 0.3;
                    [0.42 * v + 0.18, 0.27 * v + 0.09, 0.18 * v + 0.05]
                }
                Species::Spruce => {
                    let v = 0.4 + 0.4 * worldgen::noise::vnoise(fx * 0.10, fy * 0.04, 90);
                    [0.30 * v + 0.10, 0.22 * v + 0.08, 0.16 * v + 0.06]
                }
                Species::Broadleaf => {
                    let v = 0.5 + 0.4 * worldgen::noise::vnoise(fx * 0.07, fy * 0.03, 61);
                    [0.38 * v + 0.20, 0.33 * v + 0.18, 0.28 * v + 0.16]
                }
            };
            let _ = rng;
            cv.blend((qx + x) as i32, (qy + y) as i32, col, 1.0);
        }
    }
}

/// Build the full foliage atlas image.
pub fn build_atlas() -> Image {
    let mut cv = Canvas::new();
    let mut rng = worldgen::rng::Rng::new(0x0F01_1A6E);

    for sp in worldgen::ALL_SPECIES {
        let (qx, qy) = quad_origin(sp);
        match sp {
            Species::Pine => cluster_needles(
                &mut cv, qx, qy, &mut rng, 7, (26.0, 40.0), 2.1, srgb(68, 106, 60),
            ),
            Species::Spruce => cluster_needles(
                &mut cv, qx, qy, &mut rng, 9, (18.0, 28.0), 2.4, srgb(52, 88, 58),
            ),
            Species::Broadleaf => cluster_leaves(
                &mut cv, qx, qy, &mut rng, 130, (38.0, 62.0), srgb(78, 126, 54),
            ),
            Species::Birch => cluster_leaves(
                &mut cv, qx, qy, &mut rng, 170, (22.0, 34.0), srgb(116, 152, 64),
            ),
        }
        bark_strip(&mut cv, qx, qy, sp, &mut rng);
    }

    let mut img = Image::new(
        Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
        TextureDimension::D2,
        cv.px,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = crate::texload::repeat_sampler();
    img
}

/// Procedural white birch trunk texture (1024², tileable-ish) for the near-LOD bark
/// material — CC0 libraries had no birch bark.
pub fn build_birch_bark() -> (Image, Image) {
    const S: u32 = 512;
    let mut albedo = vec![0u8; (S * S * 4) as usize];
    let mut rough = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let fx = x as f32;
            let fy = y as f32;
            let band = worldgen::noise::vnoise(fx * 0.05, fy * 0.9, 7);
            let dash = worldgen::noise::vnoise(fx * 0.6, fy * 0.045, 3);
            let scuff = worldgen::noise::vnoise(fx * 0.02, fy * 0.02, 21);
            let i = ((y * S + x) * 4) as usize;
            let (c, r) = if band > 0.70 && dash > 0.52 {
                (srgb(38, 34, 30), 0.9) // lenticel
            } else {
                let v = (0.82 + 0.12 * scuff).min(1.0);
                ([v, v, v * 0.96], 0.55)
            };
            albedo[i] = (c[0] * 255.0) as u8;
            albedo[i + 1] = (c[1] * 255.0) as u8;
            albedo[i + 2] = (c[2] * 255.0) as u8;
            albedo[i + 3] = 255;
            rough[i] = (r * 255.0) as u8;
            rough[i + 1] = rough[i];
            rough[i + 2] = rough[i];
            rough[i + 3] = 255;
        }
    }
    let mut a = Image::new(
        Extent3d { width: S, height: S, depth_or_array_layers: 1 },
        TextureDimension::D2,
        albedo,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    a.sampler = crate::texload::repeat_sampler();
    let mut r = Image::new(
        Extent3d { width: S, height: S, depth_or_array_layers: 1 },
        TextureDimension::D2,
        rough,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    r.sampler = crate::texload::repeat_sampler();
    (a, r)
}
