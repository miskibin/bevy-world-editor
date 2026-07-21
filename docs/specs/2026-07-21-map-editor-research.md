# Map-editor pivot — research synthesis (2026-07-21)

## LOCKED DECISIONS (user, 2026-07-21)

1. **Scope: "LDtk of realistic nature scenes", Bevy-favoured.** Engine-agnostic open
   exports for everyone, but Bevy gets a SEAMLESS first-class path: a runtime crate
   that loads the editor project directly into a Bevy game — terrain, LOD trees,
   grass, props, collision AABBs, creature animations working out of the box.
2. **Editing model: non-destructive layers + painting** (paint-drives-procedural).
   Node graph maybe later, never as the primary UX.
3. **License: FOSS dual MIT/Apache-2.0**, donations later; paid binary = later lever.
4. **Importers: Bevy first** (the runtime crate IS the importer), then raw formats
   cover Godot/Unity/Unreal; dedicated bridges later.

Three research passes (editor UX landscape / architecture+interop / tool adoption), full
reports in session logs. This file is the distilled, decision-ready version.

## The market gap (why this can win)

- Heightfield generators (World Machine, Gaea, World Creator, Houdini, TerraForge3D)
  stop at heightmap + masks — **nobody ships a populated, dressed nature scene**.
- Populated-scene tools (Unreal PCG, Unity Terrain, Godot Terrain3D) are **engine-locked**.
- Rust/Bevy field is empty (bevy_mesh_terrain_editor archived at 19 stars).
- Incumbent weaknesses to exploit: World Creator re-introduced **subscriptions** (2024),
  Gaea **broke 1.0 projects in 2.0** + weak docs, World Machine has a **build-and-wait**
  workflow. An open, real-time, no-lock-in alternative has a clear story.

**Positioning: the "LDtk of realistic nature scenes" — engine-agnostic, real-time,
open-format editor that outputs a whole populated forest/terrain scene.**

## Non-negotiable core (the 5 loved-vs-abandoned features)

1. **Real-time feedback** — no "Build" wall (World Creator's winning trait; our Bevy
   renderer makes this natural). Low-res preview while dragging, full-res on release.
2. **Non-destructive, maskable layer stack** — reorder/disable/re-tune any step.
3. **Paint-drives-procedural** — painted masks steer procedural scatter density +
   exclusion splines (the Unreal PCG pattern, engine-agnostic).
4. **Open, documented, stable file formats** — the #1 adoption driver AND the #1
   killer mistake if missed. LDtk's dual model: rich project file + simple export.
5. **Safety net** — unlimited undo, autosave, crash recovery, great defaults
   ("good-looking result in 2 minutes").

## Architecture decisions (from the interop report)

- **Project = seed + WorldParams + ordered non-destructive layer stack** (height-delta
  sculpt layers, painted mask layers, manual-placement overrides). Brush strokes are
  layers re-applied after regeneration — NEVER written into the generated heightfield.
- **Undo = command pattern**: parameter edits store old/new; brush strokes store
  dirty-region AABB diffs. Memory-capped. Never whole-world snapshots.
- **Project file**: human-readable RON/JSON (git-friendly), bulky rasters as binary
  sidecars (EXR/PNG16) referenced by path.
- **Export lives in pure `crates/worldgen`** (serde + image, no Bevy) — deterministic,
  testable, headless. Tiers:
  - T1: **R16 + EXR heightmap** (sidecar JSON with world size/min/max), **RGBA
    splatmaps**, **JSON + CSV scatter instance lists** (stable mesh_id manifest).
  - T2: **glTF .glb** with EXT_mesh_gpu_instancing (bonus; Godot doesn't read the
    extension — lists are the fallback). Tiled output behind a flag.
  - T3: USD later (watch AOUSD; don't spend now).
  - Bridge importers per engine as cheap wins (Unity wizard, Unreal Python, Godot
    Terrain3D reads r16 natively).
- **UI: bevy_egui** (official Bevy editor is unshipped — don't couple). Mine
  **jackdaw** (jbuehler23) for Bevy sculpt/paint reference code. **BRP** as optional
  automation side-door (headless batch export, CI).
- **Perf playbook**: low-res preview on drag → full-res on release; per-stage output
  caching keyed by input hash (forest slider must not re-run erosion); worker thread +
  progressive tiles; GPU shallow-water erosion later (CPU stays the tested oracle).

## Adoption plan (from the adoption report)

- **License: FOSS, dual MIT/Apache-2.0, free.** Donations (GitHub Sponsors / Open
  Collective) once there are users. Aseprite-style paid binary = possible later lever,
  not a launch gate. Story: "own your tools — the open alternative".
- **v1.0 launch checklist**: undo/redo everywhere · Win+mac+Linux builds · documented
  versioned format + ≥1 engine importer · standard interchange (r16/EXR/splat/glTF) ·
  CLI/headless batch mode (Gaea charges for this — give it away) · docs site with a
  worked example · 3-5 example projects · 2-4 min launch video (build → export → runs
  in engine) · public roadmap · itch.io page + r/gamedev + r/proceduralgeneration +
  Bevy Discord + Show HN.
- **The killer mistake to avoid**: shipping before the export format is open, stable,
  documented, and importable. Design the format first; the editor is the thing that
  writes it.

## Per-tool cheat sheet (features to steal)

- Gaea 2: Selective Precipitation (mask/slope/altitude-limited erosion).
- World Creator: instant GPU preview of every edit.
- Houdini: named layer stacks on the heightfield, masks target any effect.
- Unreal PCG: painted layer weight = scatter density multiplier; exclusion splines
  with soft falloff; per-biome graphs; World Partition streaming.
- Godot Terrain3D: non-contiguous 1 km regions (archipelagos without paying VRAM for
  the ocean); r16 as the de-facto interchange.
- LDtk: dual format (rich project + Super Simple Export); rule-based auto-layers.
- WorldPainter: any grayscale image is a brush; DEM import.
- TrenchBroom: single-viewport tactile editing; razor scope.
