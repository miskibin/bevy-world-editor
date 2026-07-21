//! Day/night cycle — the "alive" foundation. One clock `t ∈ [0,1)` (0 = midnight,
//! 0.5 = noon) sweeps the sun; everything else derives from its elevation: light
//! colour/intensity, moonlight, ambient + IBL levels, fog/haze colours (atmospherics
//! reads the live `DistanceFog`, so it follows for free), the water shader's sun, and a
//! star dome that fades in after dusk.
//!
//! CPU discipline (the wind lesson): the water/star MATERIALS are only touched when the
//! clock has moved a meaningful step — never every frame.

use bevy::prelude::*;

use crate::sky::{Moon, Sun};

#[derive(Resource)]
pub struct DayClock {
    /// 0 = midnight, 0.25 = sunrise, 0.5 = noon, 0.75 = sunset.
    pub t: f32,
    /// Seconds of real time for a full day; 0 = paused.
    pub day_secs: f32,
    /// Last t at which the expensive (material-touching) updates ran.
    applied_t: f32,
}

impl Default for DayClock {
    fn default() -> Self {
        DayClock {
            // Start mid-morning; screenshots freeze the clock for determinism.
            // WED_TIME=0..1 (0.5 = noon) stages a time of day for a shot.
            t: std::env::var("WED_TIME")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| v.rem_euclid(1.0))
                .unwrap_or(0.38),
            day_secs: if std::env::var("WED_SHOT").is_ok() || std::env::var("WED_CLIP").is_ok()
            {
                0.0
            } else {
                900.0 // 15-minute days by default
            },
            applied_t: -1.0,
        }
    }
}

/// Direction TO the sun for a clock value (tilted orbit so noon isn't dead overhead).
pub fn sun_dir(t: f32) -> Vec3 {
    let ang = (t - 0.25) * std::f32::consts::TAU;
    Vec3::new(ang.cos() * 0.95, ang.sin(), 0.38).normalize()
}

/// Marker: the star dome entity.
#[derive(Component)]
pub struct StarDome;

#[derive(Resource)]
struct StarMat(Handle<StandardMaterial>);

pub struct DayCyclePlugin;

impl Plugin for DayCyclePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DayClock>()
            .add_systems(Startup, spawn_stars)
            .add_systems(Update, advance_clock);
    }
}

fn spawn_stars(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    // ~700 tiny emissive quads on a big dome, rotating slowly. One mesh, one material.
    use bevy::mesh::PrimitiveTopology;
    let mut rng = worldgen::rng::Rng::new(0x57A125);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let r = 4200.0f32;
    for _ in 0..700 {
        // Uniform-ish on the upper hemisphere (reject low stars near the horizon haze).
        let az = rng.range(0.0, std::f32::consts::TAU);
        let el = rng.range(0.08, 1.45);
        let dir = Vec3::new(el.cos() * az.cos(), el.sin(), el.cos() * az.sin());
        let c = dir * r;
        let s = rng.range(2.2, 6.5); // star quad half-size at 4.2 km — subpixel-ish points
        let (u, v) = (dir.cross(Vec3::Y).normalize_or_zero() * s, {
            let u = dir.cross(Vec3::Y).normalize_or_zero();
            u.cross(dir) * s
        });
        let brightness = rng.range(0.35, 1.0);
        // Slight colour spread: most white, some warm, some blue.
        let tint = match rng.next_u32() % 5 {
            0 => [1.0, 0.85, 0.7],
            1 => [0.75, 0.85, 1.0],
            _ => [1.0, 1.0, 1.0],
        };
        let base = positions.len() as u32;
        for p in [c - u - v, c + u - v, c + u + v, c - u + v] {
            positions.push(p.to_array());
            normals.push((-dir).to_array());
            colors.push([tint[0] * brightness, tint[1] * brightness, tint[2] * brightness, 1.0]);
        }
        let _ = base;
    }
    let n = positions.len();
    let mut indices = Vec::with_capacity(n / 4 * 6);
    for q in 0..(n as u32 / 4) {
        let b = q * 4;
        indices.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(bevy::mesh::Indices::U32(indices));

    let mat = mats.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0), // driven by the clock
        emissive: LinearRgba::new(9.0, 9.0, 10.0, 1.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        // 4.2 km away — DistanceFog would erase every star.
        fog_enabled: false,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(mat.clone()),
        Transform::default(),
        StarDome,
        bevy::light::NotShadowCaster,
    ));
    commands.insert_resource(StarMat(mat));
}

