# Performance: tooling, measurements, findings

## Tooling in the repo

| Hook | What it does |
|---|---|
| **F2** | live overlay: fps + frame-time sparkline, entity/mesh/image counts, per-render-pass GPU timing bars (`src/stats.rs`) |
| `WED_PROFILE=1` | scripted benchmark: flies 5 fixed poses (forest interior, canopy fly-over, lake shore, high overview, ridge vista), reports median/p95 frame time + top GPU passes per pose, then exits (`src/profile.rs`). `WED_PROFILE_SECS=<n>` lengthens each window |
| `WED_PERFLOG=1` | dumps the sorted GPU-pass table to the console every ~2 s |
| `WED_GPUSTRESS=<mult>` | renders the 3D main pass at `mult`× window resolution (weak-GPU emulation via `MainPassResolutionOverride`) |
| `WED_LOWGFX=1` | boots the LOW preset (fast terrain shader lane, 1024 shadow map, no SSAO/contact shadows/haze/DoF) |
| `WED_NOWIND` / `WED_NOGRASS` / `WED_NOTREES` | kill-switches for bisecting CPU cost |
| `WED_OCCLUSION=1` | enables Bevy 0.19 two-phase hi-Z occlusion culling on the camera (off by default — measured neutral here) |

**Methodology**: compare *median* frame time per pose (p95 catches streaming hitches), and always
check the GPU-pass total against the frame time. If Σ passes ≪ frame time you are **CPU-bound** and
GPU work is irrelevant — that was the case here.

## Measured findings (1088 m map, 13k trees, 7k props, debug build)

| Pose | Before | After wind fix |
|---|---|---|
| forest-interior | 13.7 ms | 11.3 ms |
| canopy-flyover | 20.0 ms | 7.5 ms |
| lake-shore | 24.8 ms | 11.3 ms |
| high-overview | 20.4 ms | 7.9 ms |
| ridge-vista | 27.2 ms | 10.2 ms |

**The one big win: never mutate a material asset per frame.** Wind time was driven by writing a
`#[uniform]` field on the shared leaf material every frame; mutating an entry in `Assets<M>`
re-prepares that material and everything keyed on it. Removing it (time now comes from the view
`globals` uniform — main pass binding 11, **prepass binding 1**, which no WGSL module declares, so
the prepass shader declares it itself) cut the worst pose by 2.4×.

**The one big GPU win, earlier:** the bokeh DoF's 32-tap gather ran fullscreen even where the
circle of confusion was sub-pixel. A per-pixel early-out took a water-facing view from 12 → 119 fps.
Custom post passes are **invisible** in `RenderDiagnosticsPlugin`'s table unless they record a span
(`ctx.diagnostic_recorder().as_deref()` → `time_span`) — instrument them or they read as free.

**Release vs debug is a wash** (10.99 vs 12.87 ms forest-interior, uncapped): our crate already
builds at `opt-level = 1` and every dependency at 3, so there is no "just build release" win left.
The profiler forces `PresentMode::AutoNoVsync` — with vsync on, every measurement floors at the
refresh interval and improvements below that are invisible.

**Where the remaining frame goes**: ~10 ms frame vs ~3.3 ms of GPU passes → still CPU-bound in the
heavy poses. Next step when it matters: a `tracing-chrome`/Tracy capture to name the systems
(prime suspects: grass/tree chunk streaming building meshes on the main thread, and per-frame
visibility work over ~10k entities).

**Measured neutral (kept anyway / kept off):**
- `NoCpuCulling` on static world entities: ±1.5 ms, i.e. noise at this entity count. Kept — it is
  the right call as counts grow, and GPU culling is already in `Culling` mode.
- `OcclusionCulling` (0.19, two-phase hi-Z): ±1 ms. Off by default. A forest is a poor occluder
  scene (holey alpha canopies); terrain ridges are the only real occluders.

## What the research says we should do next (not yet done)

From two research passes (modern-engine techniques + a Bevy-0.19 source audit):

1. **Shadow pass is the classic forest tax** — 4 cascades × 4096 re-rasterize the canopy 4×. Levers:
   force the impostor tier into far cascades, shrink `maximum_distance`, and (Epic's biggest foliage
   shadow win) **disable wind vertex animation beyond a distance** so the shadow geometry is static.
2. **Leaf overdraw**: tighten leaf cards to their alpha bounds, keep masked (never blend), and sort
   foliage front-to-back so the depth prepass rejects early.
3. **Bindless on our `ExtendedMaterial`s** (`#[bindless(N)]` on the extension structs) so multiple
   terrain/water material handles share a bind group and can batch together.
4. **Normalize vertex-attribute sets** across merged props/trees — different attribute layouts land
   in different mesh-allocator slabs and can never share a multi-draw batch set.
5. **Terrain**: geomorphing (CDLOD-style) instead of discrete LOD + skirts; slope-gate triplanar
   (already done) — Witcher 3's REDengine applies triplanar to the background layer only.
6. **Render scale / upsampling** is the cheapest large lever; we expose it as the supersampling
   slider (it currently runs *above* 1.0 for quality — it can go below 1.0 for speed).

Sources are linked in the session notes; the highest-value primaries were the idTech 666 SIGGRAPH
deck, the REDengine 3 GDC deck, Epic's Virtual Shadow Map / Nanite Foliage docs, and the Bevy 0.19
release notes + `bevy_render` source (`gpu_preprocessing.rs`, `occlusion_culling/mod.rs`,
`prepass/mod.rs` view layouts).
