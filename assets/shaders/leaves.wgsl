// Leaf-card wind sway — vertex-stage-only override for
// ExtendedMaterial<StandardMaterial, LeafSway>. The fragment stays the stock
// StandardMaterial one (alpha-mask atlas etc.).
//
// Cheap "the forest is alive" pass: every foliage vertex drifts on two summed sines in
// a fixed wind direction, phase keyed on world position (neighbouring trees desync),
// amplitude breathing on a slow gust cycle. Cards translate near-rigidly (they're
// small), which reads as branches stirring. Applies to every tier that shares the leaf
// material — the far billboards get it too, invisible at that range and still cheap.
// Mirrors bevy_pbr mesh.wgsl's vertex stage minus skinning/morph (leaves have neither).

#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}

struct SwayParams {
    // x = time (s) — a material uniform, NOT globals: the prepass layout lacks globals.
    params: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> sway_u: SwayParams;

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let mesh_world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);

#ifdef VERTEX_NORMALS
    out.world_normal =
        mesh_functions::mesh_normal_local_to_world(vertex.normal, vertex.instance_index);
#endif

#ifdef VERTEX_POSITIONS
    out.world_position = mesh_functions::mesh_position_local_to_world(
        mesh_world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );

    let t = sway_u.params.x;
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
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        mesh_world_from_local,
        vertex.tangent,
        vertex.instance_index,
    );
#endif
#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index, mesh_world_from_local[3]);
#endif

    return out;
}
