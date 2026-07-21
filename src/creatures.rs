//! Living creatures — bird flocks over the canopy, deer herds in the clearings,
//! butterflies over the meadow flowers. Spawn sites come from
//! `worldgen::creature_sites` (deterministic map analysis); behaviour is cheap CPU
//! steering over a few dozen root entities, with animation (wing flaps, leg swings,
//! neck dips) applied to child part entities. Creatures further than `ANIM_RANGE`
//! from the camera skip their per-frame work entirely.

use bevy::prelude::*;

use crate::creature_mesh;
use crate::daycycle::{DayClock, sun_dir};
use crate::flycam::FlyCam;
use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};

const ANIM_RANGE: f32 = 260.0;

pub struct CreaturesPlugin;

impl Plugin for CreaturesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Flocks>().add_systems(
            Update,
            (spawn_on_ready, drive_birds, drive_deer, drive_butterflies, day_gate),
        );
        if std::env::var("WED_CREATURELINE").is_ok() {
            app.add_systems(Update, stage_creatureline);
        }
    }
}

#[derive(Resource)]
struct CreatureAssets {
    mat: Handle<StandardMaterial>,
    deer_body: Handle<Mesh>,
    deer_head: [Handle<Mesh>; 2], // doe, buck
    deer_leg: Handle<Mesh>,
    bird_body: Handle<Mesh>,
    bird_wing: Handle<Mesh>,
    fly_body: Handle<Mesh>,
    fly_wing: [Handle<Mesh>; 3],
}

// ── Components ─────────────────────────────────────────────────────────────────────

struct FlockState {
    centre: Vec3,
    target: Vec3,
}

#[derive(Resource, Default)]
struct Flocks(Vec<FlockState>);

#[derive(Component)]
struct Bird {
    flock: usize,
    phase: f32,
    radius: f32,
    bob: f32,
    wings: [Entity; 2],
}

#[derive(Component)]
struct DayCreature; // hidden after dusk (birds roost, butterflies land)

#[derive(Clone, Copy, PartialEq)]
enum DeerState {
    Graze,
    Amble,
    Flee,
}

#[derive(Component)]
struct Deer {
    state: DeerState,
    timer: f32,
    yaw: f32,
    home: Vec2, // map-space anchor it drifts around
    phase: f32,
    head: Entity,
    legs: [Entity; 4],
    /// Eased head-dip weight (1 = nose in the grass).
    dip: f32,
}

#[derive(Component)]
struct Butterfly {
    home: Vec3,
    phase: f32,
    wings: [Entity; 2],
}

// ── Spawning ───────────────────────────────────────────────────────────────────────

