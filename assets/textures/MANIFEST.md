# Texture Manifest

All texture sets are sourced from [ambientCG](https://ambientcg.com) and are
released under the **Creative Commons CC0 1.0** (public domain) license — free
for any use, no attribution required.

Variant downloaded: **2K JPG**. Maps are renamed to a standard scheme per set:

| standard file  | ambientCG map      |
|----------------|--------------------|
| `albedo.jpg`   | `Color`            |
| `normal.jpg`   | `NormalGL` (OpenGL)|
| `roughness.jpg`| `Roughness`        |
| `ao.jpg`       | `AmbientOcclusion` |

Fetch/refresh with `tools/fetch_textures.ps1` (idempotent). The image files
themselves are gitignored; this manifest is committed.

## Ground layers — `assets/textures/ground/<layer>/`

| layer          | ambientCG ID | license | maps present                     | notes |
|----------------|--------------|---------|----------------------------------|-------|
| `grass`        | Grass001     | CC0     | albedo, normal, roughness, ao    | Dense fresh natural green lawn/meadow (not over-saturated golf-course). |
| `forest_floor` | Ground023    | CC0     | albedo, normal, roughness, ao    | Brown forest leaf-litter — dirt + leaves + sticks. |
| `rock`         | Rock035      | CC0     | albedo, normal, roughness, ao    | Grey layered/fractured cliff / mountain rock. |
| `dirt`         | Ground081    | CC0     | albedo, normal, roughness, ao    | Brown bare dirt path, rocky/gravel scree. |

## Bark — `assets/textures/bark/<species>/`

| species     | ambientCG ID | license | maps present                     | notes |
|-------------|--------------|---------|----------------------------------|-------|
| `pine`      | Bark014      | CC0     | albedo, normal, roughness, ao    | Rough conifer (fir) brown plated bark — closest ambientCG has to reddish pine plate. |
| `broadleaf` | Bark012      | CC0     | albedo, normal, roughness, ao    | Oak — grey-brown broadleaf bark (beech-like). |
| `birch`     | — (none)     | —       | —                                | **Not available on ambientCG.** No white birch-bark material exists (a "birch" search returns only finished birch *countertop wood*, not bark). Deliberately NOT substituted. Source elsewhere (e.g. Poly Haven / hand-paint) if a white birch bark is needed. |

## Asset ID rationale

- **Grass001** over Grass004/005: most natural park/lawn green, soft and dense, not saturated.
- **Ground023** for forest floor: tags `brown, dirt, forest, leaves, sticks` — true leaf-litter, not autumn-orange.
- **Rock035**: grey `cliff` rock with layered/cave fracture read (matches the requested Rock030/Rock035 cliff look).
- **Ground081** for dirt: `brown, dirt, gravel, path, rocky` bare earth/scree.
- **Bark014** for pine: only conifer (fir) bark in the set; rough brown plated.
- **Bark012** for broadleaf: oak, the generic grey-brown broadleaf option.
