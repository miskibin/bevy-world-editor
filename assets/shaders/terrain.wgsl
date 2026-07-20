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
    mesh_view_bindings::view,
}

struct TerrainParams {
    // x = planar UV scale (1/m), y = second-scale factor, z = water level (world y), w = normal strength
    params: vec4<f32>,
    // x = micro-relief strength, y = cavity-AO strength, z = quality (0 fast / 1 full), w spare
    params2: vec4<f32>,
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

// Micro-relief height field (Warbell recipe): three rotated-lattice octaves at clump
// scales — drives BOTH the bump normal tilt and the cavity AO so albedo and relief agree.
fn terrain_h(p: vec2<f32>) -> f32 {
    return t_noise_rot(p * 0.35, 0.946, 0.326) * 0.50
        + t_noise_rot(p * 0.90, 0.682, 0.731) * 0.35
        + t_noise_rot(p * 2.20, 0.292, 0.956) * 0.22;
}

// Fallen-twig field (Warbell port, simplified): nearest bent/tapered stick in the 3x3
// cell neighbourhood; returns (coverage, tone-along-stick).
fn twig_field(wp: vec2<f32>) -> vec2<f32> {
    let warp = (vec2<f32>(t_noise(wp * 0.8 + 8.0), t_noise(wp * 0.8 + 30.0)) - 0.5) * 1.7;
    let q = wp + warp * 0.10;
    let cell = 1.25;
    let ci = floor(q / cell);
    var best_d = 1e9;
    var best_h = 0.0;
    var best_seed = 0.0;
    for (var dy = -1; dy <= 1; dy += 1) {
        for (var dx = -1; dx <= 1; dx += 1) {
            let c = ci + vec2<f32>(f32(dx), f32(dy));
            let r1 = t_hash(c);
            if (r1 < 0.45) {
                let r2 = t_hash(c + 17.3);
                let r3 = t_hash(c + 41.7);
                let r4 = t_hash(c + 71.1);
                let center = (c + vec2<f32>(r2, r3)) * cell;
                let ang = r4 * 6.28318;
                let dir = vec2<f32>(cos(ang), sin(ang));
                let len = (0.20 + r1 * 0.55) * cell;
                let a = center - dir * len * 0.5;
                let ba = dir * len;
                let pa = q - a;
                let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-5), 0.0, 1.0);
                let d = length(pa - ba * h);
                if (d < best_d) {
                    best_d = d;
                    best_h = h;
                    best_seed = r2;
                }
            }
        }
    }
    let w = 0.045 * (0.30 + 0.70 * (1.0 - pow(abs(best_h * 2.0 - 1.0), 1.7)));
    let edge = 0.008 + 0.014 * t_noise(wp * 9.0);
    let cov = 1.0 - smoothstep(w, w + edge, best_d);
    let tone = t_noise(vec2<f32>(best_h * 4.0, best_seed * 13.0));
    return vec2<f32>(cov, tone);
}

