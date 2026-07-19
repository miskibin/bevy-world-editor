# Forest Generator — Phase 1 Design (Bevy World Editor)

Date: 2026-07-19 · Status: approved (approach A picked by user; sections written per the
brainstorm answers, user asked to fast-forward: full spec + immediate implementation).

## What this is

Phase 1 of a new project: a **realistic 3D world editor for Bevy**. Phase 1 is a
**generator + fly-cam**, not yet an interactive editor: the app procedurally generates a
realistic ~2×2 km forested terrain, the user flies a free camera through it, tweaks
generation parameters in a panel (seed, mountainousness, erosion strength, forest density,
species mix) and hits **Regenerate**. Hand-editing (brushes, placing objects, saving worlds)
is phase 2+.

Visual target: **realistic**, not stylized — PBR textures (CC0), eroded terrain, believable
trees. This deliberately diverges from Warbell's low-poly vertex-color look.

## Decisions locked in brainstorm

| Question | Decision |
|---|---|
| Phase-1 scope | Generator + fly-cam + parameter panel + regenerate |
| Asset source | Procedural geometry + **CC0 PBR textures** (ambientCG/Polyhaven), fetched by script |
| Repo | `D:\bevy-world-editor`, fresh git repo, **Bevy 0.19** (same as Warbell — knowledge transfers) |
| Terrain scale | ~2×2 km, chunked + terrain LOD, all in RAM (no disk streaming) |
| Trees | 3–4 species (pine, spruce, oak/beech, birch — mixed Polish forest), full LOD chain to impostor |
| Approach | **A**: fBM + hydraulic erosion terrain, triplanar PBR splat, parametric (Weber–Penn-style) trees |

## Reuse from Warbell (`D:\tileworld-bevy-forest`)

Surveyed 2026-07-19. Copy as-is (port, rename env prefix `FOREST_*` → `WED_*`):

- **`meshkit.rs`** — merge/tint/flat-shade helpers (`duplicate_vertices` BEFORE
  `compute_flat_normals`; `Mesh::merge` drops indices when mixing indexed/non-indexed —
  normalise first).
- **`capture.rs`** — `WED_SHOT`/`WED_CLIP` screenshot & clip harness (≥240-frame + ≥10 s
  warmup, exit-on-file-exists, stale-file deletion).
- **`quality.rs`** — Low/High/Ultra presets, hardware-aware default via
  `wgpu::RenderAdapterInfo.device_type`, JSON persistence.
- **`atmospherics.wgsl`** — analytic height fog + sun in-scatter post pass.
- **IBL freeze** — CPU-baked gradient env cubemap + drop `GeneratedEnvironmentMapLight`
  after ~12 frames (≈2.3 ms/frame saved).
- **mulberry32** (`crates/core/rng.rs`) — deterministic scatter RNG.
- **Cargo profile block** — `[profile.dev.package."*"] opt-level = 3`, own crate
  `opt-level = 1, debug = 0` (MSVC LNK1140 dodge); wgpu major pinned to Bevy's.
- **Patterns**: chunk + up-front `compute_aabb()` + `VisibilityRange` LOD (AABB fills a
  frame late otherwise and LOD falls back to entity origin — chunk under camera sticks
  low-res); shared-material auto-batching; anti-grid noise discipline (no axis-aligned
  sine fields in anything visible; rotated-octave value noise).

Adapt: `scene.rs` lighting/post architecture (strip gameplay), `terrain.rs`
`ExtendedMaterial` wiring. Rewrite for realism: heightfield generation (Warbell's terraces
are gameplay-driven), trees (stylized, no LOD chain).

## Architecture

