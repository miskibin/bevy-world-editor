# NatureScene project format — v1 (DRAFT, stabilizes at editor v1.0)

The project file is the product. Editors are things that write it; engines are things
that read its exports. This document is the contract.

## Files on disk

```
myforest.nsproj            # RON, human-readable, git-friendly — the project
myforest.nsproj.d/         # sidecar directory (binary rasters, referenced by name)
  layer-3-heightdelta.png  # PNG16 grayscale
  layer-5-forest.png       # PNG8 grayscale
```

## Project file (RON)

```ron
(
    format_version: 1,          // bumped ONLY on breaking shape changes;
                                // additive fields use serde defaults
    name: "My Forest",
    params: WorldParams(..),    // the existing worldgen params, seed included
    layers: [ Layer(..), .. ],  // ordered, applied bottom-to-top after generation
)
```

## Layers — the non-destructive stack

A layer NEVER mutates the generated base; regeneration re-runs the procedural
pipeline, then re-applies the stack. Order matters.

```ron
Layer(
    id: 7,                  // unique within the project, never reused
    name: "flatten camp",
    enabled: true,
    opacity: 1.0,           // multiplies the layer's effect
    kind: HeightDelta(sidecar: "layer-7-heightdelta.png", scale: 40.0),
)
```

### Layer kinds

| kind | payload | semantics |
|---|---|---|
| `HeightDelta { sidecar, scale }` | PNG16 grayscale at grid res | `delta_m = (v/65535 - 0.5) * 2 * scale * opacity`, added to the eroded heightfield BEFORE lakes/slope/moisture/trails/scatter recompute |
| `Mask { channel, sidecar }` | PNG8 grayscale at grid res | `w = v/255 * opacity`; channels below |
| `Instances { added, removed }` | inline in the RON | applied AFTER scatter |

### Mask channels

| channel | effect |
|---|---|
| `ForestDensity` | multiplies tree-spawn probability: `p *= lerp(1.0, 2.0, w)` for w>0.5 boost, `p *= 2w` for w<0.5 suppress (w=0.5 neutral; an unpainted mask is all-0.5) |
| `Clearing` | where `w > 0.5`: trees, rocks and props are suppressed entirely (hard exclusion, soft-falloff handled by painting soft brushes) |
| `PathWear` | added to the trail-wear field (max-combined) — painted paths look walked and repel scatter like generated trails |
| `GrassDensity` | exported + used by renderers; does not affect worldgen scatter |

### Instance overrides

```ron
Instances(
    added: [ (mesh: "tree/pine/2", x: 410.5, z: 220.0, yaw: 1.2, scale: 1.1) ],
    // y is derived from the terrain at load — never stored
    removed: [ (kind: Tree, x: 400.0, z: 218.0, radius: 3.0) ],
)
```
Removal is positional (anything of `kind` within `radius`). Documented caveat: if the
seed or terrain params change, scattered instances move and a removal may stop
matching — positional removal is an override, not an identity.

## Apply pipeline (normative)

```
base   = fBM + warp + hydraulic + thermal erosion        (from params, seed)
height = base + Σ enabled HeightDelta layers             (in stack order)
lakes  = priority-flood(height)                          (recomputed)
maps   = slope, moisture, flow                           (recomputed)
trails = dijkstra(height, ...) ∪ PathWear masks
scatter= trees/rocks/props(height, maps, trails, ForestDensity, Clearing)
world  = scatter ± Instances layers
```

Determinism: same project file + sidecars → byte-identical `World`. A project with an
empty layer stack must produce exactly what `generate(params)` produced before this
format existed (regression-tested).

## Export bundle (`wed export proj.nsproj --out dir/`)

```
dir/
  heightmap.r16        # 16-bit LE row-major, row 0 = min Z edge; range meta.json
  heightmap.exr        # 32-bit float meters (optional flag)
  splatmap.png         # RGBA8 material weights: R grass, G forest-floor, B rock, A dirt
  masks/moisture.png   # PNG16
  masks/flow.png
  masks/trail_wear.png
  masks/water_depth.png  # 0 = dry
  instances.json       # [{"mesh":"tree/pine/2","x":..,"y":..,"z":..,"yaw":..,"scale":..}]
  instances.csv        # mesh,x,y,z,yaw,scale — same data, flat
  manifest.json        # {"format_version":1,"mesh_ids":[..with per-id counts..]}
  meta.json            # world_size_m, grid_resolution, cell_size_m,
                       # height_min_m, height_max_m, seed, generator_version
```

`mesh_id` grammar: `<family>/<species-or-kind>/<variant>` — e.g. `tree/birch/1`,
`rock/boulder/3`, `prop/bush_broadleaf/0`, `prop/log/1`, `prop/mushroom/2`.
IDs are STABLE across releases; new ids may be added, existing ids never change
meaning.

## Bevy runtime (first-class path)

The `naturescene` crate (workspace member) loads a `.nsproj` directly in a Bevy app:
terrain mesh + splat data, instanced vegetation with LOD ranges, collision AABBs.
Other engines consume the export bundle; Godot Terrain3D reads `heightmap.r16` +
`splatmap.png` natively.

## Stability promise

- `format_version` bump = breaking change = a documented migration path ships with it.
- Additive changes (new layer kinds, new mask channels, new export files) do NOT bump
  the version; readers must ignore unknown fields (serde defaults both ways).
