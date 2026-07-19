// Terrain splat shader — ExtendedMaterial<StandardMaterial, TerrainExtension>.
//
// Four PBR layers packed as texture ARRAYS (albedo/normal/roughness, layers:
// 0=grass 1=forest_floor 2=rock 3=dirt). Weights come from slope (world normal),
// moisture + flow (mesh UV1 — NOT vertex color, which StandardMaterial would multiply
// into base_color), and rotated-lattice value noise for organic patch boundaries.
// Rock is triplanar-projected so cliffs don't smear; flat layers are world-XZ planar
// sampled at two scales blended by noise to bury the tiling repeat.
//
// Anti-grid discipline (Warbell lesson): every noise octave lives on its own rotated
// lattice; nothing visible aligns to world axes.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

struct TerrainParams {
    // x = planar UV scale (1/m), y = second-scale factor, z = water level (world y), w = normal strength
    params: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> ter: TerrainParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var albedo_arr: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var albedo_samp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var normal_arr: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var normal_samp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var rough_arr: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var rough_samp: sampler;

// Sine-free Hoskins hash12 — no directional correlation, no big-coordinate breakdown.
fn t_hash(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn t_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let a = t_hash(i);
    let b = t_hash(i + vec2<f32>(1.0, 0.0));
    let c = t_hash(i + vec2<f32>(0.0, 1.0));
    let d = t_hash(i + vec2<f32>(1.0, 1.0));
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    return mix(a, b, u.x) + (c - a) * u.y * (1.0 - u.x) + (d - b) * u.x * u.y;
}

fn t_noise_rot(p: vec2<f32>, c: f32, s: f32) -> f32 {
    return t_noise(vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y));
}

// Two-octave patch noise for layer boundaries + de-tiling masks.
fn patch_noise(p: vec2<f32>) -> f32 {
    return t_noise_rot(p, 0.946, 0.326) * 0.65 + t_noise_rot(p * 2.3, 0.556, 0.831) * 0.35;
}

// Planar (world-XZ) layer sample at two scales, blended by a large-scale noise mask so
// the texture repeat never registers.
fn sample_planar(layer: i32, wp: vec3<f32>) -> vec4<f32> {
    let s1 = ter.params.x;
    let s2 = ter.params.x * ter.params.y;
    let m = patch_noise(wp.xz * 0.021);
    let a = textureSample(albedo_arr, albedo_samp, wp.xz * s1, layer);
    let b = textureSample(albedo_arr, albedo_samp, wp.xz * s2 + vec2<f32>(0.37, 0.71), layer);
    return mix(a, b, clamp(m * 1.4 - 0.2, 0.0, 1.0));
}

fn sample_planar_rough(layer: i32, wp: vec3<f32>) -> f32 {
    return textureSample(rough_arr, rough_samp, wp.xz * ter.params.x, layer).r;
}

