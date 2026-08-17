//! Biome: the thin seam that lets one pipeline generate more than a temperate forest.
//!
//! A biome does NOT fork the pipeline. It picks four things and everything downstream —
//! erosion, lakes, trails, scatter, LOD, export — runs unchanged:
//!
//! 1. **Terrain profile** — which landform generator runs ([`Biome::terrain_style`]).
//! 2. **Ground layers** — the four splat texture sets, by name ([`Biome::ground_layers`]).
//! 3. **Species table** — which four tree species can grow ([`Biome::species`]).
//! 4. **Site rules** — how moisture/slope/elevation weight those species
//!    ([`Biome::species_weights_at`]) and how dense the vegetation gets overall.
//!
//! Four layers and four species are hard limits on purpose: the splat shader binds a
//! 4-layer texture array and the foliage atlas is one 2048² image split into four
//! quadrants. A biome re-points those four slots rather than growing them, so adding
//! biomes costs no VRAM and no shader permutations.

use crate::tree::Species;

/// Which world this map is. `Temperate` reproduces the original forest generator exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Biome {
    /// Eroded temperate uplands: rolling fBM + ridged massifs, conifer/broadleaf mix.
    #[default]
    Temperate,
    /// Stronghold-Crusader desert: broad flat basins, stepped sandstone mesas, one
    /// through-flowing river with a green ribbon of oasis along its banks.
    Arid,
}

/// How the base heightfield is laid out. Split from [`Biome`] so a future biome can reuse
/// an existing landform style with a different texture/species set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainStyle {
    /// Rolling fBM + ridged massifs gated by a mountain mask.
    Mountains,
    /// Low dune-rippled basins + hard-stepped mesas (sharp scarps, flat tops).
    Mesas,
}

impl Biome {
    pub const ALL: [Biome; 2] = [Biome::Temperate, Biome::Arid];