fn spawn_on_ready(
    world: Option<Res<GeneratedWorld>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut flocks: ResMut<Flocks>,
    assets: Option<Res<CreatureAssets>>,
) {
    let Some(world) = world else { return };
    if !world.is_changed() {
        return;
    }
    // Part meshes + the one shared vertex-colour material, built once.
    let assets = match assets {
        Some(a) => a.clone_inner(),
        None => {
            let a = CreatureAssets {
                mat: mats.add(StandardMaterial {
                    perceptual_roughness: 0.9,
                    reflectance: 0.1,
                    ..default()
                }),
                deer_body: meshes.add(creature_mesh::deer_body().to_mesh()),
                deer_head: [
                    meshes.add(creature_mesh::deer_head(false).to_mesh()),
                    meshes.add(creature_mesh::deer_head(true).to_mesh()),
                ],
                deer_leg: meshes.add(creature_mesh::deer_leg().to_mesh()),
                bird_body: meshes.add(creature_mesh::bird_body().to_mesh()),
                bird_wing: meshes.add(creature_mesh::bird_wing().to_mesh()),
                fly_body: meshes.add(creature_mesh::butterfly_body().to_mesh()),
                fly_wing: [
                    meshes.add(creature_mesh::butterfly_wing(0).to_mesh()),
                    meshes.add(creature_mesh::butterfly_wing(1).to_mesh()),
                    meshes.add(creature_mesh::butterfly_wing(2).to_mesh()),
                ],
            };
            let inner = a.clone_inner();
            commands.insert_resource(a);
            inner
        }
    };

    let hf = &world.0.height;
    let off = world_offset(hf);
    let ext = hf.extent();
    let mut rng = worldgen::rng::Rng::new(0xC4EA);
    let sites = worldgen::creature_sites(&world.0);

    // ── Bird flocks: anywhere over the map, high above the terrain. ──
    // WED_CREATURELINE pins flock 0 over the staged meadow (a shot can frame it).
    let pin = std::env::var("WED_CREATURELINE").ok().and_then(|_| {
        sites
            .iter()
            .find(|s| matches!(s.kind, worldgen::SiteKind::Meadow))
            .map(|s| (s.x, s.z))
    });
    flocks.0.clear();
    for f in 0..4 {
        let (mx, mz) = match (f, pin) {
            (0, Some(p)) => p,
            _ => (rng.range(ext * 0.2, ext * 0.8), rng.range(ext * 0.2, ext * 0.8)),
        };
        let y = hf.sample_world(mx, mz) + rng.range(35.0, 55.0);
        let centre = Vec3::new(mx + off, y, mz + off);
        flocks.0.push(FlockState { centre, target: centre });
        for _ in 0..7 {
            let wings = [
                spawn_part(&mut commands, &assets, assets.bird_wing.clone()),
                spawn_part(&mut commands, &assets, assets.bird_wing.clone()),
            ];
            let bird = commands
                .spawn((
                    Mesh3d(assets.bird_body.clone()),
                    MeshMaterial3d(assets.mat.clone()),
                    Transform::from_translation(centre),
                    WorldEntity,
                    DayCreature,
                    Bird {
                        flock: f,
                        phase: rng.range(0.0, std::f32::consts::TAU),
                        radius: rng.range(14.0, 30.0),
                        bob: rng.range(0.0, std::f32::consts::TAU),
                        wings,
                    },
                ))
                .id();
            commands.entity(bird).add_children(&wings);
        }
    }

    // ── Deer herds on meadow / forest-floor sites. ──
    let mut herd_sites: Vec<_> = sites
        .iter()
        .filter(|s| !matches!(s.kind, worldgen::SiteKind::LakeShore))
        .collect();
    // Spread herds out: shuffle deterministically, take a handful.
    for i in (1..herd_sites.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        herd_sites.swap(i, j);
    }
    // Model review: WED_CREATURELINE forces the first herd onto the first meadow so the
    // staged camera has something to look at.
    if std::env::var("WED_CREATURELINE").is_ok() {
        if let (Some(first_meadow), Some(slot)) = (
            sites.iter().find(|s| matches!(s.kind, worldgen::SiteKind::Meadow)),
            herd_sites.first_mut(),
        ) {
            *slot = first_meadow;
        }
    }
    for site in herd_sites.iter().take(8) {
        let n = 3 + (rng.next_u32() % 3) as usize;
        for k in 0..n {
            let mx = site.x + rng.range(-6.0, 6.0);
            let mz = site.z + rng.range(-6.0, 6.0);
            let y = hf.sample_world(mx, mz);
            let head_mesh = assets.deer_head[usize::from(k == 0)].clone(); // one buck per herd
            let head = spawn_part(&mut commands, &assets, head_mesh);
            let legs = [
                spawn_part(&mut commands, &assets, assets.deer_leg.clone()),
                spawn_part(&mut commands, &assets, assets.deer_leg.clone()),
                spawn_part(&mut commands, &assets, assets.deer_leg.clone()),
                spawn_part(&mut commands, &assets, assets.deer_leg.clone()),
            ];
            let scale = rng.range(0.82, 1.05);
            let deer = commands
                .spawn((
                    Mesh3d(assets.deer_body.clone()),
                    MeshMaterial3d(assets.mat.clone()),
                    Transform::from_xyz(mx + off, y, mz + off)
                        .with_scale(Vec3::splat(scale)),
                    WorldEntity,
                    Deer {
                        state: DeerState::Graze,
                        timer: rng.range(1.0, 6.0),
                        yaw: rng.range(0.0, std::f32::consts::TAU),
                        home: Vec2::new(site.x, site.z),
                        phase: rng.range(0.0, std::f32::consts::TAU),
                        head,
                        legs,
                        dip: 1.0,
                    },
                ))
                .id();
            commands.entity(deer).add_children(&legs);
            commands.entity(deer).add_child(head);
        }
    }

    // ── Butterflies over meadows. ──
    let meadows: Vec<_> = sites
        .iter()
        .filter(|s| matches!(s.kind, worldgen::SiteKind::Meadow))
        .collect();
    for site in meadows.iter().take(20) {
        for _ in 0..4 {
            let mx = site.x + rng.range(-8.0, 8.0);
            let mz = site.z + rng.range(-8.0, 8.0);
            let y = hf.sample_world(mx, mz) + rng.range(0.5, 1.4);
            let v = rng.next_u32();
            let wings = [
                spawn_part(&mut commands, &assets, assets.fly_wing[(v % 3) as usize].clone()),
                spawn_part(&mut commands, &assets, assets.fly_wing[(v % 3) as usize].clone()),
            ];
            let fly = commands
                .spawn((
                    Mesh3d(assets.fly_body.clone()),
                    MeshMaterial3d(assets.mat.clone()),
                    Transform::from_xyz(mx + off, y, mz + off),
                    WorldEntity,
                    DayCreature,
                    Butterfly {
                        home: Vec3::new(mx + off, y, mz + off),
                        phase: rng.range(0.0, std::f32::consts::TAU),
                        wings,
                    },
                ))
                .id();
            commands.entity(fly).add_children(&wings);
        }
    }
    // WED_CREATURELINE: also park STATIC review models (bird + 3 butterflies) at the
    // staged meadow centre so the framing camera has ground truth that cannot wander.
    if std::env::var("WED_CREATURELINE").is_ok() {
        if let Some(site) = sites.iter().find(|s| matches!(s.kind, worldgen::SiteKind::Meadow)) {
            let y = hf.sample_world(site.x, site.z);
            let c = Vec3::new(site.x + off, y, site.z + off);
            for (i, wing_v) in [0u32, 1, 2].iter().enumerate() {
                let p = c + Vec3::new(i as f32 * 0.8 - 0.8, 1.2, 0.0);
                commands.spawn((
                    Mesh3d(assets.fly_body.clone()),
                    MeshMaterial3d(assets.mat.clone()),
                    Transform::from_translation(p),
                    WorldEntity,
                ));
                for side in [-1.0f32, 1.0] {
                    commands.spawn((
                        Mesh3d(assets.fly_wing[*wing_v as usize].clone()),
                        MeshMaterial3d(assets.mat.clone()),
                        Transform::from_translation(p).with_rotation(
                            Quat::from_rotation_y(-side * std::f32::consts::FRAC_PI_2)
                                * Quat::from_rotation_z(side * 0.35),
                        ),
                        WorldEntity,
                    ));
                }
            }
            let bp = c + Vec3::new(0.8, 1.6, 0.9);
            commands.spawn((
                Mesh3d(assets.bird_body.clone()),
                MeshMaterial3d(assets.mat.clone()),
                Transform::from_translation(bp),
                WorldEntity,
            ));
            for side in [-1.0f32, 1.0] {
                commands.spawn((
                    Mesh3d(assets.bird_wing.clone()),
                    MeshMaterial3d(assets.mat.clone()),
                    Transform::from_translation(bp + Vec3::new(0.02, 0.03, 0.04 * side))
                        .with_rotation(
                            Quat::from_rotation_y(-side * std::f32::consts::FRAC_PI_2)
                                * Quat::from_rotation_z(side * 0.2),
                        ),
                    WorldEntity,
                ));
            }
        }
    }
    info!(
        "creatures: {} sites -> 4 flocks, {} herd sites, {} meadows",
        sites.len(),
        herd_sites.len().min(8),
        meadows.len().min(20)
    );
    // Staging aid: world-space coords to frame a shot at.
    if let Some(s) = herd_sites.first() {
        info!("deer herd at world ({:.0}, {:.0})", s.x + off, s.z + off);
    }
    if let Some(s) = meadows.first() {
        info!("butterflies at world ({:.0}, {:.0})", s.x + off, s.z + off);
    }
}