// Planar (world-XZ) layer sample at two scales, blended by a large-scale noise mask so
// the texture repeat never registers.
fn sample_planar(layer: i32, wp: vec3<f32>) -> vec4<f32> {
    let s1 = ter.params.x;
    let a = textureSample(albedo_arr, albedo_samp, wp.xz * s1, layer);
    // Quality lane (uniform branch — coherent skip, Warbell pattern): the de-tiling
    // second scale + mask only on full quality.
    if ter.params2.z < 0.5 {
        return a;
    }
    let s2 = ter.params.x * ter.params.y;
    let m = patch_noise(wp.xz * 0.021);
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
    // UV0.x carries trail wear 0..1 (0 on non-terrain meshes like rocks).
    var moisture = 0.5;
    var flow = 0.0;
    var trail = 0.0;
#ifdef VERTEX_UVS_B
    moisture = in.uv_b.x;
    flow = in.uv_b.y;
#endif
#ifdef VERTEX_UVS_A
    trail = in.uv.x;
#endif

    let hq = ter.params2.z > 0.5;

    // ── Layer weights ────────────────────────────────────────────────────────────
    let slope = 1.0 - gn.y; // 0 flat … 1 vertical
    let boundary = patch_noise(wp.xz * 0.055) - 0.5; // wobbles every threshold organically

    // Rock: steep faces. Threshold wobbles so the grass/rock line isn't a contour.
    let rock_w = smoothstep(0.16 + boundary * 0.06, 0.30 + boundary * 0.06, slope);
    // Dirt: gully floors (high flow) + dry patches; suppressed on rock.
    let dirt_flow = smoothstep(0.35, 0.75, flow);
    let dirt_dry = smoothstep(0.25, 0.05, moisture) * smoothstep(0.5, 0.8, patch_noise(wp.xz * 0.013));
    let dirt_w = clamp(dirt_flow + dirt_dry, 0.0, 1.0) * (1.0 - rock_w);
    // Forest floor: moist, sheltered ground in noise-broken patches. Threshold kept low —
    // the litter/moss layer IS the visible forest floor, so it should cover most woods.
    let ff_w = smoothstep(0.28 + boundary * 0.12, 0.58, moisture) * (1.0 - rock_w) * (1.0 - dirt_w);
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
        if hq {
            let rx = textureSample(albedo_arr, albedo_samp, wp.zy * rs, 2);
            let ry = textureSample(albedo_arr, albedo_samp, wp.xz * rs, 2);
            let rz = textureSample(albedo_arr, albedo_samp, wp.xy * rs, 2);
            albedo += (rx * tws.x + ry * tws.y + rz * tws.z) * rock_w;
        } else {
            albedo += textureSample(albedo_arr, albedo_samp, wp.xz * rs, 2) * rock_w;
        }
    }

    // Trail lane: worn earth punches through every layer (dirt sample, slightly dusty),
    // with a ragged noise-broken edge so the path never reads as a painted stripe.
    if trail > 0.01 {
        let ragged = trail * (0.80 + 0.4 * patch_noise(wp.xz * 0.9));
        let lane = smoothstep(0.18, 0.62, ragged);
        let dirt = sample_planar(3, wp);
        // Dusty but NOT flat: fine gravel grain re-sampled at ~0.8 m keeps the beaten
        // lane readable up close (a plain brightened dirt sample smeared into beige).
        let g =
            textureSampleBias(albedo_arr, albedo_samp, wp.xz * ter.params.x * 5.2 + 0.87, 3, -1.75)
                .rgb;
        let grain = 0.6 + 0.8 * dot(g, vec3<f32>(0.299, 0.587, 0.114));
        albedo = mix(albedo, dirt * vec4<f32>(1.10 * grain, 1.04 * grain, 0.94 * grain, 1.0), lane);
    }

    // Moss films on moist, sheltered grass (full quality only)
    if hq { — noise-broken so it patches, never coats.
    let moss = smoothstep(0.55, 0.85, moisture)
        * smoothstep(0.45, 0.75, patch_noise(wp.xz * 0.11))
        * (grass_w + ff_w)
        * (1.0 - trail);
    albedo = vec4<f32>(mix(albedo.rgb, vec3<f32>(0.16, 0.26, 0.10), moss * 0.55), 1.0);
    }

    // Fallen-twig litter baked into the forest floor (Warbell twig_field, simplified):
    // one bent, tapered stick per ~1.25 m cell, random position/angle per cell.
    if hq && ff_w > 0.05 {
        let tw = twig_field(wp.xz);
        let bark = vec3<f32>(0.26, 0.18, 0.11) * (0.75 + tw.y * 0.5);
        albedo = vec4<f32>(mix(albedo.rgb, bark, min(tw.x * ff_w * 1.2, 1.0)), 1.0);
    }

    // ── Near-camera detail pass — the cure for "mushy ground at your feet". The base
    // tiling (~4 m) is right at mid distance but has no high-frequency content up close,
    // so within ~15 m we overlay the SAME textures re-sampled at ~0.8 m tiling
    // (luminance-only overlay: no visible re-tiling, just crisp grain).
    let cam_d = length(view.world_position.xyz - wp);
    let near_w = 1.0 - smoothstep(5.0, 16.0, cam_d);
    if hq && near_w > 0.01 {
        let s3 = ter.params.x * 5.2; // ~0.8 m tiling
        // Negative mip bias: at a standing-height grazing angle the hardware picks
        // mush-tier mips even for this near overlay — bias it back toward sharp.
        let s_b = -1.75;
        let sd = textureSampleBias(albedo_arr, albedo_samp, wp.xz * s3, 0, s_b).rgb * grass_w
            + textureSampleBias(albedo_arr, albedo_samp, wp.xz * s3 + 0.31, 1, s_b).rgb * ff_w
            + textureSampleBias(albedo_arr, albedo_samp, wp.xz * s3 + 0.62, 2, s_b).rgb * rock_w
            + textureSampleBias(albedo_arr, albedo_samp, wp.xz * s3 + 0.87, 3, s_b).rgb * dirt_w;
        let lum = dot(sd, vec3<f32>(0.299, 0.587, 0.114));
        albedo = vec4<f32>(
            mix(albedo.rgb, albedo.rgb * (0.55 + 0.95 * lum), 0.8 * near_w),
            1.0,
        );
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
    // ── Micro-relief bump + cavity AO (Warbell's "fake 3D on flat ground") ───────
    // Finite-difference gradient of the clump-scale height field tilts the shading
    // normal; baked soft shadow in the SAME field's hollows carves depth that a tilted
    // normal alone can't give (ambient hits every direction). Top faces only; trails
    // flatten it (trodden ground is packed smooth).
    // Trails only SOFTEN the relief (0.35), never erase it — a fully-flattened wide
    // lane read as a featureless beige smear.
    let topw = smoothstep(0.35, 0.80, gn.y) * (1.0 - trail * 0.35);
    let relief = ter.params2.x;
    let cavity = ter.params2.y;
    if topw > 0.001 && relief > 0.01 {
        let e = 0.18;
        let hx = terrain_h(wp.xz + vec2<f32>(e, 0.0)) - terrain_h(wp.xz - vec2<f32>(e, 0.0));
        let hz = terrain_h(wp.xz + vec2<f32>(0.0, e)) - terrain_h(wp.xz - vec2<f32>(0.0, e));
        n = normalize(n + vec3<f32>(-hx, 0.0, -hz) * relief * topw);

        let h0 = terrain_h(wp.xz);
        let mound = smoothstep(0.10, 0.82, h0);
        var groove = 1.0;
        if hq {
            let crease = t_noise_rot(wp.xz * 1.7, 0.81, 0.59) * 0.34
                + t_noise_rot(wp.xz * 3.3, 0.60, 0.80) * 0.40
                + t_noise_rot(wp.xz * 6.7, 0.29, 0.957) * 0.26;
            groove = smoothstep(0.32, 0.70, crease);
        }
        let ao = mix(1.0 - 0.46 * cavity, 1.0, mound) * mix(1.0 - 0.30 * cavity, 1.0, groove);
        let crown = 1.0 + smoothstep(0.62, 1.0, h0) * 0.20 * cavity;
        let shade = mix(1.0, ao * crown, topw);
        pbr_input.material.base_color =
            vec4<f32>(pbr_input.material.base_color.rgb * shade, 1.0);
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