Cargo workspace, two crates (mirrors Warbell's proven split):

```
D:\bevy-world-editor\
├─ crates\worldgen\        pure CPU logic, NO Bevy deps, unit-tested, deterministic
│   ├─ rng.rs              mulberry32 (ported)
│   ├─ heightfield.rs      fBM + domain warp + ridged noise base
│   ├─ erosion.rs          droplet hydraulic erosion + thermal erosion
│   ├─ maps.rs             derived maps: flow/moisture, slope, normals
│   ├─ tree.rs             parametric tree skeleton generator (per species)
│   └─ scatter.rs          forest placement (density, species mix, jittered grid)
├─ src\                    Bevy app
│   ├─ main.rs             plugin assembly (Warbell style: one plugin per feature)
│   ├─ genrun.rs           async generation orchestration + progress UI
│   ├─ terrain_mesh.rs     chunked terrain meshing + LOD entities
│   ├─ terrain_mat.rs      ExtendedMaterial + splat WGSL wiring
│   ├─ trees_mesh.rs       skeleton → mesh (tubes + leaf cards), LOD chain, billboards
│   ├─ forest.rs           spawn scattered trees, far-field per-chunk billboard merge
│   ├─ flycam.rs           WASD + mouse-look + scroll speed
│   ├─ sky.rs              sun, Atmosphere::earth, cascades, SSAO/SMAA/bloom, IBL freeze
│   ├─ ui.rs               egui parameter panel + regenerate
│   ├─ quality.rs          ported preset system
│   ├─ capture.rs          ported WED_SHOT/WED_CLIP harness
│   └─ meshkit.rs          ported mesh helpers
├─ assets\shaders\terrain.wgsl
├─ assets\textures\        CC0 PBR sets (downloaded, gitignored; manifest committed)
└─ tools\fetch_textures.ps1  downloads + unpacks the ambientCG sets in the manifest
```

`worldgen` is a normal workspace member; dev profile gives it `opt-level = 3` explicitly
(the `"*"` wildcard doesn't cover workspace members) so erosion is fast in dev builds.

## Terrain

**Heightfield**: 2048×2048 cells @ 1 m → 2048×2048 m. `Vec<f32>`, ~16 MB.

**Generation pipeline** (in `worldgen`, deterministic per seed, run on a background task
with progress reporting; target < ~10 s on desktop):

1. **Base noise** — 6–8 octave fBM value noise with domain warping; a ridged-fBM component
   blended in by a "mountainousness" mask (itself low-frequency noise + user weight) so the
   map has distinct massifs and lowlands, not uniform bumpiness. Height range ~0–260 m.
2. **Hydraulic erosion** — particle droplets (~250k default, user-scalable): each droplet
   descends the gradient, picks up sediment as a function of speed·water, deposits when
   capacity drops, evaporates. This carves gullies, V-valleys, alluvial fans — the thing
   raw noise can't fake. Classic Hans Beyer/Sebastian Lague formulation, tuned.
   Erosion **also outputs a flow map** (accumulated water passage per cell).
3. **Thermal erosion** — a few relaxation passes: slopes above talus angle shed material
   downhill → scree cones under crags.
4. **Derived maps** — slope, **moisture** (blur of flow map + proximity to water level),
   normals. These drive both the splat shader and vegetation placement.

**Water**: a single flat water plane at a configurable lake level; cells below it count as
water for moisture/vegetation. No river carving in phase 1 (erosion gullies already read
as drainages).

**Meshing** (`terrain_mesh.rs`): 32×32 chunks of 64×64 cells. Per chunk two sibling
entities sharing one material: full-res (65×65 verts) and coarse (stride 4, with skirt,
`NotShadowCaster`), swapped by `VisibilityRange` with a dithered crossfade band
(~full-res inside 180 m). AABBs computed and inserted **up front**. Normals sampled from
the heightfield (central difference), smooth-shaded — realistic terrain wants smooth
normals + normal-mapped detail, not Warbell's facets.

**Material** (`terrain_mat.rs` + `terrain.wgsl`):
`ExtendedMaterial<StandardMaterial, TerrainExtension>`. Four PBR layers splatted in the
fragment shader — **grass**, **forest floor** (leaf litter), **rock** (triplanar-projected
on steep slopes so cliffs don't smear), **dirt/scree** — weighted by slope, altitude and
moisture (passed per-vertex: UV2/color channels carry moisture + flow). Each layer:
albedo + normal + roughness (+AO where the set has it), 2K, sampled at two scales and
blended by rotated-octave value noise to kill tiling (Warbell anti-grid lesson). Snow cap
above a height threshold can wait (phase 1 optional flag, default off).

## Trees

Species (phase 1): **pine** (sosna), **spruce** (świerk), **beech/oak-like broadleaf**
(dąb/buk), **birch** (brzoza).

**Skeleton generation** (`worldgen/tree.rs`): recursive parametric branching, simplified
Weber–Penn. Per-species param set: trunk height/taper, branch levels (2–3), branches per
level, split angles, down/up-curving (gravitropism), phototropism bias, gnarl (per-segment
random yaw), child length ratios, needle/leaf zones. Output: a list of swept segments
(position, direction, radius start/end) + leaf-anchor points with orientation + size.
Deterministic per (species, seed). **4 baked variants per species** (different seeds),
plus per-instance yaw/scale/hue jitter at placement — 16 unique meshes read as a varied
forest without exploding memory.

**Mesh build** (`trees_mesh.rs`, main-model work — done by hand, not delegated):
- **Wood**: tapered tubes along segments (6–8 ring sides at LOD0, fewer up the hierarchy),
  cylindrical UVs → bark PBR texture (per species: pine bark, birch bark, generic
  broadleaf bark from ambientCG). Root flare at the base.
- **Foliage**: **leaf cards** — camera-agnostic quads (single or V-folded pairs) at leaf
  anchors, alpha-masked leaf-cluster texture, `AlphaMode::Mask`. Normals bent outward
  from the canopy centroid ("spherical normals") so lighting reads soft and volumetric,
  not per-quad flat. Conifers use needle-frond cards angled along boughs.
- **Leaf/needle textures**: generated procedurally by a bake tool (`WED_BAKE=1` run mode):
  draws leaf clusters / needle fronds (shape + veins + color variation) into PNG atlases
  with alpha + matching normal maps, written into `assets/textures/foliage/` and
  committed. Full control, no CC0-leaf-card scavenging.
- **LOD chain** per variant:
  - **LOD0** (< 60 m): full skeleton, ~8–15k tris.
  - **LOD1** (60–150 m): pruned smallest branch level, fewer+larger cards, ~2–4k tris.
  - **LOD2** (150–400 m): trunk billboard + 3 crossed canopy quads, < 200 tris, texture
    baked from the LOD0 model by the same `WED_BAKE` tool (renders each variant to an
    impostor atlas via a one-shot capture camera at bake time — never at gameplay time,
    so the single-camera runtime constraint is untouched).
  - **Far field** (> 400 m): per-terrain-chunk **merged mesh** of all its LOD2 impostors
    (crossed quads work merged; no per-frame billboarding), one entity per chunk,
    `NotShadowCaster`. Beyond ~1.5 km trees cull entirely.
- Near LODs are per-tree sibling entities with complementary `VisibilityRange`s (Warbell
  terrain-LOD pattern); tree count target 30–60k so entity count stays sane.

**Placement** (`worldgen/scatter.rs`): jittered-grid scatter per chunk (deterministic
mulberry32 per chunk), rejection by slope (> ~35° bare), altitude (treeline ~220 m) and
water. Density scaled by user parameter × moisture. Species mix by site: birch + spruce on
moist lowland, beech/oak mid-slopes, pine on dry/sandy ridges, spruce toward the treeline.
Small clearings from low-frequency noise so the forest has meadows, not uniform carpet.

## App shell

- **Fly-cam**: right-mouse-hold mouse-look, WASD + QE vertical, scroll = speed, shift =
  boost. Spawns above terrain centre.
- **Lighting/sky** (`sky.rs`): adapted Warbell stack — `Atmosphere::earth` sky + sun disk,
  one directional sun (4 cascades), `ContactShadows`, SSAO, SMAA, restrained bloom,
  gradient-cubemap IBL with the freeze trick, `atmospherics.wgsl` height fog. Neutral
  grade (default AgX, no filmic push) — realism, not mood. Fixed pleasant sun angle
  (morning) in phase 1; time-of-day slider is a stretch goal.
- **UI** (`ui.rs`, `bevy_egui` 0.41): side panel — seed (+randomize), terrain params
  (mountainousness, erosion droplets, lake level), forest params (density, species
  weights), Regenerate button, progress bar during async regen, FPS readout.
- **Regenerate** (`genrun.rs`): runs the worldgen pipeline on `AsyncComputeTaskPool`,
  progress via channel; on completion despawns all `WorldEntity`-tagged entities and
  respawns terrain + forest. World gen never blocks the frame.
- **Quality + capture**: ported `quality.rs` and `capture.rs` (`WED_SHOT`, `WED_CLIP`,
  `WED_CAM`, `WED_QUALITY`, `WED_SEED`).

## Error handling

- Missing texture sets → clear startup log line naming the set + `tools/fetch_textures.ps1`,
  and a flat-color fallback material so the app still runs.
- Degenerate gen parameters (zero droplets, zero density) are valid and just produce
  smoother/barren terrain; sliders clamp to sane ranges.
- Erosion NaN guard: droplet loop clamps and skips non-finite cells (defensive; unit test
  asserts no NaN/Inf in output for fuzzed seeds).

## Testing

- `worldgen` unit tests: mulberry32 known-sequence; heightfield determinism (same seed →
  same hash, different seed → different); erosion sanity (no NaN, bounded heights,
  total-material change within tolerance for mass-conserving steps); slope/moisture maps
  in range; tree skeleton invariants (segment radii monotone down the hierarchy, anchor
  counts per species in expected band); scatter determinism + slope/water rejection.
- Visual verification: `WED_SHOT` screenshots at staged coords (valley floor, ridge,
  forest interior, far overview) checked after each visual milestone — Opus subagents
  drive the capture-iterate loop, per the household rule (Fable decides, Opus fetches).

## Milestones (implementation order)

1. Workspace scaffold + Cargo + git; `worldgen` rng/heightfield/erosion/maps + tests.
2. App boots: fly-cam, sun+sky, gray chunked terrain with LOD → first `WED_SHOT`.
3. Splat material + fetched CC0 textures → textured terrain shot.
4. Tree skeletons + wood/foliage meshes + bake tool (leaf atlases, impostors) → tree
   line-up shot (all species × variants).
5. Scatter + forest spawn + far-field merge → forest interior + overview shots.
6. Atmosphere/fog/quality/capture polish, egui panel + async regenerate.

## Out of scope (phase 1)

Editing brushes, object placement, save/load of worlds, rivers/waterfalls, roads,
grass/undergrowth billboards, wind animation, seasons, disk streaming, export formats.
Each can layer on later without re-architecting (worldgen stays pure; world spawn is
already a despawn-respawn cycle).
