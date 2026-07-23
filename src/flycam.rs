//! Free fly camera: hold RMB to mouse-look; WASD + QE vertical; scroll wheel sets speed;
//! Shift boosts ×4. Spawned by `sky.rs` (the camera entity carries `FlyCam`).

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

#[derive(Component)]
pub struct FlyCam {
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
}

impl FlyCam {
    pub fn new(yaw: f32, pitch: f32) -> Self {
        FlyCam { yaw, pitch, speed: 40.0 }
    }
}

pub struct FlyCamPlugin;

impl Plugin for FlyCamPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, fly);
    }
}

fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    cap: Res<crate::ui::UiInputCapture>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    cam: Single<(&mut Transform, &mut FlyCam)>,
    // Latched look-drag: a look begun off the UI keeps going even if the (now hidden,
    // grabbed) pointer would otherwise read as "over egui".
    mut looking: Local<bool>,
) {
    let (mut tf, mut fc) = cam.into_inner();

    // Scroll sets speed, unless egui is consuming the pointer (scrolling a panel etc.).
    if cap.pointer {
        wheel.clear();
    } else {
        for w in wheel.read() {
            fc.speed = (fc.speed * (1.0 + w.y * 0.12)).clamp(2.0, 400.0);
        }
    }

    // Only START a look-drag when the pointer isn't over egui; keep it once started.
    if buttons.just_pressed(MouseButton::Right) && !cap.pointer {
        *looking = true;
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
    if buttons.just_released(MouseButton::Right) {
        if *looking {
            cursor.grab_mode = CursorGrabMode::None;
            cursor.visible = true;
        }
        *looking = false;
    }
    if *looking {
        for m in motion.read() {
            fc.yaw -= m.delta.x * 0.0024;
            fc.pitch = (fc.pitch - m.delta.y * 0.0024).clamp(-1.54, 1.54);
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
