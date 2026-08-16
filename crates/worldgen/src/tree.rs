//! Parametric tree skeleton generator (simplified Weber–Penn). Pure geometry — the Bevy
//! side sweeps tubes along segments and pastes leaf cards at anchors. Deterministic per
//! (species, seed).
//!
//! Species read differently through STRUCTURE, not just texture:
//! - Pine: bare straight bole, irregular upswept crown near the top.
//! - Spruce: full cone — whorled branches from near the ground, longest at the base,
//!   drooping with upturned tips.
//! - Broadleaf (beech/oak): short bole splitting into scaffold limbs, rounded canopy.
//! - Birch: slender, slightly leaning bole, thin ascending branches with drooping tips.
//! - DatePalm: bare curving column, no branches at all, one crown of arching fronds.
//! - Acacia: short bole forking low into limbs that flatten into a wide umbrella.
//! - Tamarisk: multi-stem desert scrub-tree, several leaning stems from one root.
//! - DeadTree: bare forked skeleton, no foliage whatsoever.

use crate::rng::Rng;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Species {
    Pine,
    Spruce,
    Broadleaf,
    Birch,
    DatePalm,
    Acacia,
    Tamarisk,
    DeadTree,
}

/// Every species the generator knows. NB the *renderer* never iterates this — it iterates
/// `Biome::species()`, because the foliage atlas only has four quadrants and a map is one
/// biome. This exists for tests and tooling that want full coverage.
pub const ALL_SPECIES: [Species; 8] = [
    Species::Pine,
    Species::Spruce,
    Species::Broadleaf,
    Species::Birch,
    Species::DatePalm,
    Species::Acacia,
    Species::Tamarisk,
    Species::DeadTree,
];

impl Species {
    /// Does this species carry foliage cards? `DeadTree` is the one that does not, and
    /// every consumer that assumes "a tree has leaves" has to ask first.
    pub fn has_foliage(self) -> bool {
        !matches!(self, Species::DeadTree)
    }
}

#[derive(Clone, Copy)]
pub struct Segment {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub ra: f32,
    pub rb: f32,
    /// 0 = trunk, 1 = scaffold/whorl branch, 2 = twig.
    pub level: u8,
}

#[derive(Clone, Copy)]
pub struct LeafAnchor {
    pub pos: [f32; 3],
    /// Outward normal-ish direction for the card.
    pub dir: [f32; 3],
    pub size: f32,
}