#[allow(clippy::too_many_arguments)]
fn advance_clock(
    time: Res<Time>,
    mut clock: ResMut<DayClock>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), (With<Sun>, Without<Moon>)>,
    mut moon: Query<(&mut Transform, &mut DirectionalLight), (With<Moon>, Without<Sun>)>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut fog: Query<&mut bevy::pbr::DistanceFog>,
    mut env: Query<&mut EnvironmentMapLight>,
    mut exposure: Query<&mut bevy::camera::Exposure>,
    gfx: Res<crate::ui::GfxSettings>,
    dim: Res<crate::weather::WeatherDim>,
    mut stars: Query<&mut Transform, (With<StarDome>, Without<Sun>, Without<Moon>)>,
    mut clear: ResMut<ClearColor>,
    star_mat: Option<Res<StarMat>>,
    mut water: ResMut<Assets<crate::water_mat::WaterMaterial>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    lake: Option<Res<crate::water_mat::LakeMaterial>>,
) {
    if clock.day_secs > 0.0 {
        clock.t = (clock.t + time.delta_secs() / clock.day_secs).rem_euclid(1.0);
    }
    let t = clock.t;
    let dir = sun_dir(t);
    let elev = dir.y; // -1..1
    let daylight = (elev * 4.0).clamp(0.0, 1.0); // full brightness once the sun clears 15°
    let twilight = ((elev + 0.14) * 7.0).clamp(0.0, 1.0); // glow starts before sunrise
    let night = 1.0 - twilight;
    // Warmth peaks at the horizon.
    let warm = (1.0 - elev.abs() * 3.0).clamp(0.0, 1.0);
    // 0 = clear skies, 1 = full precip (dim.sun bottoms out at 0.25 in heavy rain).
    let overcast = ((1.0 - dim.sun) / 0.75).clamp(0.0, 1.0);

    // ── Sun ──────────────────────────────────────────────────────────────────────
    if let Ok((mut tf, mut light)) = sun.single_mut() {
        *tf = Transform::from_translation(dir * 100.0).looking_at(Vec3::ZERO, Vec3::Y);
        light.illuminance = (32_000.0 * daylight + 900.0 * twilight) * dim.sun;
        let day_col = Vec3::new(1.0, 0.96, 0.88);
        let horizon_col = Vec3::new(1.0, 0.55, 0.28);
        let c = day_col.lerp(horizon_col, warm);
        light.color = Color::srgb(c.x, c.y, c.z);
        // Overcast kills the hard sun shadows well before the light is fully dimmed.
        light.shadow_maps_enabled = twilight > 0.05 && overcast < 0.55;
    }
    // ── Moon: opposite the sun, cool and dim ────────────────────────────────────
    if let Ok((mut tf, mut light)) = moon.single_mut() {
        let mdir = sun_dir(t + 0.5);
        *tf = Transform::from_translation(mdir * 100.0).looking_at(Vec3::ZERO, Vec3::Y);
        let moon_up = (mdir.y * 4.0).clamp(0.0, 1.0);
        // Kept LOW: Bevy's procedural Atmosphere shades the sky from EVERY directional
        // light, so a bright "moon" reads as a grey daylight sky at midnight.
        light.illuminance = 12.0 * moon_up * night;
        light.shadow_maps_enabled = night > 0.6 && moon_up > 0.2;
    }
    // ── Exposure: the camera opens up after dark (EV 11.7 is a daylight stop — held
    // fixed it renders any physically-dim night to black). ~4.5 stops over the night.
    for mut e in &mut exposure {
        e.ev100 = gfx.ev100 - 4.5 * night;
    }
    // ── Ambient + IBL ────────────────────────────────────────────────────────────
    ambient.brightness = (40.0 + 300.0 * twilight) * dim.ambient;
    let amb_day = Vec3::new(0.85, 0.92, 1.0);
    let amb_night = Vec3::new(0.45, 0.55, 0.85);
    let a = amb_night.lerp(amb_day, twilight);
    ambient.color = Color::srgb(a.x, a.y, a.z);
    for mut e in &mut env {
        e.intensity = (60.0 + 1440.0 * twilight * twilight) * dim.ambient;
    }
    // ── Fog colours (atmospherics inherits these live) ──────────────────────────
    for mut f in &mut fog {
        let day = Vec3::new(0.80, 0.76, 0.64);
        let dusk = Vec3::new(0.85, 0.48, 0.30);
        let night_c = Vec3::new(0.05, 0.07, 0.13);
        let mut c = if twilight > 0.0 {
            day.lerp(dusk, warm).lerp(night_c, night)
        } else {
            night_c
        };
        // Precip greys the haze out.
        c = c.lerp(Vec3::splat(0.55) * twilight.max(0.08), overcast * 0.8);
        f.color = Color::srgba(c.x, c.y, c.z, 1.0);
        let g_day = Vec3::new(1.0, 0.95, 0.85);
        let g_dusk = Vec3::new(1.0, 0.55, 0.25);
        let g = g_day.lerp(g_dusk, warm) * twilight.max(0.05);
        f.directional_light_color = Color::srgba(g.x, g.y, g.z, 0.6);
        // Weather thickens the fog: visibility scales by the eased fog_mul. The UI's
        // on-change write uses the same formula, so the two never fight.
        if gfx.fog {
            f.falloff = bevy::pbr::FogFalloff::from_visibility_colors(
                gfx.visibility * dim.fog_mul,
                Color::srgb(0.42, 0.48, 0.55),
                Color::srgb(0.68, 0.76, 0.88),
            );
        }
    }
    // ── Backdrop: the procedural Atmosphere goes transparent once the sun sets, so
    // whatever ClearColor shows through IS the night sky. Fade it day-blue → near-black.
    {
        let day_c = Vec3::new(0.62, 0.75, 0.92).lerp(Vec3::splat(0.62), overcast * 0.8);
        let night_c = Vec3::new(0.010, 0.014, 0.030);
        let c = night_c.lerp(day_c, twilight);
        clear.0 = Color::srgb(c.x, c.y, c.z);
    }
    // ── Star dome: slow rotation, fade in after dusk ────────────────────────────
    for mut tf in &mut stars {
        tf.rotation = Quat::from_rotation_y(t * std::f32::consts::TAU * 0.6);
    }

    // ── Quantized material updates (never per-frame — the wind lesson) ──────────
    let step = 1.0 / 512.0;
    if (t - clock.applied_t).abs() < step && clock.applied_t >= 0.0 {
        return;
    }
    clock.applied_t = t;
    if let Some(sm) = star_mat {
        if let Some(mut m) = std_mats.get_mut(&sm.0) {
            let vis = (night - 0.25).clamp(0.0, 1.0) * 1.33;
            m.base_color = Color::srgba(1.0, 1.0, 1.0, vis);
        }
    }
    if let Some(lake) = lake {
        if let Some(mut m) = water.get_mut(&lake.0) {
            m.extension.params.sun_dir = dir.max(Vec3::new(-1.0, 0.02, -1.0)).extend(0.0);
            let glint = Vec3::new(0.98, 0.95, 0.72).lerp(Vec3::new(1.0, 0.5, 0.25), warm)
                * twilight.max(0.08);
            m.extension.params.sun_glint = glint.extend(600.0);
            // Sky reflection follows the mood.
            let zen_day = Vec3::new(0.12, 0.24, 0.52);
            let zen_night = Vec3::new(0.015, 0.02, 0.05);
            let z = zen_night.lerp(zen_day, twilight);
            m.extension.params.sky_zenith = z.extend(0.0);
            let hor_day = Vec3::new(0.40, 0.48, 0.56);
            let hor_dusk = Vec3::new(0.75, 0.42, 0.25);
            let h = hor_day.lerp(hor_dusk, warm).lerp(Vec3::new(0.04, 0.05, 0.09), night);
            m.extension.params.sky_horizon = h.extend(0.0);
        }
    }
}
