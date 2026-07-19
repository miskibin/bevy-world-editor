// Calm-lake water — ExtendedMaterial<StandardMaterial, WaterExtension>.
//
// Recipe distilled from three.js Water/Water2 + GPU Gems ch.1 (see the water research
// note in the repo docs): NO reflection camera, NO refraction pass, NO textures —
//   body   = Beer–Lambert depth absorption (baked per-vertex depth, UV1.x)
//   mirror = analytic sky sampled along reflect(V, N), mixed by Schlick fresnel F0=0.02
//   N      = sum of 4 analytic Gerstner-derivative normals (fragment only, no displace)
//   glint  = tight Blinn-Phong sun highlight (exponent ~600)
//   foam   = baked shore distance (UV1.y) + scrolling value noise
//   edge   = alpha fade over the first ~25 cm of depth (soft waterline)

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::main_pass_post_lighting_processing,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::{globals, view},
}

struct WaterParams {
    // xyz = direction TO the sun (normalised), w unused
    sun_dir: vec4<f32>,
    // rgb = sun glint colour (linear, premultiplied by intensity), w = glint exponent
    sun_glint: vec4<f32>,
    // rgb = sky zenith colour (linear), w unused
    sky_zenith: vec4<f32>,
    // rgb = sky horizon colour (linear), w unused
    sky_horizon: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterParams;

fn w_hash(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn w_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let a = w_hash(i);
    let b = w_hash(i + vec2<f32>(1.0, 0.0));
    let c = w_hash(i + vec2<f32>(0.0, 1.0));
    let d = w_hash(i + vec2<f32>(1.0, 1.0));
    let u = f * f * (3.0 - 2.0 * f);
    return mix(a, b, u.x) + (c - a) * u.y * (1.0 - u.x) + (d - b) * u.x * u.y;
}

// Analytic Gerstner normal (GPU Gems eq.12), 4 waves, calm-lake tuning:
// A/L ≈ 0.012, Q low, directions spread ~100°, speeds 0.4–0.9 u/s.
fn wave_normal(xz: vec2<f32>, t: f32) -> vec3<f32> {
    var nx = 0.0;
    var nz = 0.0;
    var ny_sub = 0.0;
    // (dir.x, dir.y, wavelength, amplitude)
    let waves = array<vec4<f32>, 4>(
        vec4<f32>(1.0, 0.12, 7.3, 0.055),
        vec4<f32>(0.42, 0.91, 4.1, 0.038),
        vec4<f32>(-0.75, 0.66, 2.6, 0.025),
        vec4<f32>(-0.24, -0.97, 1.7, 0.016),
    );
    let speeds = vec4<f32>(0.85, 0.62, 0.50, 0.38);
    let q = 0.25;
    for (var i = 0; i < 4; i++) {
        let wv = waves[i];
        let d = normalize(wv.xy);
        let w = 6.28318 / wv.z;
        let wa = w * wv.w;
        let phase = w * dot(d, xz) + w * speeds[i] * t;
        let s = sin(phase);
        let c = cos(phase);
        nx += d.x * wa * c;
        nz += d.y * wa * c;
        ny_sub += q * wa * s;
    }
    return normalize(vec3<f32>(-nx, 1.0 - ny_sub, -nz));
}

fn sky_color(dir: vec3<f32>) -> vec3<f32> {
    let h = clamp(dir.y, 0.0, 1.0);
    let s = h * h * (3.0 - 2.0 * h);
    // Slight horizon-side darkening so the grazing mirror doesn't blow out.
    return mix(water.sky_horizon.rgb * 0.9, water.sky_zenith.rgb, s);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let wp = in.world_position.xyz;
    var depth = 0.2;
    var shore = 10.0;
#ifdef VERTEX_UVS_B
    depth = in.uv_b.x;  // metres of water under this fragment (baked)
    shore = in.uv_b.y;  // metres to the nearest dry cell (baked)
#endif

    var n = wave_normal(wp.xz, globals.time);
    // Incoherent micro-ripple: two slow noise fetches break the parallel-wave banding
    // that a pure 4-wave sum shows on large calm sheets.
    let r1 = w_noise(wp.xz * 0.63 + vec2<f32>(globals.time * 0.05, 0.0)) - 0.5;
    let r2 = w_noise(wp.xz * 0.91 - vec2<f32>(0.0, globals.time * 0.04)) - 0.5;
    n = normalize(n + vec3<f32>(r1, 0.0, r2) * 0.10);
    let v = normalize(view.world_position.xyz - wp);
    let theta = max(dot(n, v), 0.0);
    let fresnel = 0.02 + 0.98 * pow(1.0 - theta, 5.0);

    // Body: per-channel Beer–Lambert toward a dark blue deep.
    let shallow = vec3<f32>(0.10, 0.38, 0.40);
    let deep = vec3<f32>(0.015, 0.06, 0.10);
    let absorb = exp(-depth * vec3<f32>(0.55, 0.18, 0.12));
    let body = mix(deep, shallow, absorb);

    // Damped mirror: a lake also reflects dark terrain/trees we can't raytrace, so a
    // pure-sky reflection reads too bright — pull it down.
    let refl = sky_color(reflect(-v, n)) * 0.72;
    var col = mix(body, refl, fresnel);

    // Sun glint — tight warm highlight, only while the sun is up.
    let hvec = normalize(v + water.sun_dir.xyz);
    col += water.sun_glint.rgb
        * pow(max(dot(n, hvec), 0.0), water.sun_glint.w)
        * max(water.sun_dir.y, 0.0);

    // Shore foam: contact line driven by CONTINUOUS depth (per-vertex-interpolated, so
    // it hugs the true waterline) + a noisy lapping band from shore distance.
    let contact = (1.0 - smoothstep(0.0, 0.5, depth)) * 0.55;
    let band = 1.0 - smoothstep(0.2, 2.0, shore);
    let fn1 = w_noise(wp.xz * 1.9 + vec2<f32>(globals.time * 0.06, globals.time * 0.045));
    let foam = clamp(contact + band * smoothstep(0.55, 0.95, fn1) * 0.5, 0.0, 1.0);
    col = mix(col, vec3<f32>(0.85, 0.90, 0.92), foam * 0.75);

    // Soft waterline: fully transparent right at the shore, opaque past ~30 cm depth.
    let alpha = smoothstep(0.015, 0.30, depth) * 0.94;

    var out: FragmentOutput;
    out.color = vec4<f32>(col, alpha);
    // Distance fog etc. still applies — far lakes haze out like the terrain does.
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