impl CreatureAssets {
    fn clone_inner(&self) -> CreatureAssets {
        CreatureAssets {
            mat: self.mat.clone(),
            deer_body: self.deer_body.clone(),
            deer_head: self.deer_head.clone(),
            deer_leg: self.deer_leg.clone(),
            bird_body: self.bird_body.clone(),
            bird_wing: self.bird_wing.clone(),
            fly_body: self.fly_body.clone(),
            fly_wing: self.fly_wing.clone(),
        }
    }
}

fn spawn_part(commands: &mut Commands, assets: &CreatureAssets, mesh: Handle<Mesh>) -> Entity {
    commands
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(assets.mat.clone()),
            Transform::default(),
            WorldEntity,
        ))
        .id()
}

/// `WED_CREATURELINE=1` — park one deer (buck), one bird and one butterfly in a lit
/// row at the first meadow site and aim the camera at them (model review, like
/// WED_LODLINE). They still animate in place via the normal drive systems.
fn stage_creatureline(
    world: Option<Res<GeneratedWorld>>,
    sites_done: Query<(), With<Deer>>,
    mut cam: Query<(&mut Transform, &mut FlyCam)>,
    mut done: Local<bool>,
) {
    if *done || sites_done.is_empty() {
        return;
    }
    // An explicit WED_EYE/WED_CAM wins — creatureline then only pins the models.
    if std::env::var("WED_EYE").is_ok() || std::env::var("WED_CAM").is_ok() {
        *done = true;
        return;
    }
    let Some(world) = world else { return };
    let hf = &world.0.height;
    let off = world_offset(hf);
    let sites = worldgen::creature_sites(&world.0);
    let Some(site) = sites.iter().find(|s| matches!(s.kind, worldgen::SiteKind::Meadow))
    else {
        return;
    };
    let y = hf.sample_world(site.x, site.z);
    let centre = Vec3::new(site.x + off, y + 1.0, site.z + off);
    let eye = centre + Vec3::new(-6.0, 1.6, -6.0);
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }
    info!("creatureline staged at world ({:.0}, {:.0})", centre.x, centre.z);
    *done = true;
}

