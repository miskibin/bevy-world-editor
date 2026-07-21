// Ambient particles — pollen motes, fireflies, falling leaves, rain, snow.
//
// One static mesh of quads per emitter; ALL motion lives here in the vertex shader
// (the wind lesson: never touch a material or transform per frame). Each particle's
// mesh position is a seed inside a wrap box; we add a per-mode drift as a function of
// `globals.time`, wrap into the box, and centre the box on the camera — so the field
// follows the fly-cam forever with zero CPU work and one draw call per emitter.
//
// Colours are scene-referred (multiplied by view.exposure) so particles hold up against
// the physically-bright day (EV ~11.7) and the opened-up night exposure alike.

#import bevy_pbr::mesh_view_bindings::{globals, view}

struct ParticleParams {
    // x = mode (0 pollen, 1 firefly, 2 leaf, 3 rain, 4 snow)
    // y = strength 0..1 (0 disables — quads collapse to zero alpha)
    // z = box extent (m, horizontal), w = fall/drift speed (m/s)
    a: vec4<f32>,
    // xyz = wind drift (m/s), w = base particle half-size (m)
    b: vec4<f32>,
    // rgb = tint (scene-referred luminance, pre-exposure), w = brightness boost
    tint: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> P: ParticleParams;

struct Vertex {
    @location(0) position: vec3<f32>, // seed in [0, ext)^3
    @location(2) uv: vec2<f32>,       // quad corner (0/1, 0/1)
    @location(5) color: vec4<f32>,    // per-particle randoms: phase, size, hue, blink
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) tint: vec3<f32>,
    @location(3) mode: f32,
}

const TAU: f32 = 6.28318530718;

