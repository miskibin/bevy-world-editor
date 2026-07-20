// PREPASS twin of leaves.wgsl — the depth/normal prepass MUST apply the exact same wind
// sway as the main pass: with a DepthPrepass on the camera the main pass depth-tests
// EQUAL against prepass depth, and any mismatch discards the swayed leaf pixels (they
// render as sky-colored silhouettes — the "frosted trees" bug).
// KEEP THE SWAY MATH IDENTICAL to leaves.wgsl.

#import bevy_pbr::{
    mesh_functions,
    prepass_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}
#import bevy_render::globals::Globals

// The PREPASS view layout binds Globals at binding 1 (the main pass uses 11, and no WGSL
// module declares the prepass one) — see bevy_pbr::prepass view_layout_* in 0.19.
@group(0) @binding(1) var<uniform> globals: Globals;

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let mesh_world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);

    out.world_position = mesh_functions::mesh_position_local_to_world(
        mesh_world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );

    // ── sway (identical to leaves.wgsl) ─────────────────────────────────────────
    let t = globals.time;
    let wp = out.world_position.xyz;
    let gust = 0.55 + 0.45 * sin(t * 0.23 + wp.x * 0.012);
    let sway = sin(t * 1.4 + wp.x * 0.35 + wp.z * 0.27) * 0.6
        + sin(t * 3.1 + wp.x * 0.9 - wp.z * 0.5) * 0.4;
    let wind_dir = vec2<f32>(0.85, 0.53);
    let amp = 0.055 * gust;
    out.world_position = vec4<f32>(
        wp + vec3<f32>(wind_dir.x * sway * amp, sway * amp * 0.25, wind_dir.y * sway * amp),
        out.world_position.w,
    );
    out.position = position_world_to_clip(out.world_position.xyz);

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
#ifdef VERTEX_NORMALS
    out.world_normal =
        mesh_functions::mesh_normal_local_to_world(vertex.normal, vertex.instance_index);
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        mesh_world_from_local,
        vertex.tangent,
        vertex.instance_index,
    );
#endif
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif

#ifdef VISIBILITY_RANGE_DITHER
    // MUST be set — an uninitialised dither level discards leaf pixels through the whole
    // LOD-crossfade band (the band-localised "frost" variant of the depth-EQUAL bug).
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index, mesh_world_from_local[3]);
#endif

    return out;
}
