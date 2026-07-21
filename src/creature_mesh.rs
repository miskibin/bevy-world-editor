//! Procedural creature meshes — deer, bird, butterfly. Unlike the Warbell-style faceted
//! props, these are SMOOTH lofted surfaces (an elliptical cross-section swept along a
//! spine, smooth normals, soft dorsal→belly colour gradients) so they sit in the
//! realistic look of the rest of the scene. Parts pivot at their joints (neck root,
//! hips, wing roots) so the behaviour systems can pose them as child entities.

use bevy::prelude::*;

use crate::trees_mesh::MeshData;

/// One station along a loft spine.
#[derive(Clone, Copy)]
struct Ring {
    /// Centre of the cross-section.
    c: Vec3,
    /// Horizontal (side-to-side) half-width.
    rh: f32,
    /// Vertical half-height.
    rv: f32,
    /// Dorsal (top) colour at this station.
    top: [f32; 3],
    /// Ventral (belly) colour at this station.
    bot: [f32; 3],
}

fn ring(c: Vec3, rh: f32, rv: f32, top: [f32; 3], bot: [f32; 3]) -> Ring {
    Ring { c, rh, rv, top, bot }
}

/// Sweep an ellipse along the spine. Tangents from neighbouring stations, side vector
/// kept horizontal (cross with world Y) — right for bodies/necks/legs, which never roll.
/// Ends are closed by collapsing an extra ring to a point. Normals are exact ellipse
/// normals, so the surface shades smooth without any duplicate/flat pass.
fn loft(d: &mut MeshData, rings: &[Ring], segs: usize) {
    let n = rings.len();
    assert!(n >= 2);
    let base = d.positions.len() as u32;
    // Per-station frames.
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        let t = if i == 0 {
            rings[1].c - rings[0].c
        } else if i == n - 1 {
            rings[n - 1].c - rings[n - 2].c
        } else {
            rings[i + 1].c - rings[i - 1].c
        }
        .normalize_or_zero();
        let side = t.cross(Vec3::Y).normalize_or(Vec3::Z);
        let up = side.cross(t).normalize_or(Vec3::Y);
        frames.push((side, up));
    }
    for (i, r) in rings.iter().enumerate() {
        let (side, up) = frames[i];
        for j in 0..=segs {
            let a = j as f32 / segs as f32 * std::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            let pos = r.c + side * (c * r.rh) + up * (s * r.rv);
            // Exact ellipse normal: gradient of the implicit form.
            let nrm = (side * (c / r.rh.max(1e-4)) + up * (s / r.rv.max(1e-4))).normalize();
            // Belly blend: s < 0 is the underside.
            let w = (0.5 - 0.5 * s).clamp(0.0, 1.0);
            let col = [
                r.top[0] + (r.bot[0] - r.top[0]) * w,
                r.top[1] + (r.bot[1] - r.top[1]) * w,
                r.top[2] + (r.bot[2] - r.top[2]) * w,
                1.0,
            ];
            d.positions.push(pos.to_array());
            d.normals.push(nrm.to_array());
            d.uvs.push([0.0, 0.0]);
            d.colors.push(col);
        }
    }
    let stride = (segs + 1) as u32;
    for i in 0..(n - 1) as u32 {
        for j in 0..segs as u32 {
            let a = base + i * stride + j;
            let b = a + stride;
            d.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    // End caps: a centre-point fan with the spine tangent as the normal.
    for (i, dir) in [(0usize, -1.0f32), (n - 1, 1.0)] {
        let r = rings[i];
        let t = if i == 0 { rings[0].c - rings[1].c } else { rings[n - 1].c - rings[n - 2].c }
            .normalize_or_zero();
        let centre = d.positions.len() as u32;
        d.positions.push(r.c.to_array());
        d.normals.push(t.to_array());
        d.uvs.push([0.0, 0.0]);
        d.colors.push([
            (r.top[0] + r.bot[0]) * 0.5,
            (r.top[1] + r.bot[1]) * 0.5,
            (r.top[2] + r.bot[2]) * 0.5,
            1.0,
        ]);
        let ring0 = base + i as u32 * stride;
        for j in 0..segs as u32 {
            if dir < 0.0 {
                d.indices.extend_from_slice(&[centre, ring0 + j, ring0 + j + 1]);
            } else {
                d.indices.extend_from_slice(&[centre, ring0 + j + 1, ring0 + j]);
            }
        }
    }
}

/// A flat two-sided polygon (fan-triangulated) in the local XY plane, extruded normal
/// ±Z. `pts` wind counter-clockwise; `cols` per point.
fn flat_poly(d: &mut MeshData, pts: &[Vec2], cols: &[[f32; 4]]) {
    let centroid: Vec2 = pts.iter().copied().sum::<Vec2>() / pts.len() as f32;
    let ccol = {
        let mut c = [0.0f32; 4];
        for col in cols {
            for k in 0..4 {
                c[k] += col[k];
            }
        }
        for k in 0..4 {
            c[k] /= cols.len() as f32;
        }
        c
    };
    for zn in [1.0f32, -1.0] {
        let n = [0.0, 0.0, zn];
        let base = d.positions.len() as u32;
        d.positions.push([centroid.x, centroid.y, 0.0]);
        d.normals.push(n);
        d.uvs.push([0.0, 0.0]);
        d.colors.push(ccol);
        for (p, c) in pts.iter().zip(cols) {
            d.positions.push([p.x, p.y, 0.0]);
            d.normals.push(n);
            d.uvs.push([0.0, 0.0]);
            d.colors.push(*c);
        }
        let m = pts.len() as u32;
        for j in 0..m {
            let a = base + 1 + j;
            let b = base + 1 + (j + 1) % m;
            if zn > 0.0 {
                d.indices.extend_from_slice(&[base, a, b]);
            } else {
                d.indices.extend_from_slice(&[base, b, a]);
            }
        }
    }
}

// ── Deer (roe-deer proportions) ─────────────────────────────────────────────────────

const COAT: [f32; 3] = [0.46, 0.32, 0.20];
const COAT_DARK: [f32; 3] = [0.38, 0.26, 0.16];
const BELLY: [f32; 3] = [0.66, 0.58, 0.45];
const RUMP: [f32; 3] = [0.85, 0.80, 0.68];
const MUZZLE: [f32; 3] = [0.22, 0.16, 0.11];

/// Torso: one smooth tube from rump to chest, arched back, cream rump patch, short
/// tail. Pivot at ground under the chest; faces +X.
pub fn deer_body() -> MeshData {
    let mut d = MeshData::default();
    loft(
        &mut d,
        &[
            // Tail nub.
            ring(Vec3::new(-0.58, 0.80, 0.0), 0.035, 0.04, RUMP, RUMP),
            // Rump (cream patch shows from behind).
            ring(Vec3::new(-0.50, 0.76, 0.0), 0.13, 0.16, COAT_DARK, RUMP),
            ring(Vec3::new(-0.34, 0.74, 0.0), 0.155, 0.19, COAT_DARK, BELLY),
            // Waist dips slightly.
            ring(Vec3::new(-0.08, 0.73, 0.0), 0.145, 0.175, COAT, BELLY),
            // Chest is the deepest station.
            ring(Vec3::new(0.20, 0.72, 0.0), 0.16, 0.21, COAT, BELLY),
            ring(Vec3::new(0.38, 0.74, 0.0), 0.135, 0.185, COAT, BELLY),
            // Shoulder → neck root.
            ring(Vec3::new(0.48, 0.80, 0.0), 0.10, 0.13, COAT, COAT),
        ],
        12,
    );
    d
}

/// Neck + head + ears (+ buck antlers). Pivot at the neck root on the shoulders —
/// the graze/alert dip rotates the whole assembly about Z. Faces +X.
pub fn deer_head(buck: bool) -> MeshData {
    let mut d = MeshData::default();
    // Neck: smooth S-curve up and forward.
    loft(
        &mut d,
        &[
            ring(Vec3::new(-0.02, -0.04, 0.0), 0.095, 0.12, COAT, COAT),
            ring(Vec3::new(0.05, 0.10, 0.0), 0.08, 0.10, COAT, BELLY),
            ring(Vec3::new(0.12, 0.24, 0.0), 0.065, 0.085, COAT, BELLY),
            ring(Vec3::new(0.18, 0.34, 0.0), 0.055, 0.07, COAT, BELLY),
        ],
        10,
    );
    // Skull + tapering muzzle, carried nearly level.
    loft(
        &mut d,
        &[
            ring(Vec3::new(0.13, 0.38, 0.0), 0.062, 0.062, COAT, COAT),
            ring(Vec3::new(0.22, 0.415, 0.0), 0.072, 0.072, COAT, BELLY),
            ring(Vec3::new(0.32, 0.405, 0.0), 0.052, 0.052, COAT, BELLY),
            ring(Vec3::new(0.40, 0.375, 0.0), 0.032, 0.030, MUZZLE, MUZZLE),
            ring(Vec3::new(0.435, 0.37, 0.0), 0.020, 0.018, MUZZLE, MUZZLE),
        ],
        10,
    );
    // Ears: small flattened cones angled out-back.
    for s in [-1.0f32, 1.0] {
        loft(
            &mut d,
            &[
                ring(Vec3::new(0.16, 0.43, 0.045 * s), 0.012, 0.03, COAT, BELLY),
                ring(Vec3::new(0.13, 0.52, 0.085 * s), 0.016, 0.042, COAT, BELLY),
                ring(Vec3::new(0.11, 0.585, 0.105 * s), 0.006, 0.014, COAT, COAT),
            ],
            8,
        );
    }
    if buck {
        // Antlers: thin lofted beams sweeping back then up, one brow tine each.
        const BONE: [f32; 3] = [0.55, 0.48, 0.38];
        for s in [-1.0f32, 1.0] {
            loft(
                &mut d,
                &[
                    ring(Vec3::new(0.18, 0.45, 0.030 * s), 0.022, 0.022, BONE, BONE),
                    ring(Vec3::new(0.15, 0.58, 0.060 * s), 0.018, 0.018, BONE, BONE),
                    ring(Vec3::new(0.19, 0.71, 0.085 * s), 0.014, 0.014, BONE, BONE),
                    ring(Vec3::new(0.28, 0.81, 0.070 * s), 0.010, 0.010, BONE, BONE),
                    ring(Vec3::new(0.34, 0.85, 0.055 * s), 0.005, 0.005, BONE, BONE),
                ],
                6,
            );
            loft(
                &mut d,
                &[
                    ring(Vec3::new(0.16, 0.58, 0.060 * s), 0.013, 0.013, BONE, BONE),
                    ring(Vec3::new(0.25, 0.66, 0.085 * s), 0.009, 0.009, BONE, BONE),
                    ring(Vec3::new(0.30, 0.69, 0.095 * s), 0.004, 0.004, BONE, BONE),
                ],
                6,
            );
        }
    }
    d
}

/// One leg, pivot at the hip, hanging down local -Y: tapered thigh, slim cannon, dark
/// hoof, a hint of a knee. Swings about Z.
pub fn deer_leg() -> MeshData {
    let mut d = MeshData::default();
    loft(
        &mut d,
        &[
            ring(Vec3::new(0.0, 0.02, 0.0), 0.062, 0.062, COAT, COAT),
            ring(Vec3::new(0.015, -0.16, 0.0), 0.045, 0.05, COAT, COAT),
            // Knee.
            ring(Vec3::new(0.025, -0.30, 0.0), 0.032, 0.034, COAT, COAT_DARK),
            ring(Vec3::new(0.01, -0.44, 0.0), 0.022, 0.024, COAT_DARK, COAT_DARK),
            // Cannon.
            ring(Vec3::new(0.005, -0.56, 0.0), 0.019, 0.02, COAT_DARK, COAT_DARK),
            // Hoof.
            ring(Vec3::new(0.01, -0.60, 0.0), 0.024, 0.022, MUZZLE, MUZZLE),
            ring(Vec3::new(0.015, -0.645, 0.0), 0.02, 0.016, MUZZLE, MUZZLE),
        ],
        8,
    );
    d
}

/// Hip anchors (x fore/aft, y hip height, z left/right) relative to the body pivot.
pub const DEER_HIPS: [[f32; 3]; 4] = [
    [0.34, 0.66, 0.10],
    [0.34, 0.66, -0.10],
    [-0.36, 0.68, 0.10],
    [-0.36, 0.68, -0.10],
];
/// Neck-root anchor on the shoulders.
pub const DEER_NECK: [f32; 3] = [0.48, 0.84, 0.0];

// ── Bird (thrush-like) ──────────────────────────────────────────────────────────────

const PLUMAGE: [f32; 3] = [0.16, 0.13, 0.11];
const BREAST: [f32; 3] = [0.52, 0.38, 0.24];

/// Body: teardrop loft nose-to-tail plus a fanned tail feather plate. Faces +X,
/// pivot at the body centre.
pub fn bird_body() -> MeshData {
    let mut d = MeshData::default();
    loft(
        &mut d,
        &[
            // Beak.
            ring(Vec3::new(0.20, 0.015, 0.0), 0.006, 0.006, [0.25, 0.2, 0.12], [0.25, 0.2, 0.12]),
            ring(Vec3::new(0.165, 0.02, 0.0), 0.018, 0.018, PLUMAGE, PLUMAGE),
            // Head.
            ring(Vec3::new(0.12, 0.03, 0.0), 0.042, 0.042, PLUMAGE, BREAST),
            // Chest is the widest.
            ring(Vec3::new(0.03, 0.0, 0.0), 0.062, 0.066, PLUMAGE, BREAST),
            ring(Vec3::new(-0.08, 0.0, 0.0), 0.05, 0.052, PLUMAGE, BREAST),
            // Taper to the tail root.
            ring(Vec3::new(-0.16, 0.015, 0.0), 0.024, 0.024, PLUMAGE, PLUMAGE),
        ],
        10,
    );
    // Tail: flat fan behind (local XY plane needs a tilt — build in XZ via loft is
    // wrong, so use the polygon helper rotated by hand: points in (x = aft, y = span)).
    let pts = [
        Vec2::new(-0.15, 0.02),
        Vec2::new(-0.30, 0.045),
        Vec2::new(-0.315, 0.0),
        Vec2::new(-0.30, -0.045),
        Vec2::new(-0.15, -0.02),
    ];
    let pc = [PLUMAGE[0], PLUMAGE[1], PLUMAGE[2], 1.0];
    let start = d.positions.len();
    flat_poly(&mut d, &pts, &[pc; 5]);
    // flat_poly builds in XY facing Z — swap Y↔Z so the fan lies flat like a tail.
    for p in &mut d.positions[start..] {
        let y = p[1];
        p[1] = p[2] * 0.3 + 0.02;
        p[2] = y;
    }
    for n in &mut d.normals[start..] {
        let y = n[1];
        n[1] = n[2];
        n[2] = y;
    }
    d
}

/// One wing: rounded flat silhouette, root at origin extending +X (mounted sideways by
/// the pose code), slight camber via the colour-matched underside.
pub fn bird_wing() -> MeshData {
    let mut d = MeshData::default();
    let pts = [
        Vec2::new(0.0, 0.055),
        Vec2::new(0.13, 0.05),
        Vec2::new(0.25, 0.028),
        Vec2::new(0.33, -0.005),
        Vec2::new(0.30, -0.03),
        Vec2::new(0.18, -0.045),
        Vec2::new(0.05, -0.05),
        Vec2::new(0.0, -0.04),
    ];
    let pc = [PLUMAGE[0], PLUMAGE[1], PLUMAGE[2], 1.0];
    let start = d.positions.len();
    flat_poly(&mut d, &pts, &[pc; 8]);
    // Lay the silhouette flat (XY→XZ): wings sweep in the horizontal plane.
    for p in &mut d.positions[start..] {
        let y = p[1];
        p[1] = p[2] * 0.5;
        p[2] = y;
    }
    for n in &mut d.normals[start..] {
        let y = n[1];
        n[1] = n[2];
        n[2] = y;
    }
    d
}

// ── Butterfly ───────────────────────────────────────────────────────────────────────

/// Slim lofted body with a hint of a head.
pub fn butterfly_body() -> MeshData {
    let mut d = MeshData::default();
    const B: [f32; 3] = [0.10, 0.08, 0.06];
    loft(
        &mut d,
        &[
            ring(Vec3::new(0.05, 0.0, 0.0), 0.008, 0.008, B, B),
            ring(Vec3::new(0.03, 0.0, 0.0), 0.012, 0.012, B, B),
            ring(Vec3::new(-0.01, 0.0, 0.0), 0.010, 0.010, B, B),
            ring(Vec3::new(-0.055, 0.0, 0.0), 0.005, 0.005, B, B),
        ],
        8,
    );
    d
}

/// One wing: true butterfly outline (fore + hind lobe), dark border, bright field.
/// Root at origin, extends +X in the wing's flat plane; the pose code hinges it.
pub fn butterfly_wing(variant: u32) -> MeshData {
    let (field, border) = match variant % 3 {
        0 => ([0.82, 0.42, 0.08, 1.0], [0.12, 0.08, 0.05, 1.0]), // monarch
        1 => ([0.88, 0.88, 0.92, 1.0], [0.25, 0.25, 0.28, 1.0]), // cabbage white
        _ => ([0.22, 0.34, 0.80, 1.0], [0.06, 0.08, 0.18, 1.0]), // blue morpho
    };
    // Outline: forewing lobe up-forward, hindwing lobe down-back (x = span, y = chord).
    let pts = [
        Vec2::new(0.005, 0.015),
        Vec2::new(0.06, 0.065),
        Vec2::new(0.125, 0.075),
        Vec2::new(0.155, 0.045),
        Vec2::new(0.12, 0.005),
        Vec2::new(0.14, -0.03),
        Vec2::new(0.10, -0.07),
        Vec2::new(0.045, -0.065),
        Vec2::new(0.005, -0.025),
    ];
    // Border points dark, inner span bright — the fan centroid lands bright.
    let cols = [
        field, field, border, border, field, border, border, field, field,
    ];
    let mut d = MeshData::default();
    let start = d.positions.len();
    flat_poly(&mut d, &pts, &cols);
    // Lay flat: wings live in the horizontal plane at rest.
    for p in &mut d.positions[start..] {
        let y = p[1];
        p[1] = p[2];
        p[2] = y;
    }
    for n in &mut d.normals[start..] {
        let y = n[1];
        n[1] = n[2];
        n[2] = y;
    }
    d
}
