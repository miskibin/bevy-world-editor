//! Free fly camera: hold RMB to mouse-look; WASD + QE vertical; scroll wheel sets speed;
//! Shift boosts ×4; MMB-drag pans screen-space; F frames the terrain under the cursor.
//! Spawned by `sky.rs` (the camera entity carries `FlyCam`). Look deltas are per-frame and
//! deliberately NOT dt-scaled (that makes look framerate-dependent + laggy); movement IS
//! dt-scaled. No smoothing/easing on either — editors want crisp, direct input.

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

#[derive(Component)]
pub struct FlyCam {
    pub yaw: f32,
    pub pitch: f32,
    /// Live fly speed (m/s) — scroll adjusts it, the Camera panel sets it directly.
    pub speed: f32,
    /// Radians of look per pixel of mouse motion.
    pub sensitivity: f32,
    pub invert_y: bool,
}

impl FlyCam {
    pub fn new(yaw: f32, pitch: f32) -> Self {
        FlyCam { yaw, pitch, speed: 40.0, sensitivity: 0.0024, invert_y: false }
    }
}

pub struct FlyCamPlugin;

impl Plugin for FlyCamPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, fly);
    }
}

#[allow(clippy::too_many_arguments)]
fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    cap: Res<crate::ui::UiInputCapture>,
    editor: Option<Res<crate::editor::EditorState>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    cam: Single<(&mut Transform, &mut FlyCam)>,
    // Latched drags: a look/pan begun off the UI keeps going even if the (now hidden,
    // grabbed) pointer would otherwise read as "over egui".
    mut looking: Local<bool>,
    mut panning: Local<bool>,
) {
    let (mut tf, mut fc) = cam.into_inner();

    // Wheel ownership (mirrors editor.rs brush_shortcuts): RMB-hold or no active tool keeps
    // the wheel on fly speed; otherwise an active brush claims it for its radius.
    let tool_active =
        editor.as_deref().map(|e| e.tool != crate::editor::Tool::Off).unwrap_or(false);
    let camera_owns_wheel = buttons.pressed(MouseButton::Right) || !tool_active;
    if cap.pointer || !camera_owns_wheel {
        wheel.clear();
    } else {
        for w in wheel.read() {
            fc.speed = (fc.speed * (1.0 + w.y * 0.12)).clamp(2.0, 400.0);
        }
    }

    // F = frame: snap the camera onto the terrain under the cursor (or the map centre) at a
    // sensible vantage — the universal editor "focus" key. Gated on keyboard capture so it
    // doesn't fire while typing in a panel field.
    if !cap.keyboard && keys.just_pressed(KeyCode::KeyF) {
        let target = editor.as_deref().and_then(|e| e.cursor_hit).unwrap_or(Vec3::ZERO);
        let eye = target + Vec3::new(0.0, 0.55, 1.0).normalize() * 60.0;
        *tf = Transform::from_translation(eye).looking_at(target, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
        return;
    }

    // Only START a look/pan drag when the pointer isn't over egui; keep it once started.
    if buttons.just_pressed(MouseButton::Right) && !cap.pointer {
        *looking = true;
    }
    if buttons.just_pressed(MouseButton::Middle) && !cap.pointer {
        *panning = true;
    }
    if buttons.just_released(MouseButton::Right) {
        *looking = false;
    }
    if buttons.just_released(MouseButton::Middle) {
        *panning = false;
    }
    // Grab/hide the cursor for the duration of either drag; release the instant both end.
    let grabbing = *looking || *panning;
    let want_grab = if grabbing { CursorGrabMode::Locked } else { CursorGrabMode::None };
    if cursor.grab_mode != want_grab {
        cursor.grab_mode = want_grab;
        cursor.visible = !grabbing;
    }

    if *looking || *panning {
        let inv = if fc.invert_y { -1.0 } else { 1.0 };
        let right = tf.right().as_vec3();
        let up = tf.up().as_vec3();
        // Pan speed scales with fly speed so it stays usable whether zoomed in or way out.
        let pan = fc.speed * 0.0018;
        for m in motion.read() {
            if *looking {
                fc.yaw -= m.delta.x * fc.sensitivity;
                fc.pitch =
                    (fc.pitch - m.delta.y * fc.sensitivity * inv).clamp(-1.54, 1.54);
            } else {
                // Screen-space pan: drag grabs the world and slides it under the cursor.
                tf.translation += -right * m.delta.x * pan + up * m.delta.y * pan;
            }
        }
    } else {
        motion.clear();
    }
    tf.rotation = Quat::from_euler(EulerRot::YXZ, fc.yaw, fc.pitch, 0.0);

    // Suppress WASD/QE flight while egui is listening for keyboard input (typing in a field).
    if cap.keyboard {
        return;
    }
    let mut dir = Vec3::ZERO;
    let fwd = tf.forward().as_vec3();
    let right = tf.right().as_vec3();
    if keys.pressed(KeyCode::KeyW) {
        dir += fwd;
    }
    if keys.pressed(KeyCode::KeyS) {
        dir -= fwd;
    }
    if keys.pressed(KeyCode::KeyD) {
        dir += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        dir -= right;
    }
    if keys.pressed(KeyCode::KeyE) || keys.pressed(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if keys.pressed(KeyCode::KeyQ) || keys.pressed(KeyCode::ControlLeft) {
        dir -= Vec3::Y;
    }
    if dir != Vec3::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) { 4.0 } else { 1.0 };
        tf.translation += dir.normalize() * fc.speed * boost * time.delta_secs();
    }
}