// ── Behaviour ──────────────────────────────────────────────────────────────────────

fn cam_pos(cam: &Query<&Transform, With<FlyCam>>) -> Vec3 {
    cam.single().map(|t| t.translation).unwrap_or(Vec3::ZERO)
}

fn drive_birds(
    time: Res<Time>,
    world: Option<Res<GeneratedWorld>>,
    mut flocks: ResMut<Flocks>,
    mut birds: Query<(&Bird, &mut Transform), Without<FlyCam>>,
    mut parts: Query<&mut Transform, (Without<Bird>, Without<FlyCam>)>,
    cam: Query<&Transform, With<FlyCam>>,
) {
    let Some(world) = world else { return };
    let hf = &world.0.height;
    let off = world_offset(hf);
    let ext = hf.extent();
    let t = time.elapsed_secs();
    let dt = time.delta_secs();
    let camp = cam_pos(&cam);

    let pinned = std::env::var("WED_CREATURELINE").is_ok();
    // Flock anchors drift between waypoints.
    for (i, f) in flocks.0.iter_mut().enumerate() {
        if pinned && i == 0 {
            continue;
        }
        if f.centre.distance(f.target) < 12.0 {
            // Cheap deterministic-ish retarget from time + index.
            let mut rng = worldgen::rng::Rng::new((t * 977.0) as u32 ^ (i as u32 * 7919));
            let mx = rng.range(ext * 0.15, ext * 0.85);
            let mz = rng.range(ext * 0.15, ext * 0.85);
            let y = hf.sample_world(mx, mz) + rng.range(32.0, 58.0);
            f.target = Vec3::new(mx + off, y, mz + off);
        }
        let dir = (f.target - f.centre).normalize_or_zero();
        f.centre += dir * 6.5 * dt;
    }

    for (bird, mut tf) in &mut birds {
        let f = &flocks.0[bird.flock];
        if f.centre.distance(camp) > 700.0 {
            continue; // too far to matter
        }
        // Circle the anchor; angular speed inversely with radius (constant airspeed).
        let w = 9.0 / bird.radius;
        let ang = t * w + bird.phase;
        let (s, c) = ang.sin_cos();
        let pos = f.centre
            + Vec3::new(c * bird.radius, (t * 0.7 + bird.bob).sin() * 2.2, s * bird.radius);
        // Velocity direction = tangent.
        let vel = Vec3::new(-s, 0.0, c);
        let yaw = f32::atan2(-vel.z, vel.x);
        tf.translation = pos;
        // Bank into the turn.
        tf.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-0.35);
        // Flap: glide (slow shallow) most of the time, burst on the bob's rising edge.
        let flap = (t * 8.0 + bird.phase * 3.0).sin() * 0.55;
        for (side, w_ent) in [(1.0f32, bird.wings[0]), (-1.0, bird.wings[1])] {
            if let Ok(mut wtf) = parts.get_mut(w_ent) {
                wtf.translation = Vec3::new(0.02, 0.03, 0.04 * side);
                wtf.rotation = Quat::from_rotation_y(-side * std::f32::consts::FRAC_PI_2)
                    * Quat::from_rotation_z(side * flap);
            }
        }
    }
}

