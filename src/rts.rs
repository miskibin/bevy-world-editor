//! RTS camera: an orbiting, ground-anchored view for playing on a map rather than flying
//! through one. Toggle with **F3**, or boot straight into it with `WED_RTS=1`.
//!
//! **It drives the same camera entity as the fly-cam** — never a second `Camera3d`. Half
//! this codebase reaches for the camera with `Single<&Camera3d>`/`.single()` (the post
//! passes, DoF focus, godrays, atmospherics, stats), and a second one makes every one of
//! those queries ambiguous, so the real camera silently stops being driven. The two
//! controllers cooperate instead: each early-outs unless [`CamMode`] names it.
//!
//! The camera is defined by a **ground focus point** plus orbit (yaw / pitch / distance).
//! Panning moves the focus across the map and the eye follows, which is what makes a
//! ground-relative view: at any zoom the camera holds the same height over the terrain
//! under it, so climbing onto a mesa doesn't leave you staring at rock.
//!
//! Pitch is **coupled to zoom** — near the ground the view flattens toward eye level so
//! silhouettes read, and zoomed out it tips toward a map view. That coupling is what
//! separates an RTS camera from an orbit camera with a pan button; without it you spend
//! the whole time re-aiming after every zoom.

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;

use crate::flycam::FlyCam;
use crate::genrun::{GeneratedWorld, world_offset};

/// Which controller owns the camera transform this frame.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CamMode {
    #[default]
    Fly,
    Rts,
}

impl CamMode {
    pub fn is_rts(self) -> bool {
        self == CamMode::Rts
    }
}

/// Zoom limits in metres from focus to eye. The near end is close enough to read a palm's
/// fronds; the far end is where a 512 m map fits comfortably on screen.
const DIST_MIN: f32 = 22.0;
const DIST_MAX: f32 = 130.0;
/// Pitch (radians below horizontal) at each end of the zoom range.
const PITCH_NEAR: f32 = 0.52; // ~30° — reads silhouettes
const PITCH_FAR: f32 = 0.92; // ~53° — reads layout, without going near-top-down
/// Ground pan speed in metres/second at the closest zoom; scaled up as you zoom out so a
/// keypress always crosses roughly the same fraction of the screen.
const PAN_SPEED: f32 = 26.0;
/// How far from the window edge the mouse starts pushing the view, in pixels.
const EDGE_PX: f32 = 6.0;

#[derive(Component)]
pub struct RtsCam {
    /// The point on the ground the camera looks at, world space.
    pub focus: Vec3,
    pub yaw: f32,
    pub dist: f32,
}

impl Default for RtsCam {
    fn default() -> Self {
        RtsCam { focus: Vec3::ZERO, yaw: 0.6, dist: 62.0 }
    }
}

pub struct RtsPlugin;

impl Plugin for RtsPlugin {
    fn build(&self, app: &mut App) {
        let boot = if std::env::var("WED_RTS").is_ok() { CamMode::Rts } else { CamMode::Fly };
        app.insert_resource(boot)
            // `attach` runs in Update, NOT Startup: the camera entity is spawned by
            // `sky::setup_camera` in Startup too, and with no ordering between them this
            // ran first, attached nothing, and left the RTS mode with no camera to drive —
            // while the fly-cam had already stood down for it. The view then sat frozen at
            // the boot pose and looked, misleadingly, like a camera that simply ignored its
            // inputs.
            .add_systems(Update, (attach, toggle, apply_preset, centre_on_new_world, drive).chain());
    }
}

fn profiling() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("WED_PROFILE").is_ok())
}

fn attach(mut commands: Commands, cam: Query<Entity, (With<FlyCam>, Without<RtsCam>)>) {
    for e in &cam {
        commands.entity(e).insert(RtsCam::default());
    }
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut mode: ResMut<CamMode>) {
    if keys.just_pressed(KeyCode::F3) {
        *mode = if mode.is_rts() { CamMode::Fly } else { CamMode::Rts };
        info!("camera mode: {:?}", *mode);
    }
}

/// Shadow-cascade reach for each mode, metres. The fly-cam can be a kilometre up looking
/// at the horizon; the RTS camera physically cannot see past a few hundred metres, and
/// every metre of unused cascade range is resolution thrown away — shorter is both faster
/// AND sharper here, which is rare enough to be worth stating.
const SHADOW_FAR_FLY: f32 = 700.0;
const SHADOW_FAR_RTS: f32 = 280.0;