@vertex
fn vertex(v: Vertex) -> VsOut {
    var out: VsOut;
    let mode = i32(P.a.x + 0.5);
    let t = globals.time;
    let phase = v.color.x;
    let rsize = v.color.y;
    let rhue = v.color.z;
    let rblink = v.color.w;

    let ext = P.a.z;
    // Vertical extent: rain/snow need headroom above the camera; fireflies hug a band.
    var ext_y = ext * 0.6;
    if (mode == 1) { ext_y = 14.0; }
    if (mode >= 3) { ext_y = ext * 0.8; }
    let box = vec3<f32>(ext, ext_y, ext);

    // Per-mode drift. Everything is a pure function of time + per-particle randoms.
    var drift = vec3<f32>(0.0);
    if (mode == 0) {
        // Pollen: slow wind ride + lazy swirl.
        drift = P.b.xyz * t * 0.35
            + vec3<f32>(sin(t * 0.31 + phase * TAU), sin(t * 0.23 + phase * 9.0) * 0.4,
                        cos(t * 0.27 + phase * TAU)) * 1.4;
    } else if (mode == 1) {
        // Firefly: wandering figure-eights.
        drift = vec3<f32>(sin(t * 0.43 + phase * TAU) * 2.6,
                          sin(t * 0.61 + phase * 5.0) * 0.9,
                          cos(t * 0.37 + phase * TAU) * 2.6);
    } else if (mode == 2) {
        // Leaf: steady fall + pendulum sway riding the wind.
        drift = P.b.xyz * t * 0.6 - vec3<f32>(0.0, P.a.w * t, 0.0)
            + vec3<f32>(sin(t * 1.7 + phase * TAU), 0.0, cos(t * 1.3 + phase * TAU)) * 0.7;
    } else if (mode == 3) {
        // Rain: fast fall with a touch of wind shear.
        drift = P.b.xyz * t * 0.5 - vec3<f32>(0.0, P.a.w * (0.85 + 0.3 * rsize) * t, 0.0);
    } else {
        // Snow: slow fall, wide flutter.
        drift = P.b.xyz * t * 0.8 - vec3<f32>(0.0, P.a.w * (0.7 + 0.6 * rsize) * t, 0.0)
            + vec3<f32>(sin(t * 0.9 + phase * TAU), 0.0, cos(t * 0.7 + phase * TAU)) * 1.1;
    }

    let cam = view.world_position;
    let wrapped = fract((v.position + drift) / box) * box;
    // Centre the box on the camera; fireflies sit lower (they live near the ground).
    var centre_off = vec3<f32>(-0.5 * box.x, -0.5 * box.y, -0.5 * box.z);
    if (mode == 1) { centre_off.y = -10.0; }
    let base = cam + centre_off + wrapped;

    // Hide the wrap borders: fade to zero alpha near each face of the box.
    let q = wrapped / box; // 0..1 per axis
    let edge = smoothstep(0.0, 0.12, q.x) * smoothstep(1.0, 0.88, q.x)
        * smoothstep(0.0, 0.12, q.z) * smoothstep(1.0, 0.88, q.z)
        * smoothstep(0.0, 0.10, q.y) * smoothstep(1.0, 0.90, q.y);

    var brightness = 1.0;
    if (mode == 1) {
        // Blink: mostly dark, periodic soft glows, desynchronised per particle.
        let s = 0.5 + 0.5 * sin(t * (1.1 + rblink * 0.9) + phase * TAU);
        brightness = pow(s, 6.0);
    }

    // Build the quad corner.
    let corner = v.uv - vec2<f32>(0.5);
    let size = P.b.w * (0.65 + 0.7 * rsize);
    var world = base;
    if (mode == 3) {
        // Rain streak: thin vertical quad in world space.
        let right = normalize(vec3<f32>(view.world_from_view[0].x, 0.0, view.world_from_view[0].z));
        world += right * corner.x * 0.02 + vec3<f32>(0.0, corner.y * size * 22.0, 0.0);
    } else if (mode == 2) {
        // Leaf: billboard spinning in the view plane.
        let ang = t * (1.2 + rblink * 1.5) + phase * TAU;
        let c = cos(ang); let s = sin(ang);
        let rot = vec2<f32>(corner.x * c - corner.y * s, corner.x * s + corner.y * c);
        world += view.world_from_view[0].xyz * rot.x * size
            + view.world_from_view[1].xyz * rot.y * size;
    } else {
        world += view.world_from_view[0].xyz * corner.x * size
            + view.world_from_view[1].xyz * corner.y * size;
    }

    out.clip = view.clip_from_world * vec4<f32>(world, 1.0);
    out.uv = v.uv;
    out.alpha = edge * brightness * P.a.y;
    // Tint: leaves spread across green→amber; everything else takes the flat tint.
    var tint = P.tint.rgb;
    if (mode == 2) {
        tint = mix(vec3<f32>(0.35, 0.42, 0.12), vec3<f32>(0.55, 0.30, 0.08), rhue) * P.tint.r;
    }
    out.tint = tint * P.tint.w;
    out.mode = P.a.x;
    return out;
}

@fragment
fn fragment(in: VsOut) -> @location(0) vec4<f32> {
    let mode = i32(in.mode + 0.5);
    let p = in.uv - vec2<f32>(0.5);
    let d = length(p) * 2.0;
    var alpha = in.alpha;
    var col = in.tint;
    if (mode == 3) {
        // Streak: soft across, faded ends.
        alpha *= (1.0 - smoothstep(0.3, 1.0, abs(p.x) * 2.0))
            * (1.0 - smoothstep(0.55, 1.0, abs(p.y) * 2.0));
    } else if (mode == 2) {
        // Leaf: irregular rounded card.
        let wobble = 0.85 + 0.15 * sin(atan2(p.y, p.x) * 3.0);
        alpha *= 1.0 - smoothstep(wobble * 0.7, wobble, d);
    } else if (mode == 1) {
        // Firefly: hot core + halo.
        alpha *= exp(-d * d * 3.0);
        col *= 1.0 + 3.0 * exp(-d * d * 14.0);
    } else {
        // Soft disc (pollen, snow).
        alpha *= 1.0 - smoothstep(0.25, 1.0, d);
    }
    // Scene-referred: survive the day exposure, glow correctly at night.
    return vec4<f32>(col * view.exposure, alpha);
}