    /// Human-facing name (editor UI).
    pub fn label(self) -> &'static str {
        match self {
            Biome::Temperate => "Temperate",
            Biome::Arid => "Arid (Crusader)",
        }
    }

    /// Stable machine id, written into exported bundles. Never rename one of these — a
    /// consumer keys its texture set off this string.
    pub fn id(self) -> &'static str {
        match self {
            Biome::Temperate => "temperate",
            Biome::Arid => "arid",
        }
    }

    pub fn terrain_style(self) -> TerrainStyle {
        match self {
            Biome::Temperate => TerrainStyle::Mountains,
            Biome::Arid => TerrainStyle::Mesas,
        }
    }

    /// The four ground texture sets in shader-layer order. These are directory names under
    /// `assets/textures/ground/`, and the layer names written into an export's splat
    /// metadata — an exported map is self-describing about what its splat channels mean.
    ///
    /// Slot roles are consistent across biomes so the shader's weighting logic stays one
    /// branch: 0 = the dominant flat ground, 1 = the "rich"/low-lying variant, 2 = the
    /// steep triplanar rock, 3 = the bare trodden earth the trails punch through.
    pub fn ground_layers(self) -> [&'static str; 4] {
        match self {
            Biome::Temperate => ["grass", "forest_floor", "rock", "dirt"],
            Biome::Arid => ["sand", "sand_gravel", "sandstone", "dry_clay"],
        }
    }

    /// The four species this biome can scatter, in `ForestParams::species_weights` order.
    pub fn species(self) -> [Species; 4] {
        match self {
            Biome::Temperate => {
                [Species::Pine, Species::Spruce, Species::Broadleaf, Species::Birch]
            }
            Biome::Arid => {
                [Species::DatePalm, Species::Acacia, Species::Tamarisk, Species::DeadTree]
            }
        }
    }

    /// Site-modified species preference at one scatter site. `m` = moisture 0..1,
    /// `elev` = height as a fraction of the treeline 0..1. Returned in `species()` order;
    /// the caller multiplies by the user's `species_weights` and picks weighted-random.
    ///
    /// This is where a biome's *character* lives. Temperate spreads its four species over
    /// the whole map; arid concentrates every living tree on the river and leaves the dry
    /// flats to dead wood, which is exactly the Crusader read — a green thread through a
    /// dead landscape.
    pub fn species_weights_at(self, m: f32, elev: f32) -> [f32; 4] {
        match self {
            Biome::Temperate => [
                // pine — dry, sandy, ridge sites
                (1.2 - m) * (0.4 + elev),
                // spruce — moist and/or high sites
                (0.4 + 0.8 * m) * (0.5 + elev * 0.9),
                // broadleaf — mid-elevation, mesic slopes
                (0.5 + 0.7 * m) * (1.1 - elev).max(0.05),
                // birch — wet lowland pioneer
                (0.3 + m) * (1.0 - elev * 0.6),
            ],
            Biome::Arid => {
                // Palms are strictly riparian: they need the river. `m` past ~0.55 is
                // effectively "on the bank", and the cubic makes the falloff a hard edge
                // rather than a gradient — a palm 40 m into the sand reads as a mistake.
                let wet = ((m - 0.42) / 0.38).clamp(0.0, 1.0);
                [
                    // Tempered from 3.4: at that weight palms swamped the bank into a
                    // single-species carpet and the oasis stopped reading as a mixed
                    // stand. The hard `wet` cutoff already keeps them off the flats — the
                    // weight only has to make them the *most common* bank tree, not the
                    // only one.
                    wet * wet * wet * 1.8,
                    // Acacia: the classic lone desert tree — mid-dry, tolerates the flats.
                    (0.25 + 1.5 * m) * (1.0 - elev * 0.5),
                    // Tamarisk scrub-tree: wadi floors and the fringe just off the water.
                    (0.15 + 2.0 * m) * (1.0 - wet * 0.55),
                    // Dead wood: everywhere the others give up, and only there.
                    (0.55 - 0.5 * m).max(0.02) * (0.6 + 0.7 * elev),
                ]
            }
        }
    }

    /// Multiplier on overall tree stocking. A desert is not a thinner forest — it is a
    /// mostly empty map with a dense ribbon, so arid trades global density away and buys
    /// it back near water through [`Biome::species_weights_at`] and the moisture term.
    pub fn density_scale(self) -> f32 {
        match self {
            Biome::Temperate => 1.0,
            Biome::Arid => 0.30,
        }
    }

    /// Erosion budget multiplier. Hydraulic erosion is what rounds a landscape off; run it
    /// at full strength on the mesas and the scarps melt into hills, which is the single
    /// fastest way to lose the Crusader silhouette.
    pub fn erosion_scale(self) -> f32 {
        match self {
            Biome::Temperate => 1.0,
            Biome::Arid => 0.35,
        }
    }

    /// Does this biome carve a through-flowing river? (Temperate already gets its water
    /// from priority-flood mountain lakes.)
    pub fn has_river(self) -> bool {
        matches!(self, Biome::Arid)
    }

    /// Does this biome scatter isolated damp patches away from its water? See
    /// [`crate::maps::oasis_field`]. Temperate has no need — its moisture is already
    /// spread over the whole map.
    pub fn has_oases(self) -> bool {
        matches!(self, Biome::Arid)
    }

    /// Metres above the water level that still count as damp ground — see
    /// [`crate::maps::moisture_map`]. This single number is what makes a desert a desert:
    /// widen it and the oasis smears out into a green map with sand-coloured textures.
    pub fn moisture_band(self) -> f32 {
        match self {
            Biome::Temperate => 14.0,
            // 4.5 m of bank. At 2.5 the ribbon was so thin the oasis read as a couple of
            // stray palms rather than a stand; much past 6 and the moisture creeps back
            // out over the flats and the desert starts greening up again.
            Biome::Arid => 4.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_temperate_for_old_projects() {
        assert_eq!(Biome::default(), Biome::Temperate);
    }

    #[test]
    fn every_biome_fills_four_slots() {
        for b in Biome::ALL {
            assert_eq!(b.ground_layers().len(), 4);
            let sp = b.species();
            // Species must be distinct — two slots on one species wastes an atlas quadrant.
            for i in 0..4 {
                for j in i + 1..4 {
                    assert_ne!(sp[i], sp[j], "{b:?} repeats {:?}", sp[i]);
                }
            }
        }
    }

    #[test]
    fn weights_finite_and_nonnegative_everywhere() {
        for b in Biome::ALL {
            for mi in 0..=10 {
                for ei in 0..=10 {
                    let w = b.species_weights_at(mi as f32 / 10.0, ei as f32 / 10.0);
                    assert!(w.iter().all(|v| v.is_finite() && *v >= 0.0), "{b:?} {w:?}");
                    assert!(w.iter().sum::<f32>() > 0.0, "{b:?} has no viable species");
                }
            }
        }
    }

    #[test]
    fn arid_puts_palms_on_the_water_and_deadwood_in_the_sand() {
        let wet = Biome::Arid.species_weights_at(0.95, 0.1);
        let dry = Biome::Arid.species_weights_at(0.05, 0.1);
        // Palm dominates the bank...
        assert!(wet[0] > wet[3], "palms should beat dead wood on the river");
        // ...and is absent from the deep desert, where only dead wood stands.
        assert_eq!(dry[0], 0.0, "palm grew in dry sand");
        assert!(dry[3] > dry[1], "dead wood should dominate the dry flats");
    }
}