fn drive_deer(
    time: Res<Time>,
    world: Option<Res<GeneratedWorld>>,
    mut deer: Query<(&mut Deer, &mut Transform), Without<FlyCam>>,
    mut parts: Query<&mut Transform, (Without<Deer>, Without<FlyCam>)>,
    cam: Query<&Transform, With<FlyCam>>,
) {
    let Some(world) = world else { return };
    let hf = &world.0.height;
    let off = world_offset(hf);
    let t = time.elapsed_secs();
    let dt = time.delta_secs();
    let camp = cam_pos(&cam);

    for (mut d, mut tf) in &mut deer {
        let dist_cam = tf.translation.distance(camp);
        if dist_cam > ANIM_RANGE {
            continue;
        }
        // Startle: flee straight away from the camera.
        if dist_cam < 15.0 && d.state != DeerState::Flee {
            d.state = DeerState::Flee;
            d.timer = 4.0;
            let away = (tf.translation - camp).with_y(0.0).normalize_or_zero();
            d.yaw = f32::atan2(-away.z, away.x);
        }
        d.timer -= dt;
        if d.timer <= 0.0 {
            // Cycle behaviours; drift back toward home so herds don't scatter forever.
            let mut rng = worldgen::rng::Rng::new((t * 613.0) as u32 ^ d.phase.to_bits());
            d.state = if matches!(d.state, DeerState::Graze) {
                DeerState::Amble
            } else {
                DeerState::Graze
            };
            d.timer = rng.range(2.5, 8.0);
            if matches!(d.state, DeerState::Amble) {
                let here = Vec2::new(tf.translation.x - off, tf.translation.z - off);
                let to_home = d.home - here;
                let head_home = to_home.length() > 24.0;
                d.yaw = if head_home {
                    f32::atan2(-to_home.y, to_home.x)
                } else {
                    rng.range(0.0, std::f32::consts::TAU)
                };
            }
        }
        let speed = match d.state {
            DeerState::Graze => 0.0,
            DeerState::Amble => 0.85,
            DeerState::Flee => 4.2,
        };
        if speed > 0.0 {
            let dir = Vec3::new(d.yaw.cos(), 0.0, -d.yaw.sin());
            let next = tf.translation + dir * speed * dt;
            let mx = next.x - off;
            let mz = next.z - off;
            // Stay on the map and out of the lakes.
            let ext = hf.extent();
            let dry = mx > 8.0 && mz > 8.0 && mx < ext - 8.0 && mz < ext - 8.0 && {
                let size = hf.size;
                let ix = (mx as usize).min(size - 1);
                let iz = (mz as usize).min(size - 1);
                !world.0.water[iz * size + ix].is_finite()
            };
            if dry {
                tf.translation = next;
                tf.translation.y = hf.sample_world(mx, mz);
            } else {
                d.yaw += 1.7; // bounce off the shoreline
            }
        }
        tf.rotation = Quat::from_rotation_y(d.yaw);

        // Head: nose down while grazing, up and alert otherwise (eased).
        let want_dip = if matches!(d.state, DeerState::Graze) { 1.0 } else { 0.0 };
        d.dip += (want_dip - d.dip) * (dt * 3.0).min(1.0);
        if let Ok(mut htf) = parts.get_mut(d.head) {
            htf.translation = Vec3::from_array(creature_mesh::DEER_NECK);
            htf.rotation = Quat::from_rotation_z(-1.35 * d.dip);
        }
        // Legs: diagonal-pair walk swing, amplitude with speed.
        let amp = 0.55 * (speed / 4.2).clamp(0.0, 1.0) + 0.25 * (speed > 0.0) as u32 as f32;
        let rate = if matches!(d.state, DeerState::Flee) { 11.0 } else { 5.0 };
        for (i, leg) in d.legs.iter().enumerate() {
            if let Ok(mut ltf) = parts.get_mut(*leg) {
                let pair = if i == 0 || i == 3 { 0.0 } else { std::f32::consts::PI };
                ltf.translation = Vec3::from_array(creature_mesh::DEER_HIPS[i]);
                ltf.rotation =
                    Quat::from_rotation_z((t * rate + pair + d.phase).sin() * amp);
            }
        }
    }
}