pub struct TreeSkeleton {
    pub species: Species,
    pub segments: Vec<Segment>,
    pub leaves: Vec<LeafAnchor>,
    pub height: f32,
    pub canopy_center: [f32; 3],
    pub canopy_radius: f32,
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale3(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn norm3(a: [f32; 3]) -> [f32; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-6);
    scale3(a, 1.0 / l)
}

/// A direction at `angle` from `axis`, rotated `azimuth` around it.
fn cone_dir(axis: [f32; 3], angle: f32, azimuth: f32) -> [f32; 3] {
    let axis = norm3(axis);
    // Build any orthonormal frame around axis.
    let helper = if axis[1].abs() < 0.9 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
    let u = norm3([
        axis[1] * helper[2] - axis[2] * helper[1],
        axis[2] * helper[0] - axis[0] * helper[2],
        axis[0] * helper[1] - axis[1] * helper[0],
    ]);
    let v = [
        axis[1] * u[2] - axis[2] * u[1],
        axis[2] * u[0] - axis[0] * u[2],
        axis[0] * u[1] - axis[1] * u[0],
    ];
    let (sa, ca) = angle.sin_cos();
    let (sz, cz) = azimuth.sin_cos();
    norm3(add3(
        scale3(axis, ca),
        add3(scale3(u, sa * cz), scale3(v, sa * sz)),
    ))
}

/// Walk an axis in `nseg` segments, applying per-segment random gnarl and a constant
/// tropism pull (up or down). Pushes segments, returns sample points (pos, dir, t).
#[allow(clippy::too_many_arguments)]
fn grow_axis(
    out: &mut Vec<Segment>,
    rng: &mut Rng,
    start: [f32; 3],
    dir: [f32; 3],
    len: f32,
    r0: f32,
    r1: f32,
    level: u8,
    nseg: usize,
    gnarl: f32,
    tropism: f32, // +up, -down, applied per segment
) -> Vec<([f32; 3], [f32; 3], f32)> {
    let mut pos = start;
    let mut d = norm3(dir);
    let step = len / nseg as f32;
    let mut samples = Vec::with_capacity(nseg + 1);
    for i in 0..nseg {
        let t0 = i as f32 / nseg as f32;
        let t1 = (i + 1) as f32 / nseg as f32;
        samples.push((pos, d, t0));
        // EZ-Tree trick: gnarl amplitude scales ~1/sqrt(radius), so thin twigs wander
        // hard while the trunk stays stately — a big chunk of "real tree" silhouette.
        let r_cur = (r0 + (r1 - r0) * t0).max(1e-3);
        let g = gnarl * (1.0 / r_cur.sqrt()).clamp(1.0, 3.0);
        d = norm3(add3(
            d,
            [rng.signed() * g, tropism + rng.signed() * g * 0.4, rng.signed() * g],
        ));
        let next = add3(pos, scale3(d, step));
        out.push(Segment {
            a: pos,
            b: next,
            ra: r0 + (r1 - r0) * t0,
            rb: r0 + (r1 - r0) * t1,
            level,
        });
        pos = next;
    }
    samples.push((pos, d, 1.0));
    samples
}

pub fn grow(species: Species, seed: u32) -> TreeSkeleton {
    let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9).wrapping_add(species as u32 * 7919));
    let mut segments = Vec::new();
    let mut leaves = Vec::new();

    match species {
        Species::Pine => {
            let h = rng.range(17.0, 25.0);
            let r = h * 0.021;
            let lean = [rng.signed() * 0.03, 1.0, rng.signed() * 0.03];
            let trunk =
                grow_axis(&mut segments, &mut rng, [0.0; 3], lean, h, r, r * 0.06, 0, 10, 0.05, 0.012);
            // Irregular crown: branches only on the top ~40% of the bole.
            let crown_start = rng.range(0.55, 0.68);
            let n_branches = rng.next_u32() % 5 + 9;
            for _ in 0..n_branches {
                let t = rng.range(crown_start, 0.97);
                let (bp, bd, _) = trunk[(t * 10.0) as usize];
                // High branches shorter + steeper up; low crown branches flatter.
                let up = (t - crown_start) / (1.0 - crown_start);
                let angle = (1.35 - up * 0.75) + rng.signed() * 0.15;
                let blen = h * (0.16 + (1.0 - up) * 0.10) * rng.range(0.8, 1.2);
                let bdir = cone_dir(bd, angle, rng.range(0.0, std::f32::consts::TAU));
                let br = r * 0.22 * (0.5 + 0.5 * (1.0 - up));
                let tips = grow_axis(
                    &mut segments, &mut rng, bp, bdir, blen, br, br * 0.15, 1, 4, 0.10, 0.020,
                );
                // Needle sprays on the outer half of each crown branch.
                for &(p, d, t) in &tips {
                    if t > 0.4 {
                        leaves.push(LeafAnchor {
                            pos: p,
                            dir: norm3(add3(d, [0.0, 0.35, 0.0])),
                            size: h * rng.range(0.09, 0.15),
                        });
                    }
                }
            }
        }
        Species::Spruce => {
            let h = rng.range(16.0, 24.0);
            let r = h * 0.020;
            let trunk = grow_axis(
                &mut segments, &mut rng, [0.0; 3], [0.0, 1.0, 0.0], h, r, r * 0.03, 0, 12, 0.025,
                0.015,
            );
            // Whorls from near the ground to the tip; branch length tapers to the cone.
            let n_whorls = 11 + (rng.next_u32() % 3) as usize;
            for w in 0..n_whorls {
                let t = 0.10 + 0.86 * w as f32 / (n_whorls - 1) as f32;
                let (bp, bd, _) = trunk[(t * 12.0) as usize];
                let cone = 1.0 - t; // 1 at base, 0 at tip
                let blen = h * 0.185 * cone.max(0.12) * rng.range(0.85, 1.15);
                let per = 5 + (rng.next_u32() % 3);
                for _ in 0..per {
                    // Droop: branch leaves the trunk near-horizontal, sags, tip turns up
                    // — approximated by a downward tropism + a slightly upward launch.
                    let bdir =
                        cone_dir(bd, 1.42 + rng.signed() * 0.08, rng.range(0.0, std::f32::consts::TAU));
                    let br = r * 0.16 * (0.4 + 0.6 * cone);
                    let tips = grow_axis(
                        &mut segments, &mut rng, bp, bdir, blen, br, br * 0.12, 1, 3, 0.06, -0.045,
                    );
                    for &(p, d, tt) in &tips {
                        if tt > 0.4 {
                            leaves.push(LeafAnchor {
                                pos: p,
                                dir: norm3(add3(d, [0.0, -0.25, 0.0])),
                                size: h * rng.range(0.06, 0.11) * (0.5 + 0.6 * cone),
                            });
                        }
                    }
                }
            }
        }
        Species::Broadleaf => {
            // Oak/beech habit: ONE continuous trunk with scaffold limbs distributed
            // along its upper 55% (the old bole-splits-into-candelabra read as fake),
            // lower limbs wide and long, upper steep and short → rounded crown.
            let h = rng.range(13.0, 19.0);
            let r = h * 0.028;
            let lean = [rng.signed() * 0.05, 1.0, rng.signed() * 0.05];
            let trunk = grow_axis(
                &mut segments, &mut rng, [0.0; 3], lean,
                h * 0.72, r, r * 0.10, 0, 8, 0.045, 0.012,
            );
            let n_scaffold = 6 + (rng.next_u32() % 4);
            for s in 0..n_scaffold {
                let t = 0.45 + 0.53 * (s as f32 + rng.f32() * 0.8) / n_scaffold as f32;
                let (bp, bd, _) = trunk[((t * 8.0) as usize).min(8)];
                let k = (t - 0.45) / 0.53; // 0 low … 1 top
                // Radial slot + jitter (decorrelated from height, EZ permutation idea).
                let az = s as f32 * 2.39996 + rng.signed() * 0.5; // golden angle
                let sdir = cone_dir(bd, 1.15 - 0.55 * k + rng.signed() * 0.12, az);
                let slen = h * 0.42 * (1.0 - 0.45 * k) * rng.range(0.85, 1.15);
                let sr = r * 0.42 * (1.0 - 0.35 * k);
                let limb = grow_axis(
                    &mut segments, &mut rng, bp, sdir, slen, sr, sr * 0.08, 1, 5, 0.10, 0.030,
                );
                for &(p, d, tt) in &limb {
                    if tt > 0.55 {
                        leaves.push(LeafAnchor {
                            pos: p,
                            dir: d,
                            size: h * rng.range(0.12, 0.22),
                        });
                    }
                }
                let n_twigs = 2 + (rng.next_u32() % 3);
                for _ in 0..n_twigs {
                    let tt = rng.range(0.45, 0.95);
                    let (tp, td, _) = limb[((tt * 5.0) as usize).min(5)];
                    let tdir2 =
                        cone_dir(td, rng.range(0.45, 0.95), rng.range(0.0, std::f32::consts::TAU));
                    let tlen = slen * rng.range(0.30, 0.50);
                    let tips = grow_axis(
                        &mut segments, &mut rng, tp, tdir2, tlen, sr * 0.25, sr * 0.03, 2, 3,
                        0.14, 0.035,
                    );
                    for &(p, d, t3) in &tips {
                        if t3 > 0.35 {
                            leaves.push(LeafAnchor {
                                pos: p,
                                dir: d,
                                size: h * rng.range(0.11, 0.21),
                            });
                        }
                    }
                }
            }
        }
        Species::Birch => {
            let h = rng.range(14.0, 20.0);
            let r = h * 0.016;
            let lean = [rng.signed() * 0.06, 1.0, rng.signed() * 0.06];
            let trunk = grow_axis(
                &mut segments, &mut rng, [0.0; 3], lean,
                h, r, r * 0.05, 0, 10, 0.045, 0.010,
            );
            let start = rng.range(0.35, 0.45);
            let n_branches = 10 + (rng.next_u32() % 5);
            for _ in 0..n_branches {
                let t = rng.range(start, 0.96);
                let (bp, bd, _) = trunk[(t * 10.0) as usize];
                let up = (t - start) / (1.0 - start);
                // Ascending launch, then strong droop — the birch "weeping tip" look.
                let bdir = cone_dir(bd, 0.85 - up * 0.35 + rng.signed() * 0.1,
                    rng.range(0.0, std::f32::consts::TAU));
                let blen = h * (0.20 - up * 0.08) * rng.range(0.8, 1.2);
                let br = r * 0.30 * (0.6 + 0.4 * (1.0 - up));
                let tips = grow_axis(
                    &mut segments, &mut rng, bp, bdir, blen, br, br * 0.10, 1, 4, 0.10, -0.060,
                );
                for &(p, d, tt) in &tips {
                    if tt > 0.55 {
                        leaves.push(LeafAnchor {
                            pos: p,
                            dir: d,
                            size: h * rng.range(0.08, 0.14),
                        });
                    }
                }
            }
        }
        Species::DatePalm => {
            // A palm is not a tree with few branches — it has NONE. One column, one crown.
            // The silhouette is carried entirely by frond arch, so the fronds get real
            // segments (they are visible geometry at LOD0) rather than being pure anchors.
            // Trunk height only — the crown adds a few metres on top, and `height` below
            // measures the whole plant.
            let h = rng.range(9.5, 14.5);
            let r = h * 0.017;
            // Palms lean into a curve rather than gnarling: one persistent bias direction
            // plus a strong sideways tropism bends the whole column the same way.
            let bend = rng.range(0.0, std::f32::consts::TAU);
            let (bs, bc) = bend.sin_cos();
            let lean = [bc * 0.10, 1.0, bs * 0.10];
            // Near-zero taper: a date palm's trunk is a column, not a cone.
            let trunk =
                grow_axis(&mut segments, &mut rng, [0.0; 3], lean, h, r, r * 0.72, 0, 9, 0.018, 0.0);
            let (top, _, _) = trunk[9];
            let n_fronds = 13 + (rng.next_u32() % 7);
            for f in 0..n_fronds {
                // Golden-angle azimuth so the crown is even without looking stamped.
                let az = f as f32 * 2.39996 + rng.signed() * 0.35;
                // Older fronds hang near-horizontal, young ones point up out of the spear.
                let age = f as f32 / n_fronds as f32;
                // Even the youngest frond leaves the spear at ~30° off vertical; letting
                // any of them run straight up turns the crown into a fountain and adds
                // metres of fake height.
                let angle = 0.55 + age * 1.00 + rng.signed() * 0.12;
                // Frond length sets the crown DIAMETER (≈2×), and a date palm's crown is
                // 6–9 m, not the 12–14 m the longer fronds were producing. Oversized
                // crowns are especially loud from an RTS camera: the canopy is most of
                // what you see, so a palm reading twice its size makes the whole map read
                // half its size.
                let flen = h * rng.range(0.26, 0.34);
                let fdir = cone_dir([0.0, 1.0, 0.0], angle, az);
                let fr = r * 0.16;
                // Negative tropism = the frond arches over under its own weight.
                let rib = grow_axis(
                    &mut segments, &mut rng, top, fdir, flen, fr, fr * 0.25, 1, 4, 0.03, -0.10,
                );
                // EXACTLY ONE card per frond, at its base, sized to the whole frond.
                //
                // The card texture is a complete frond — rachis, leaflets and all — so
                // emitting one per rib sample (the pattern every other species uses, where
                // a card is a small twig sprig) stacks five overlapping full fronds along
                // each rib. The crown then reads as a flat rosette of ferns with no trunk
                // visible, which is exactly what it looked like before this comment.
                let (bp, bd, _) = rib[0];
                leaves.push(LeafAnchor { pos: bp, dir: bd, size: flen });
            }
        }
        Species::Acacia => {
            // The umbrella read comes from limbs that START steep and are then flattened
            // by a hard negative tropism — a limb launched flat just looks like a dead
            // horizontal stick, while one that bends over as it grows reads as canopy.
            let h = rng.range(9.0, 13.0);
            let r = h * 0.034;
            let lean = [rng.signed() * 0.07, 1.0, rng.signed() * 0.07];
            let fork = rng.range(0.30, 0.42); // fraction of height where the bole forks
            let trunk = grow_axis(
                &mut segments, &mut rng, [0.0; 3], lean, h * fork, r, r * 0.62, 0, 4, 0.05, 0.0,
            );
            let (fp, fd, _) = trunk[4];
            let n_limbs = 3 + (rng.next_u32() % 3);
            for l in 0..n_limbs {
                let az = l as f32 * 2.39996 + rng.signed() * 0.4;
                let ldir = cone_dir(fd, rng.range(0.55, 0.85), az);
                let llen = h * rng.range(0.62, 0.82);
                let lr = r * 0.52;
                let limb = grow_axis(
                    &mut segments, &mut rng, fp, ldir, llen, lr, lr * 0.12, 1, 6, 0.07, -0.075,
                );
                // Foliage only on the OUTER, upper part of each limb: the flat top plate.
                for &(p, d, t) in &limb {
                    if t > 0.55 {
                        leaves.push(LeafAnchor {
                            pos: p,
                            // Cards lie back toward horizontal — an umbrella crown is a
                            // plate seen from above, which is exactly the RTS view.
                            dir: norm3(add3(scale3(d, 0.6), [0.0, 0.5, 0.0])),
                            size: h * rng.range(0.16, 0.26),
                        });
                    }
                }
                // A couple of twigs per limb thicken the plate's edge.
                for _ in 0..2 + rng.next_u32() % 2 {
                    let tt = rng.range(0.5, 0.9);
                    let (tp, td, _) = limb[((tt * 6.0) as usize).min(6)];
                    let tdir = cone_dir(td, rng.range(0.3, 0.7), rng.range(0.0, std::f32::consts::TAU));
                    let tlen = llen * rng.range(0.20, 0.34);
                    let tips = grow_axis(
                        &mut segments, &mut rng, tp, tdir, tlen,
                        lr * 0.22, lr * 0.04, 2, 3, 0.10, -0.05,
                    );
                    for &(p, d, t3) in &tips {
                        if t3 > 0.3 {
                            leaves.push(LeafAnchor {
                                pos: p,
                                dir: norm3(add3(scale3(d, 0.6), [0.0, 0.45, 0.0])),
                                size: h * rng.range(0.13, 0.21),
                            });
                        }
                    }
                }
            }
        }
        Species::Tamarisk => {
            // Wadi scrub-tree: no single trunk — several stems leave one root ball and
            // fan outward, carrying fine drooping foliage most of their length. Short by
            // design (~5 m); it is the filler between palms and bare sand.
            let h = rng.range(3.8, 6.2);
            let n_stems = 3 + (rng.next_u32() % 3);
            let fan = rng.range(0.0, std::f32::consts::TAU);
            for s in 0..n_stems {
                let az = fan + s as f32 * (std::f32::consts::TAU / n_stems as f32)
                    + rng.signed() * 0.4;
                let sdir = cone_dir([0.0, 1.0, 0.0], rng.range(0.20, 0.45), az);
                let slen = h * rng.range(0.82, 1.05);
                let sr = h * 0.030;
                let stem = grow_axis(
                    &mut segments, &mut rng, [0.0; 3], sdir, slen, sr, sr * 0.14, 0, 5, 0.09, -0.02,
                );
                for &(p, d, t) in &stem {
                    if t > 0.30 {
                        leaves.push(LeafAnchor {
                            pos: p,
                            dir: norm3(add3(d, [0.0, -0.30, 0.0])),
                            size: h * rng.range(0.26, 0.40),
                        });
                    }
                }
                // One side shoot per stem so the mass reads bushy, not broom-like.
                let (bp, bd, _) = stem[2];
                let bdir = cone_dir(bd, rng.range(0.5, 0.9), rng.range(0.0, std::f32::consts::TAU));
                let tips = grow_axis(
                    &mut segments, &mut rng, bp, bdir, slen * 0.55, sr * 0.4, sr * 0.06, 1, 3,
                    0.13, -0.03,
                );
                for &(p, d, t) in &tips {
                    if t > 0.25 {
                        leaves.push(LeafAnchor {
                            pos: p,
                            dir: norm3(add3(d, [0.0, -0.25, 0.0])),
                            size: h * rng.range(0.22, 0.34),
                        });
                    }
                }
            }
        }
        Species::DeadTree => {
            // Zero foliage — the whole silhouette is the fork pattern, so it gets more
            // branch levels than a leafed tree of the same size would bother with.
            let h = rng.range(5.5, 9.5);
            let r = h * 0.030;
            let lean = [rng.signed() * 0.16, 1.0, rng.signed() * 0.16];
            // High gnarl: a dead trunk is crooked, and there are no leaves to hide it.
            let bole = h * rng.range(0.62, 0.85);
            let trunk = grow_axis(
                &mut segments, &mut rng, [0.0; 3], lean, bole, r, r * 0.14, 0, 7, 0.10, 0.0,
            );
            let n_limbs = 3 + (rng.next_u32() % 4);
            for l in 0..n_limbs {
                let t = rng.range(0.30, 0.95);
                let (bp, bd, _) = trunk[((t * 7.0) as usize).min(7)];
                let az = l as f32 * 2.39996 + rng.signed() * 0.6;
                let ldir = cone_dir(bd, rng.range(0.6, 1.25), az);
                let llen = h * rng.range(0.22, 0.44);
                let lr = r * rng.range(0.24, 0.42);
                let limb = grow_axis(
                    &mut segments, &mut rng, bp, ldir, llen, lr, lr * 0.10, 1, 3, 0.20, -0.02,
                );
                // Snapped-off forks: half the limbs split once more into bare twigs.
                if rng.chance(0.55) {
                    let (tp, td, _) = limb[2];
                    let tdir =
                        cone_dir(td, rng.range(0.5, 1.1), rng.range(0.0, std::f32::consts::TAU));
                    let tlen = llen * rng.range(0.35, 0.6);
                    grow_axis(
                        &mut segments, &mut rng, tp, tdir, tlen,
                        lr * 0.30, lr * 0.05, 2, 2, 0.26, -0.03,
                    );
                }
            }
        }
    }

    // NB: no anchor densification and no dir-bending here — each anchor now carries a
    // WHOLE photographic sprig (base-pivot card growing along the branch tangent), and
    // "soft volume" lighting comes from per-vertex rounded normals in the mesh builder.
    // Canopy bounds drive the LOD2 impostor and the renderer's per-tree cull radius. They
    // come from the leaf cloud where there is one, and from the branch tips otherwise —
    // a leafless species (DeadTree) still needs a real radius or it culls at zero distance.
    let (mut cx, mut cy, mut cz) = (0.0f32, 0.0f32, 0.0f32);
    let n = if leaves.is_empty() {
        for s in &segments {
            cx += s.b[0];
            cy += s.b[1];
            cz += s.b[2];
        }
        segments.len().max(1) as f32
    } else {
        for l in &leaves {
            cx += l.pos[0];
            cy += l.pos[1];
            cz += l.pos[2];
        }
        leaves.len() as f32
    };
    let center = [cx / n, cy / n, cz / n];
    // Radius spans BOTH the leaf anchors and the branch tips. Anchors alone are not
    // enough: a palm's fronds all pivot from one point at the top of the trunk, so its
    // leaf cloud is a single position and the radius would collapse to nothing — taking
    // the LOD2 impostor and the renderer's cull sphere with it.
    let mut radius = 0.0f32;
    let mut extend = |p: [f32; 3]| {
        let out = [p[0] - center[0], p[1] - center[1] + 0.5, p[2] - center[2]];
        radius = radius.max((out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt());
    };
    for l in &leaves {
        extend(l.pos);
    }
    for s in &segments {
        extend(s.b);
    }

    let height = segments
        .iter()
        .map(|s| s.b[1])
        .fold(0.0f32, f32::max);

    TreeSkeleton { species, segments, leaves, height, canopy_center: center, canopy_radius: radius }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        for sp in ALL_SPECIES {
            let a = grow(sp, 5);
            let b = grow(sp, 5);
            assert_eq!(a.segments.len(), b.segments.len());
            assert_eq!(a.leaves.len(), b.leaves.len());
            for (x, y) in a.segments.iter().zip(&b.segments) {
                assert_eq!(x.a, y.a);
                assert_eq!(x.b, y.b);
            }
        }
    }

    #[test]
    fn radii_taper_and_finite() {
        for sp in ALL_SPECIES {
            for seed in 0..8 {
                let t = grow(sp, seed);
                for s in &t.segments {
                    assert!(s.ra.is_finite() && s.rb.is_finite());
                    assert!(s.ra > 0.0 && s.rb > 0.0, "{sp:?} radius <= 0");
                    assert!(s.rb <= s.ra + 1e-4, "{sp:?} radius grows down a segment");
                    for v in s.a.iter().chain(s.b.iter()) {
                        assert!(v.is_finite());
                    }
                }
            }
        }
    }

    /// Plausible standing height per species, in metres. Encoded per-species because the
    /// arid biome's habits are genuinely small — a tamarisk is scrub, and asserting one
    /// forest-sized range across every species would only be satisfiable by drawing a
    /// wrong-looking tree.
    fn height_range(sp: Species) -> (f32, f32) {
        match sp {
            Species::Pine | Species::Spruce | Species::Broadleaf | Species::Birch => (10.0, 30.0),
            Species::DatePalm => (10.0, 22.0),
            Species::Acacia => (7.0, 15.0),
            Species::Tamarisk => (2.5, 8.0),
            Species::DeadTree => (3.0, 11.0),
        }
    }

    #[test]
    fn species_shapes_sane() {
        for sp in ALL_SPECIES {
            for seed in 0..8 {
                let t = grow(sp, seed);
                let (lo, hi) = height_range(sp);
                assert!(t.height > lo && t.height < hi, "{sp:?} height {}", t.height);
                if sp.has_foliage() {
                    // Sprig-card era: each anchor is a WHOLE photographic twig, so healthy
                    // counts are tens, not hundreds (EZ-Tree ships full oaks at ~200 sprigs).
                    // The palm is lower still — one card IS one whole frond, so a full
                    // crown is a dozen-odd anchors and more would mean double-stacking.
                    let min = if sp == Species::DatePalm { 10 } else { 18 };
                    assert!(t.leaves.len() > min, "{sp:?} only {} leaves", t.leaves.len());
                    assert!(t.leaves.len() < 800, "{sp:?} leaf explosion");
                } else {
                    assert!(t.leaves.is_empty(), "{sp:?} should be bare");
                }
                assert!(t.segments.len() > 10 && t.segments.len() < 3000);
                assert!(t.canopy_radius > 0.5 && t.canopy_radius.is_finite());
            }
        }
    }

    #[test]
    fn palm_has_no_branch_forks_below_the_crown() {
        // The palm's read depends on a clean column: nothing may join the TRUNK partway up
        // it. Tested on where a branch segment attaches to the axis, not on its height —
        // a frond arches well below the crown as it droops, and that is correct.
        for seed in 0..6 {
            let t = grow(Species::DatePalm, seed);
            let bole_top = t
                .segments
                .iter()
                .filter(|s| s.level == 0)
                .map(|s| s.b[1])
                .fold(0.0f32, f32::max);
            for s in t.segments.iter().filter(|s| s.level > 0) {
                let on_axis = (s.a[0] * s.a[0] + s.a[2] * s.a[2]).sqrt() < 0.6;
                if on_axis {
                    assert!(
                        s.a[1] > bole_top - 0.75,
                        "palm branched off the trunk at {} of {bole_top}",
                        s.a[1]
                    );
                }
            }
        }
    }

    #[test]
    fn acacia_crown_is_flatter_than_it_is_tall() {
        // The umbrella test: crown width should clearly exceed crown depth.
        for seed in 0..6 {
            let t = grow(Species::Acacia, seed);
            let (mut wide, mut lo, mut hi) = (0.0f32, f32::MAX, f32::MIN);
            for l in &t.leaves {
                wide = wide.max((l.pos[0] * l.pos[0] + l.pos[2] * l.pos[2]).sqrt());
                lo = lo.min(l.pos[1]);
                hi = hi.max(l.pos[1]);
            }
            assert!(wide * 2.0 > (hi - lo) * 1.5, "acacia crown not umbrella-flat");
        }
    }

    #[test]
    fn spruce_cone_wider_at_base() {
        // Spruce: mean branch-tip distance from axis should be larger low than high.
        let t = grow(Species::Spruce, 3);
        let h = t.height;
        let (mut low, mut nlow, mut high, mut nhigh) = (0.0f32, 0, 0.0f32, 0);
        for l in &t.leaves {
            let d = (l.pos[0] * l.pos[0] + l.pos[2] * l.pos[2]).sqrt();
            if l.pos[1] < h * 0.4 {
                low += d;
                nlow += 1;
            } else if l.pos[1] > h * 0.6 {
                high += d;
                nhigh += 1;
            }
        }
        assert!(nlow > 0 && nhigh > 0);
        assert!(low / nlow as f32 > high / nhigh as f32, "spruce isn't conical");
    }
}