/// Graphics settings that follow the camera mode.
///
/// The RTS view changes what is worth paying for, so the preset is not a quality drop —
/// it is a re-allocation. Depth of field and god rays are cinematic effects aimed at a
/// camera standing in the scene; from a map view they mostly blur the thing you are
/// looking at. Supersampling drops to native because alpha-cutout foliage edges — the
/// reason it was raised — are a few pixels across at this distance. What the budget buys
/// instead is the shadow work that actually reads from above.
///
/// `WED_RTS_FULLGFX=1` disables the coupling for A/B comparisons.
#[allow(clippy::too_many_arguments)]
fn apply_preset(
    mode: Res<CamMode>,
    mut gfx: ResMut<crate::ui::GfxSettings>,
    mut rays: ResMut<crate::godrays::GodRaySettings>,
    mut atmo: ResMut<crate::atmospherics::AtmoSettings>,
    mut shadow_map: ResMut<bevy::light::DirectionalLightShadowMap>,
    mut dof: Query<&mut crate::dof::Dof>,
    mut fog: Query<&mut DistanceFog, With<Camera3d>>,
    mut cascades: Query<&mut bevy::light::CascadeShadowConfig, With<DirectionalLight>>,
    mut applied: Local<Option<CamMode>>,
) {
    if *applied == Some(*mode) || std::env::var("WED_RTS_FULLGFX").is_ok() {
        return;
    }
    *applied = Some(*mode);
    let rts = mode.is_rts();

    gfx.ssaa = if rts { 1.0 } else { 1.35 };
    rays.enabled = !rts;

    // Haze, dialled well back for the map view.
    //
    // The cinematic settings are tuned for a camera standing IN the landscape looking at a
    // kilometre of it, where aerial perspective is the depth cue. An RTS camera looks at a
    // few hundred metres of a 512 m map, so the same haze just lays a beige veil over the
    // thing you are trying to read — and the further half of the map is permanently milky.
    // Visibility more than doubles and the post-pass drops to a third.
    gfx.visibility = if rts { 3600.0 } else { 1400.0 };
    atmo.strength = if rts { 0.35 } else { 1.0 };
    for mut f in &mut fog {
        f.falloff = FogFalloff::from_visibility_colors(
            gfx.visibility,
            Color::srgb(0.42, 0.48, 0.55),
            Color::srgb(0.68, 0.76, 0.88),
        );
    }
    // Shadow map stays at 4096 in BOTH modes. Dropping it to 2048 looked like a free
    // saving and was not: the cascades still span hundreds of metres, so halving the
    // resolution quadrupled the texel footprint and the shadow edges crawled and flickered
    // as the camera panned. The cascade DISTANCE below is the real saving here, and it
    // makes shadows sharper rather than blurrier.
    shadow_map.size = 4096;
    for mut d in &mut dof {
        d.max_radius = if rts { 0.0 } else { 2.5 };
    }
    let far = if rts { SHADOW_FAR_RTS } else { SHADOW_FAR_FLY };
    for mut c in &mut cascades {
        // Three cascades instead of four: the range they now cover is less than half as
        // deep, so a fourth split buys nothing and costs a whole extra shadow pass over
        // the canopy — the classic foliage tax.
        *c = bevy::light::CascadeShadowConfigBuilder {
            num_cascades: if rts { 3 } else { 4 },
            maximum_distance: far,
            first_cascade_far_bound: if rts { 24.0 } else { 40.0 },
            ..default()
        }
        .build();
    }
    info!("graphics preset: {}", if rts { "RTS" } else { "cinematic" });
}

/// Park the focus at the middle of a freshly generated map, so switching to RTS after a
/// regenerate doesn't leave it anchored wherever the previous map's centre happened to be.
fn centre_on_new_world(
    world: Option<Res<GeneratedWorld>>,
    mut cam: Query<&mut RtsCam>,
) {
    let Some(world) = world else { return };
    if !world.is_changed() {
        return;
    }
    let hf = &world.0.height;
    let off = world_offset(hf);
    // `WED_RTS_CAM="fx,fz,dist[,yaw_deg]"` stages a repeatable view: focus at map-extent
    // fractions, plus zoom and heading. The screenshot harness has no way to pan or zoom,
    // so without this every capture frames whatever happens to be at the map centre.
    let staged = std::env::var("WED_RTS_CAM").ok().and_then(|s| {
        let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        (v.len() >= 3).then(|| v)
    });
    let ext = hf.extent();
    let (fx, fz, dist, yaw) = match &staged {
        Some(v) => (v[0], v[1], v[2], v.get(3).copied().unwrap_or(35.0).to_radians()),
        None => (0.5, 0.5, 62.0, 0.6),
    };
    let (mx, mz) = (ext * fx, ext * fz);
    for mut rts in &mut cam {
        rts.focus = Vec3::new(mx + off, hf.sample_world(mx, mz), mz + off);
        rts.dist = dist.clamp(DIST_MIN, DIST_MAX);
        rts.yaw = yaw;
    }
}