fn drive_butterflies(
    time: Res<Time>,
    mut flies: Query<(&Butterfly, &mut Transform), Without<FlyCam>>,
    mut parts: Query<&mut Transform, (Without<Butterfly>, Without<FlyCam>)>,
    cam: Query<&Transform, With<FlyCam>>,
) {
    let t = time.elapsed_secs();
    let camp = cam_pos(&cam);
    for (fly, mut tf) in &mut flies {
        if fly.home.distance(camp) > 160.0 {
            continue;
        }
        let p = fly.phase;
        // Lissajous wander around the home flower patch + jittery bob.
        let pos = fly.home
            + Vec3::new(
                (t * 0.53 + p).sin() * 3.2 + (t * 1.7 + p * 2.0).sin() * 0.5,
                (t * 1.1 + p).sin() * 0.5 + (t * 3.9 + p).sin() * 0.14,
                (t * 0.61 + p * 1.3).cos() * 3.2 + (t * 1.9 + p).cos() * 0.5,
            );
        let vel = pos - tf.translation;
        if vel.length_squared() > 1e-6 {
            let yaw = f32::atan2(-vel.z, vel.x);
            tf.rotation = Quat::from_rotation_y(yaw);
        }
        tf.translation = pos;
        let flap = (t * 11.0 + p * 5.0).sin() * 1.1;
        for (side, w_ent) in [(1.0f32, fly.wings[0]), (-1.0, fly.wings[1])] {
            if let Ok(mut wtf) = parts.get_mut(w_ent) {
                wtf.translation = Vec3::new(-0.01, 0.005, 0.0);
                wtf.rotation = Quat::from_rotation_y(-side * std::f32::consts::FRAC_PI_2)
                    * Quat::from_rotation_z(side * flap);
            }
        }
    }
}

/// Birds roost and butterflies land after dusk — flip visibility on the twilight edge.
fn day_gate(
    clock: Res<DayClock>,
    mut q: Query<&mut Visibility, With<DayCreature>>,
    mut was_day: Local<Option<bool>>,
) {
    let day = sun_dir(clock.t).y > -0.02;
    if *was_day == Some(day) {
        return;
    }
    *was_day = Some(day);
    for mut v in &mut q {
        *v = if day { Visibility::Inherited } else { Visibility::Hidden };
    }
}
