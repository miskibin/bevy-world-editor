//! Procedural creature part meshes — deer, bird, butterfly. Same recipe as the props:
//! primitive parts with baked vertex COLOR, duplicated verts + flat normals for the
//! faceted look, base at the part's own pivot so the behaviour systems can rotate them
//! (legs swing, wings flap, necks dip) as child entities.

use bevy::prelude::*;

use crate::trees_mesh::MeshData;

/// Axis-aligned box: centre (cx, cy, cz), half-extents (hx, hy, hz), flat vertex colour.
/// 24 verts (4 per face) so normals stay flat without a duplicate pass.
fn push_box(d: &mut MeshData, c: Vec3, h: Vec3, col: [f32; 4]) {
    // (normal, two tangents) per face.
    const FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    for (n, u, v) in FACES {
        let n = Vec3::from_array(n);
        let u = Vec3::from_array(u);
        let v = Vec3::from_array(v);
        let base = d.positions.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            // corner = c + n∘h + su·(u∘h) + sv·(v∘h), component-wise products.
            let pos = c + Vec3::new(
                n.x * h.x + u.x * h.x * su + v.x * h.x * sv,
                n.y * h.y + u.y * h.y * su + v.y * h.y * sv,
                n.z * h.z + u.z * h.z * su + v.z * h.z * sv,
            );
            d.positions.push(pos.to_array());
            d.normals.push(n.to_array());
            d.uvs.push([0.0, 0.0]);
            d.colors.push(col);
        }
        d.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// A flat quad in the XZ... — actually in the local XY plane facing +Z and -Z (two-sided),
/// pivot at the -X edge midpoint so rotating about local Y hinges it like a wing.
fn push_wing_quad(d: &mut MeshData, len: f32, chord: f32, col: [f32; 4]) {
    // Slightly tapered: root chord full, tip chord 55%.
    let tip = chord * 0.55;
    let pts = [
        Vec3::new(0.0, 0.0, -chord * 0.5),
        Vec3::new(len, 0.0, -tip * 0.5),
        Vec3::new(len, 0.0, tip * 0.5),
        Vec3::new(0.0, 0.0, chord * 0.5),
    ];
    for (n, order) in [(Vec3::Y, [0u32, 1, 2, 0, 2, 3]), (-Vec3::Y, [0, 2, 1, 0, 3, 2])] {
        let base = d.positions.len() as u32;
        for p in pts {
            d.positions.push(p.to_array());
            d.normals.push(n.to_array());
            d.uvs.push([0.0, 0.0]);
            d.colors.push(col);
        }
        d.indices.extend(order.iter().map(|i| base + i));
    }
}

// ── Deer ────────────────────────────────────────────────────────────────────────────

const DEER_BROWN: [f32; 4] = [0.58, 0.42, 0.26, 1.0];
const DEER_DARK: [f32; 4] = [0.36, 0.25, 0.15, 1.0];
const DEER_CREAM: [f32; 4] = [0.92, 0.86, 0.72, 1.0];

/// Torso + rump patch + tail. Pivot at ground level under the chest; body floats at
/// leg height. Faces +X.
pub fn deer_body() -> MeshData {
    let mut d = MeshData::default();
    // Torso: ~1.1 long, shoulder height ~0.75.
    push_box(&mut d, Vec3::new(0.0, 0.84, 0.0), Vec3::new(0.50, 0.21, 0.16), DEER_BROWN);
    // Chest wedge (slightly deeper at the front).
    push_box(&mut d, Vec3::new(0.34, 0.74, 0.0), Vec3::new(0.20, 0.22, 0.17), DEER_BROWN);
    // Cream rump patch + stubby tail.
    push_box(&mut d, Vec3::new(-0.50, 0.86, 0.0), Vec3::new(0.06, 0.16, 0.15), DEER_CREAM);
    push_box(&mut d, Vec3::new(-0.58, 0.94, 0.0), Vec3::new(0.06, 0.07, 0.045), DEER_DARK);
    d
}

/// Neck + head + ears + (buck) antlers. Pivot at the neck root — the graze/alert dip
/// rotates this whole assembly about Z. Faces +X, neck rises forward-up.
pub fn deer_head(buck: bool) -> MeshData {
    let mut d = MeshData::default();
    // Neck: leans ~45° forward from the pivot (vertical reads as a llama).
    push_box(&mut d, Vec3::new(0.13, 0.13, 0.0), Vec3::new(0.13, 0.17, 0.095), DEER_BROWN);
    push_box(&mut d, Vec3::new(0.24, 0.26, 0.0), Vec3::new(0.11, 0.13, 0.085), DEER_BROWN);
    // Head + muzzle, carried ahead of the chest.
    push_box(&mut d, Vec3::new(0.37, 0.40, 0.0), Vec3::new(0.12, 0.10, 0.08), DEER_BROWN);
    push_box(&mut d, Vec3::new(0.53, 0.36, 0.0), Vec3::new(0.09, 0.055, 0.055), DEER_DARK);
    // Ears.
    for s in [-1.0f32, 1.0] {
        push_box(
            &mut d,
            Vec3::new(0.30, 0.52, 0.10 * s),
            Vec3::new(0.03, 0.075, 0.025),
            DEER_CREAM,
        );
    }
    if buck {
        // Simple three-tine antlers.
        for s in [-1.0f32, 1.0] {
            push_box(&mut d, Vec3::new(0.33, 0.60, 0.07 * s), Vec3::new(0.025, 0.10, 0.025), DEER_CREAM);
            push_box(&mut d, Vec3::new(0.37, 0.68, 0.10 * s), Vec3::new(0.08, 0.02, 0.02), DEER_CREAM);
            push_box(&mut d, Vec3::new(0.29, 0.70, 0.09 * s), Vec3::new(0.02, 0.06, 0.02), DEER_CREAM);
        }
    }
    d
}

/// One leg, pivot at the hip — swings about Z. Extends DOWN from the pivot.
pub fn deer_leg() -> MeshData {
    let mut d = MeshData::default();
    push_box(&mut d, Vec3::new(0.0, -0.20, 0.0), Vec3::new(0.055, 0.20, 0.05), DEER_BROWN);
    push_box(&mut d, Vec3::new(0.0, -0.52, 0.0), Vec3::new(0.04, 0.14, 0.038), DEER_DARK);
    d
}

/// Hip anchors (x fore/aft, z left/right) relative to the body pivot; legs hang from
/// y = hip height.
pub const DEER_HIPS: [[f32; 3]; 4] = [
    [0.38, 0.62, 0.14],
    [0.38, 0.62, -0.14],
    [-0.38, 0.62, 0.14],
    [-0.38, 0.62, -0.14],
];
/// Neck-root anchor on the body.
pub const DEER_NECK: [f32; 3] = [0.46, 0.94, 0.0];

// ── Bird ────────────────────────────────────────────────────────────────────────────

const BIRD_SLATE: [f32; 4] = [0.13, 0.13, 0.16, 1.0];
const BIRD_LIGHT: [f32; 4] = [0.35, 0.33, 0.30, 1.0];

/// Body + tail, faces +X. Pivot at the body centre.
pub fn bird_body() -> MeshData {
    let mut d = MeshData::default();
    push_box(&mut d, Vec3::ZERO, Vec3::new(0.16, 0.07, 0.06), BIRD_SLATE);
    push_box(&mut d, Vec3::new(0.16, 0.02, 0.0), Vec3::new(0.05, 0.045, 0.04), BIRD_SLATE);
    push_box(&mut d, Vec3::new(0.23, 0.02, 0.0), Vec3::new(0.03, 0.015, 0.015), BIRD_LIGHT);
    // Fanned tail.
    push_box(&mut d, Vec3::new(-0.20, 0.01, 0.0), Vec3::new(0.08, 0.012, 0.05), BIRD_SLATE);
    d
}

/// One wing, pivot at the root, extends +Z (left wing; right is rotated PI about X…
/// — actually spawn it rotated PI about the body X axis so the flap sign mirrors).
pub fn bird_wing() -> MeshData {
    let mut d = MeshData::default();
    // Reuse the tapered quad: swing extends along +X in wing space, so rotate later.
    push_wing_quad(&mut d, 0.34, 0.16, BIRD_SLATE);
    d
}

// ── Butterfly ───────────────────────────────────────────────────────────────────────

/// Tiny dark body.
pub fn butterfly_body() -> MeshData {
    let mut d = MeshData::default();
    push_box(&mut d, Vec3::ZERO, Vec3::new(0.055, 0.016, 0.016), [0.10, 0.08, 0.06, 1.0]);
    d
}

/// One wing; palette keyed by `variant`.
pub fn butterfly_wing(variant: u32) -> MeshData {
    let col = match variant % 3 {
        0 => [0.85, 0.45, 0.10, 1.0], // monarch orange
        1 => [0.80, 0.80, 0.90, 1.0], // cabbage white
        _ => [0.25, 0.35, 0.85, 1.0], // blue morpho-ish
    };
    let mut d = MeshData::default();
    push_wing_quad(&mut d, 0.16, 0.14, col);
    d
}