/// Terrain height at a world XZ, or the current focus height if there's no world yet.
fn ground_at(world: &GeneratedWorld, x: f32, z: f32) -> f32 {
    let hf = &world.0.height;
    let off = world_offset(hf);
    hf.sample_world(x - off, z - off)
}

#[allow(clippy::too_many_arguments)]
fn drive(
    mode: Res<CamMode>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    windows: Query<&Window>,
    world: Option<Res<GeneratedWorld>>,
    mut cam: Query<(&mut Transform, &mut RtsCam)>,
) {
    // Under the profiler the harness owns the camera transform (it flies scripted poses),
    // but the MODE still matters — it selects the graphics preset being measured. So the
    // controller stands down while the preset stays applied.
    if !mode.is_rts() || profiling() {
        // Drop any queued input so a mode switch doesn't apply a frame of stale motion.
        motion.clear();
        wheel.clear();
        return;
    }
    let Ok((mut tf, mut rts)) = cam.single_mut() else { return };

    for w in wheel.read() {
        // Multiplicative zoom: a notch covers the same *proportion* of the range at every
        // distance, so zooming feels the same close up and far out.
        rts.dist = (rts.dist * (1.0 - w.y * 0.10)).clamp(DIST_MIN, DIST_MAX);
    }
    // Middle-drag orbits. Deliberately NOT right-drag: right-click is the universal RTS
    // command button and stealing it would make this camera useless in an actual game.
    if buttons.pressed(MouseButton::Middle) {
        for m in motion.read() {
            rts.yaw -= m.delta.x * 0.005;
        }
    } else {
        motion.clear();
    }
    if keys.pressed(KeyCode::KeyQ) {
        rts.yaw += 1.4 * time.delta_secs();
    }
    if keys.pressed(KeyCode::KeyE) {
        rts.yaw -= 1.4 * time.delta_secs();
    }

    // Pan, in the camera's ground plane so W always goes "up the screen".
    let (sin, cos) = rts.yaw.sin_cos();
    let fwd = Vec3::new(-sin, 0.0, -cos);
    let right = Vec3::new(cos, 0.0, -sin);
    let mut pan = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        pan += fwd;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        pan -= fwd;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        pan += right;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        pan -= right;
    }
    // Edge scrolling — only when the cursor is genuinely inside the window, so the view
    // doesn't drift while you are working in another app.
    if let Ok(win) = windows.single() {
        if let Some(c) = win.cursor_position() {
            let (w, h) = (win.width(), win.height());
            if c.x < EDGE_PX {
                pan -= right;
            } else if c.x > w - EDGE_PX {
                pan += right;
            }
            if c.y < EDGE_PX {
                pan += fwd;
            } else if c.y > h - EDGE_PX {
                pan -= fwd;
            }
        }
    }
    if pan != Vec3::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) { 2.5 } else { 1.0 };
        // Zoomed-out panning covers more ground per second — otherwise crossing the map
        // from a map-scale view takes as long as walking it.
        let scale = rts.dist / DIST_MIN;
        rts.focus += pan.normalize() * PAN_SPEED * scale * boost * time.delta_secs();
    }

    if let Some(world) = &world {
        let hf = &world.0.height;
        let off = world_offset(hf);
        let ext = hf.extent();
        // Keep the focus on the map, with a margin so the view never looks off the edge.
        let lo = off + 12.0;
        let hi = off + ext - 12.0;
        rts.focus.x = rts.focus.x.clamp(lo, hi);
        rts.focus.z = rts.focus.z.clamp(lo, hi);
        // Follow the terrain, smoothed: snapping the focus to every bump makes the whole
        // view jitter as you pan across broken ground.
        let target_y = ground_at(world, rts.focus.x, rts.focus.z);
        rts.focus.y += (target_y - rts.focus.y) * (1.0 - (-6.0 * time.delta_secs()).exp());
    }

    let t = ((rts.dist - DIST_MIN) / (DIST_MAX - DIST_MIN)).clamp(0.0, 1.0);
    let pitch = PITCH_NEAR + (PITCH_FAR - PITCH_NEAR) * t;
    let (ys, yc) = rts.yaw.sin_cos();
    let eye = rts.focus
        + Vec3::new(ys * pitch.cos(), pitch.sin(), yc * pitch.cos()) * rts.dist;
    tf.translation = eye;
    tf.look_at(rts.focus, Vec3::Y);
}