// Tangent-space normal fetch, decoded to [-1,1].
fn fetch_ts_normal(uv: vec2<f32>, layer: i32) -> vec3<f32> {
    return textureSample(normal_arr, normal_samp, uv, layer).rgb * 2.0 - 1.0;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let wp = in.world_position.xyz;
    let gn = normalize(in.world_normal);

    // UV1 carries per-vertex data: x = moisture 0..1, y = normalised flow 0..1.
    var moisture = 0.5;
    var flow = 0.0;
#ifdef VERTEX_UVS_B
    moisture = in.uv_b.x;
    flow = in.uv_b.y;
#endif

    // ── Layer weights ────────────────────────────────────────────────────────────
    let slope = 1.0 - gn.y; // 0 flat … 1 vertical
    let boundary = patch_noise(wp.xz * 0.055) - 0.5; // wobbles every threshold organically

    // Rock: steep faces. Threshold wobbles so the grass/rock line isn't a contour.
    let rock_w = smoothstep(0.16 + boundary * 0.06, 0.30 + boundary * 0.06, slope);
    // Dirt: gully floors (high flow) + dry patches; suppressed on rock.
    let dirt_flow = smoothstep(0.35, 0.75, flow);
    let dirt_dry = smoothstep(0.25, 0.05, moisture) * smoothstep(0.5, 0.8, patch_noise(wp.xz * 0.013));
    let dirt_w = clamp(dirt_flow + dirt_dry, 0.0, 1.0) * (1.0 - rock_w);
    // Forest floor: moist, sheltered ground in noise-broken patches.
    let ff_w = smoothstep(0.42 + boundary * 0.12, 0.72, moisture) * (1.0 - rock_w) * (1.0 - dirt_w);
    let grass_w = max(1.0 - rock_w - dirt_w - ff_w, 0.0);

    // ── Albedo ───────────────────────────────────────────────────────────────────
    var albedo = sample_planar(0, wp) * grass_w
        + sample_planar(1, wp) * ff_w
        + sample_planar(3, wp) * dirt_w;

    // Rock: triplanar so vertical faces don't smear.
    let rs = ter.params.x * 1.6;
    let an = abs(gn);
    let tw = pow(an, vec3<f32>(4.0));
    let tws = tw / (tw.x + tw.y + tw.z);
    if rock_w > 0.001 {
        let rx = textureSample(albedo_arr, albedo_samp, wp.zy * rs, 2);
        let ry = textureSample(albedo_arr, albedo_samp, wp.xz * rs, 2);
        let rz = textureSample(albedo_arr, albedo_samp, wp.xy * rs, 2);
        albedo += (rx * tws.x + ry * tws.y + rz * tws.z) * rock_w;
    }

    // Faint large-scale value drift — cures the "one flat green" read at distance.
    let drift = 0.90 + 0.20 * patch_noise(wp.xz * 0.0045);
    pbr_input.material.base_color = vec4<f32>(albedo.rgb * drift, 1.0);

    // ── Roughness ────────────────────────────────────────────────────────────────
    let rough = sample_planar_rough(0, wp) * grass_w
        + sample_planar_rough(1, wp) * ff_w
        + sample_planar_rough(2, wp) * rock_w
        + sample_planar_rough(3, wp) * dirt_w;
    pbr_input.material.perceptual_roughness = clamp(rough, 0.35, 1.0);

    // ── Normal perturbation ──────────────────────────────────────────────────────
    // Flat layers: treat the map as world-XZ tangent space (T=+X, B=+Z) — fine for
    // mostly-up terrain. Rock: whiteout-ish blend of the triplanar fetches.
    let strength = ter.params.w;
    let tsn = fetch_ts_normal(wp.xz * ter.params.x, 0) * grass_w
        + fetch_ts_normal(wp.xz * ter.params.x, 1) * ff_w
        + fetch_ts_normal(wp.xz * ter.params.x, 3) * dirt_w;
    var n = normalize(gn + vec3<f32>(tsn.x, 0.0, tsn.y) * strength * (1.0 - rock_w));
    if rock_w > 0.001 {
        let nx = fetch_ts_normal(wp.zy * rs, 2);
        let ny = fetch_ts_normal(wp.xz * rs, 2);
        let nz = fetch_ts_normal(wp.xy * rs, 2);
        let rock_perturb = vec3<f32>(
            ny.x * tws.y + nz.x * tws.z,
            nx.y * tws.x + nz.y * tws.z,
            nx.x * tws.x + ny.y * tws.y,
        );
        n = normalize(mix(n, normalize(gn + rock_perturb * strength * 1.3), rock_w));
    }
    pbr_input.N = n;

    // Wet ground darkens + tightens near the water line.
    let wet = smoothstep(ter.params.z + 1.5, ter.params.z + 0.2, wp.y);
    pbr_input.material.base_color =
        vec4<f32>(pbr_input.material.base_color.rgb * (1.0 - wet * 0.35), 1.0);
    pbr_input.material.perceptual_roughness =
        mix(pbr_input.material.perceptual_roughness, 0.30, wet * 0.7);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
