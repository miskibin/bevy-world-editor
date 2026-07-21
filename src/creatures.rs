//! Living creatures — bird flocks over the canopy, deer herds in the clearings,
//! butterflies over the meadow flowers. Spawn sites come from
//! `worldgen::creature_sites` (deterministic map analysis); behaviour is cheap CPU
//! steering over a few dozen root entities, with animation (wing flaps, leg swings,
//! neck dips) applied to child part entities. Creatures further than `ANIM_RANGE`
//! from the camera skip their per-frame work entirely.

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
use bevy::prelude::*;

use crate::creature_mesh;
use crate::daycycle::{DayClock, sun_dir};
use crate::flycam::FlyCam;
use crate::genrun::{GeneratedWorld, WorldEntity, world_offset};

const ANIM_RANGE: f32 = 260.0;

/// Coarse A* navigation grid over the generated world (4 m cells; water and cliffs
/// are impassable). Rebuilt whenever the world regenerates.
#[derive(Resource, Default)]
struct NavGrid(Option<worldgen::PathGrid>);

pub struct CreaturesPlugin;

impl Plugin for CreaturesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Flocks>().init_resource::<NavGrid>().add_systems(
            Update,
            (spawn_on_ready, drive_birds, drive_deer, drive_butterflies, day_gate, night_howls),
        );
        if std::env::var("WED_CREATURELINE").is_ok() {
            app.add_systems(Update, stage_creatureline);
        }
        if std::env::var("WED_MODELSHOT").is_ok() {
            app.add_systems(Update, stage_modelshot);
        }
    }
}

#[derive(Resource)]
struct CreatureAssets {
    mat: Handle<StandardMaterial>,
    fur: Handle<StandardMaterial>,
    feather: Handle<StandardMaterial>,
    wing: [Handle<StandardMaterial>; 3],
    deer_body: Handle<Mesh>,
    deer_head: [Handle<Mesh>; 2], // doe, buck
    deer_leg: Handle<Mesh>,
    bird_body: Handle<Mesh>,
    bird_wing: Handle<Mesh>,
    fly_body: Handle<Mesh>,
    fly_wing: [Handle<Mesh>; 3],
}

// ── Components ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum FlockMode {
    Fly,
    Land,
    Ground,
    Rise,
}

struct FlockState {
    centre: Vec3,
    target: Vec3,
    mode: FlockMode,
    /// Seconds left in the current mode.
    timer: f32,
    /// 0 = airborne, 1 = on the ground (eased through Land/Rise).
    blend: f32,
    /// Where the flock is parked while grounded.
    ground: Vec3,
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
    /// This bird's parking offset in the grounded flock.
    ground_off: Vec2,
}

#[derive(Component)]
struct DayCreature; // hidden after dusk (birds roost, butterflies land)

#[derive(Clone, Copy, PartialEq)]
enum DeerState {
    Graze,
    Amble,
    Flee,
    Rest,
    Alert,
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
    /// Eased lying-down weight (Rest folds the legs, lowers the body).
    rest: f32,
    /// Eased locomotion weight (leg swing / body bob fade in and out).
    gait: f32,
    /// A* waypoints (map space) the amble follows; empty = straight wander.
    path: Vec<(f32, f32)>,
    wp: usize,
    /// Cooldown until the next vocalisation.
    call_in: f32,
}

#[derive(Component)]
struct Butterfly {
    home: Vec3,
    phase: f32,
    wings: [Entity; 2],
    /// Eased sitting weight (1 = perched, wings fanning slowly).
    sit: f32,
}

// ── Spawning ───────────────────────────────────────────────────────────────────────

fn spawn_on_ready(
    world: Option<Res<GeneratedWorld>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
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
            let fur_tex = images.add(crate::creature_tex::fur_image());
            let feather_tex = images.add(crate::creature_tex::feather_image());
            let [w0, w1, w2] = crate::creature_tex::wing_images();
            let coat = |tex: Handle<Image>, rough: f32| StandardMaterial {
                base_color_texture: Some(tex),
                perceptual_roughness: rough,
                reflectance: 0.1,
                ..default()
            };
            let a = CreatureAssets {
                mat: mats.add(StandardMaterial {
                    perceptual_roughness: 0.9,
                    reflectance: 0.1,
                    ..default()
                }),
                fur: mats.add(coat(fur_tex, 0.95)),
                feather: mats.add(coat(feather_tex, 0.75)),
                wing: [
                    mats.add(coat(images.add(w0), 0.6)),
                    mats.add(coat(images.add(w1), 0.6)),
                    mats.add(coat(images.add(w2), 0.55)),
                ],
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
    commands.insert_resource(NavGrid(Some(worldgen::PathGrid::build(&world.0))));

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
        flocks.0.push(FlockState {
            centre,
            target: centre,
            mode: FlockMode::Fly,
            timer: rng.range(18.0, 40.0),
            blend: 0.0,
            ground: centre,
        });
        for _ in 0..7 {
            let wings = [
                spawn_part(&mut commands, &assets.feather, assets.bird_wing.clone()),
                spawn_part(&mut commands, &assets.feather, assets.bird_wing.clone()),
            ];
            let bird = commands
                .spawn((
                    Mesh3d(assets.bird_body.clone()),
                    MeshMaterial3d(assets.feather.clone()),
                    Transform::from_translation(centre),
                    WorldEntity,
                    DayCreature,
                    Bird {
                        flock: f,
                        phase: rng.range(0.0, std::f32::consts::TAU),
                        radius: rng.range(14.0, 30.0),
                        bob: rng.range(0.0, std::f32::consts::TAU),
                        wings,
                        ground_off: Vec2::new(rng.range(-2.2, 2.2), rng.range(-2.2, 2.2)),
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
            let head = spawn_part(&mut commands, &assets.fur, head_mesh);
            let legs = [
                spawn_part(&mut commands, &assets.fur, assets.deer_leg.clone()),
                spawn_part(&mut commands, &assets.fur, assets.deer_leg.clone()),
                spawn_part(&mut commands, &assets.fur, assets.deer_leg.clone()),
                spawn_part(&mut commands, &assets.fur, assets.deer_leg.clone()),
            ];
            let scale = rng.range(0.82, 1.05);
            let deer = commands
                .spawn((
                    Mesh3d(assets.deer_body.clone()),
                    MeshMaterial3d(assets.fur.clone()),
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
                        rest: 0.0,
                        gait: 0.0,
                        path: Vec::new(),
                        wp: 0,
                        call_in: rng.range(6.0, 30.0),
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
                spawn_part(&mut commands, &assets.wing[(v % 3) as usize], assets.fly_wing[(v % 3) as usize].clone()),
                spawn_part(&mut commands, &assets.wing[(v % 3) as usize], assets.fly_wing[(v % 3) as usize].clone()),
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
                        sit: 0.0,
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
                        MeshMaterial3d(assets.wing[*wing_v as usize].clone()),
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
                MeshMaterial3d(assets.feather.clone()),
                Transform::from_translation(bp),
                WorldEntity,
            ));
            for side in [-1.0f32, 1.0] {
                commands.spawn((
                    Mesh3d(assets.bird_wing.clone()),
                    MeshMaterial3d(assets.feather.clone()),
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
            fur: self.fur.clone(),
            feather: self.feather.clone(),
            wing: self.wing.clone(),
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

fn spawn_part(
    commands: &mut Commands,
    mat: &Handle<StandardMaterial>,
    mesh: Handle<Mesh>,
) -> Entity {
    commands
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(mat.clone()),
            Transform::default(),
            WorldEntity,
        ))
        .id()
}

/// `WED_MODELSHOT=1` — studio contact sheet: every creature model in 4 orientations
/// (front / left / back / right), rows stacked vertically in open air against the sky,
/// camera parked square-on. Pair with `WED_SHOT` for a review PNG.
fn stage_modelshot(
    world: Option<Res<GeneratedWorld>>,
    assets: Option<Res<CreatureAssets>>,
    mut commands: Commands,
    mut cam: Query<(&mut Transform, &mut FlyCam)>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let (Some(world), Some(assets)) = (world, assets) else { return };
    let hf = &world.0.height;
    // Stage floats well above the map centre — no terrain, no occlusion, sky backdrop.
    let base =
        Vec3::new(0.0, hf.sample_world(hf.extent() * 0.5, hf.extent() * 0.5) + 110.0, 0.0);
    let assets = assets.clone_inner();

    // (label kind, scale) rows bottom-to-top; scales are review-only so the small
    // models read at the same framing distance as the deer.
    enum M {
        Deer { buck: bool },
        Bird,
        Fly(usize),
    }
    // WED_MODELSHOT=deer|bird|fly narrows the sheet to one species, camera close-up.
    let which = std::env::var("WED_MODELSHOT").unwrap_or_default().to_ascii_lowercase();
    let all = [
        (M::Deer { buck: true }, 1.0f32),
        (M::Deer { buck: false }, 1.0),
        (M::Bird, 2.2),
        (M::Fly(0), 3.0),
        (M::Fly(1), 3.0),
        (M::Fly(2), 3.0),
    ];
    let rows: Vec<(M, f32)> = all
        .into_iter()
        .filter(|(m, _)| match which.as_str() {
            "deer" => matches!(m, M::Deer { .. }),
            "bird" => matches!(m, M::Bird),
            "fly" => matches!(m, M::Fly(_)),
            _ => true,
        })
        .collect();
    // Column yaws: front, left profile, back, right profile. The camera sits on the
    // +Z side looking -Z — the sky sun (its +Z lean) then lights the camera-facing side.
    let yaws = [
        -std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
        std::f32::consts::FRAC_PI_2,
        0.0,
    ];
    for (ri, (model, scale)) in rows.iter().enumerate() {
        for (ci, yaw) in yaws.iter().enumerate() {
            let pos = base + Vec3::new(ci as f32 * 3.0 - 4.5, ri as f32 * 2.4 - 5.0, 0.0);
            let tf = Transform::from_translation(pos)
                .with_rotation(Quat::from_rotation_y(*yaw))
                .with_scale(Vec3::splat(*scale));
            match model {
                M::Deer { buck } => {
                    let root = commands
                        .spawn((
                            Mesh3d(assets.deer_body.clone()),
                            MeshMaterial3d(assets.fur.clone()),
                            tf,
                            WorldEntity,
                        ))
                        .id();
                    let head = commands
                        .spawn((
                            Mesh3d(assets.deer_head[usize::from(*buck)].clone()),
                            MeshMaterial3d(assets.fur.clone()),
                            Transform::from_translation(Vec3::from_array(creature_mesh::DEER_NECK)),
                            WorldEntity,
                        ))
                        .id();
                    commands.entity(root).add_child(head);
                    // Light stride so the gait silhouette is reviewable.
                    for (i, hip) in creature_mesh::DEER_HIPS.iter().enumerate() {
                        let swing = if i == 0 || i == 3 { 0.18 } else { -0.18 };
                        let leg = commands
                            .spawn((
                                Mesh3d(assets.deer_leg.clone()),
                                MeshMaterial3d(assets.fur.clone()),
                                Transform::from_translation(Vec3::from_array(*hip))
                                    .with_rotation(Quat::from_rotation_z(swing)),
                                WorldEntity,
                            ))
                            .id();
                        commands.entity(root).add_child(leg);
                    }
                }
                M::Bird => {
                    let root = commands
                        .spawn((
                            Mesh3d(assets.bird_body.clone()),
                            MeshMaterial3d(assets.feather.clone()),
                            tf,
                            WorldEntity,
                        ))
                        .id();
                    for side in [-1.0f32, 1.0] {
                        let wing = commands
                            .spawn((
                                Mesh3d(assets.bird_wing.clone()),
                                MeshMaterial3d(assets.feather.clone()),
                                Transform::from_translation(Vec3::new(0.02, 0.03, 0.04 * side))
                                    .with_rotation(
                                        Quat::from_rotation_y(
                                            -side * std::f32::consts::FRAC_PI_2,
                                        ) * Quat::from_rotation_z(side * 0.55),
                                    ),
                                WorldEntity,
                            ))
                            .id();
                        commands.entity(root).add_child(wing);
                    }
                }
                M::Fly(v) => {
                    let root = commands
                        .spawn((
                            Mesh3d(assets.fly_body.clone()),
                            MeshMaterial3d(assets.mat.clone()),
                            tf,
                            WorldEntity,
                        ))
                        .id();
                    for side in [-1.0f32, 1.0] {
                        let wing = commands
                            .spawn((
                                Mesh3d(assets.fly_wing[*v].clone()),
                                MeshMaterial3d(assets.wing[*v].clone()),
                                Transform::from_translation(Vec3::new(-0.01, 0.005, 0.0))
                                    .with_rotation(
                                        Quat::from_rotation_y(
                                            -side * std::f32::consts::FRAC_PI_2,
                                        ) * Quat::from_rotation_z(side * 0.45),
                                    ),
                                WorldEntity,
                            ))
                            .id();
                        commands.entity(root).add_child(wing);
                    }
                }
            }
        }
    }

    // Square-on camera, close enough that the rows fill a 45° vertical FOV.
    let sheet_h = rows.len() as f32 * 2.4;
    let row_mid = base + Vec3::new(0.0, (rows.len() as f32 - 1.0) * 1.2 - 5.0 + 0.8, 0.0);
    let centre = row_mid;
    let dist = (sheet_h * 1.25).max(9.5).min(17.5);
    let eye = centre + Vec3::new(0.0, 0.0, dist);
    for (mut tf, mut fc) in &mut cam {
        *tf = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        fc.yaw = yaw;
        fc.pitch = pitch;
    }
    info!("modelshot staged at {centre:?}");
    *done = true;
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
    for (i, f) in flocks.0.iter_mut().enumerate() {
        // Mode clock: fly -> land -> peck about -> rise -> fly.
        f.timer -= dt;
        if f.timer <= 0.0 {
            let mut rng = worldgen::rng::Rng::new((t * 977.0) as u32 ^ (i as u32 * 6151));
            match f.mode {
                FlockMode::Fly => {
                    // Only land on dry ground; over a lake just keep flying.
                    let mx = f.centre.x - off;
                    let mz = f.centre.z - off;
                    let size = hf.size;
                    let ix = (mx.max(0.0) as usize).min(size - 1);
                    let iz = (mz.max(0.0) as usize).min(size - 1);
                    if world.0.water[iz * size + ix].is_finite() {
                        f.timer = rng.range(8.0, 15.0);
                    } else {
                        f.ground = Vec3::new(f.centre.x, hf.sample_world(mx, mz), f.centre.z);
                        f.mode = FlockMode::Land;
                        f.timer = 3.0;
                    }
                }
                FlockMode::Land => {
                    f.mode = FlockMode::Ground;
                    f.timer = rng.range(10.0, 22.0);
                }
                FlockMode::Ground => {
                    f.mode = FlockMode::Rise;
                    f.timer = 3.0;
                }
                FlockMode::Rise => {
                    f.mode = FlockMode::Fly;
                    f.timer = rng.range(20.0, 45.0);
                }
            }
        }
        // A landed flock startles airborne if the camera closes in.
        if matches!(f.mode, FlockMode::Ground | FlockMode::Land)
            && f.ground.distance(camp) < 9.0
        {
            f.mode = FlockMode::Rise;
            f.timer = 2.0;
        }
        let want = match f.mode {
            FlockMode::Fly | FlockMode::Rise => 0.0,
            FlockMode::Land | FlockMode::Ground => 1.0,
        };
        f.blend += (want - f.blend) * (dt * 1.0).min(1.0);

        // Anchor drift only while airborne (a grounded flock stays put).
        if (!pinned || i != 0) && matches!(f.mode, FlockMode::Fly) {
            if f.centre.distance(f.target) < 12.0 {
                let mut rng = worldgen::rng::Rng::new((t * 977.0) as u32 ^ (i as u32 * 7919));
                let mx = rng.range(ext * 0.15, ext * 0.85);
                let mz = rng.range(ext * 0.15, ext * 0.85);
                let y = hf.sample_world(mx, mz) + rng.range(32.0, 58.0);
                f.target = Vec3::new(mx + off, y, mz + off);
            }
            let dir = (f.target - f.centre).normalize_or_zero();
            f.centre += dir * 6.5 * dt * (1.0 - f.blend);
        }
    }

    for (bird, mut tf) in &mut birds {
        let f = &flocks.0[bird.flock];
        if f.centre.distance(camp) > 700.0 {
            continue;
        }
        // Airborne pose: constant-airspeed circling with a glide/flap cycle.
        let w = 9.0 / bird.radius;
        let ang = t * w + bird.phase;
        let (sa, ca) = ang.sin_cos();
        let air = f.centre
            + Vec3::new(ca * bird.radius, (t * 0.7 + bird.bob).sin() * 2.2, sa * bird.radius);
        let vel = Vec3::new(-sa, 0.0, ca);
        let air_yaw = f32::atan2(-vel.z, vel.x);
        // Grounded pose: parked at the flock spot, slow look-around.
        let grd = f.ground + Vec3::new(bird.ground_off.x, 0.02, bird.ground_off.y);
        let grd_yaw = bird.phase + (t * 0.35 + bird.phase * 2.0).sin() * 1.2;

        let k = f.blend * f.blend * (3.0 - 2.0 * f.blend); // smoothstep
        tf.translation = air.lerp(grd, k);
        // Peck: quick nose-dips while grounded.
        let peck = if k > 0.8 {
            let cycle = (t * 0.9 + bird.phase * 1.7).sin().max(0.0);
            cycle.powi(6) * 0.7
        } else {
            0.0
        };
        let yaw = if k < 0.5 { air_yaw } else { grd_yaw };
        tf.rotation = Quat::from_rotation_y(yaw)
            * Quat::from_rotation_x(-0.35 * (1.0 - k))
            * Quat::from_rotation_z(-peck);

        // Wings: flap bursts + glides airborne; big flare while transitioning; folded
        // tight against the body on the ground.
        let flap_gate = ((t * 0.31 + bird.phase).sin() * 2.5).clamp(0.0, 1.0);
        let air_flap = (t * 8.0 + bird.phase * 3.0).sin() * (0.15 + 0.4 * flap_gate);
        let flare = (k * (1.0 - k)) * 4.0; // peaks mid-transition
        let flap = air_flap * (1.0 - k) + (t * 6.0).sin() * 0.8 * flare;
        let fold = 1.05 * k; // sweep the wing back along the body when grounded
        for (side, w_ent) in [(1.0f32, bird.wings[0]), (-1.0, bird.wings[1])] {
            if let Ok(mut wtf) = parts.get_mut(w_ent) {
                wtf.translation = Vec3::new(0.02 - 0.05 * k, 0.03, 0.04 * side);
                wtf.rotation =
                    Quat::from_rotation_y(-side * (std::f32::consts::FRAC_PI_2 - fold))
                        * Quat::from_rotation_z(side * (flap + 0.10 * k));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_deer(
    time: Res<Time>,
    world: Option<Res<GeneratedWorld>>,
    nav: Res<NavGrid>,
    mut deer: Query<(&mut Deer, &mut Transform), Without<FlyCam>>,
    mut parts: Query<&mut Transform, (Without<Deer>, Without<FlyCam>)>,
    cam: Query<&Transform, With<FlyCam>>,
    mut commands: Commands,
    asset: Res<AssetServer>,
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
        // Threat response: freeze and stare first, bolt when pressed.
        if dist_cam < 26.0 && !matches!(d.state, DeerState::Flee | DeerState::Alert) {
            d.state = DeerState::Alert;
            d.timer = 2.2;
            let to_cam = (camp - tf.translation).with_y(0.0).normalize_or_zero();
            d.yaw = f32::atan2(-to_cam.z, to_cam.x);
            d.path.clear();
        }
        if dist_cam < 13.0 && d.state != DeerState::Flee {
            d.state = DeerState::Flee;
            d.timer = 4.5;
            let away = (tf.translation - camp).with_y(0.0).normalize_or_zero();
            d.yaw = f32::atan2(-away.z, away.x);
            d.path.clear();
            // Alarm snort, louder up close.
            let vol = (1.0 - dist_cam / 30.0).clamp(0.2, 1.0) * 0.8;
            commands.spawn((
                AudioPlayer(asset.load::<AudioSource>("audio/deer-2.ogg")),
                PlaybackSettings::DESPAWN.with_volume(Volume::Linear(vol)),
            ));
        }
        // Occasional soft call while calm and near enough to hear.
        d.call_in -= dt;
        if d.call_in <= 0.0 {
            let mut rng = worldgen::rng::Rng::new((t * 631.0) as u32 ^ d.phase.to_bits());
            d.call_in = rng.range(18.0, 55.0);
            if dist_cam < 55.0 && matches!(d.state, DeerState::Graze | DeerState::Amble) {
                let vol = (1.0 - dist_cam / 60.0).clamp(0.1, 0.6) * 0.5;
                commands.spawn((
                    AudioPlayer(asset.load::<AudioSource>("audio/deer-1.ogg")),
                    PlaybackSettings::DESPAWN.with_volume(Volume::Linear(vol)),
                ));
            }
        }

        d.timer -= dt;
        if d.timer <= 0.0 {
            let mut rng = worldgen::rng::Rng::new((t * 613.0) as u32 ^ d.phase.to_bits());
            let here = Vec2::new(tf.translation.x - off, tf.translation.z - off);
            d.state = match d.state {
                DeerState::Alert | DeerState::Flee | DeerState::Rest => DeerState::Graze,
                DeerState::Graze => {
                    let roll = rng.next_u32() % 100;
                    if roll < 55 {
                        DeerState::Amble
                    } else if roll < 75 && dist_cam > 35.0 {
                        DeerState::Rest
                    } else {
                        DeerState::Graze
                    }
                }
                DeerState::Amble => DeerState::Graze,
            };
            d.timer = match d.state {
                DeerState::Rest => rng.range(14.0, 28.0),
                DeerState::Amble => 30.0, // ended early when the path completes
                _ => rng.range(3.0, 9.0),
            };
            d.path.clear();
            d.wp = 0;
            if matches!(d.state, DeerState::Amble) {
                // Plot an A* amble to a fresh spot around home (falls back to a
                // straight wander when no path fits the expansion budget).
                let goal_ang = rng.range(0.0, std::f32::consts::TAU);
                let goal_r = rng.range(8.0, 26.0);
                let goal = (
                    (d.home.x + goal_ang.cos() * goal_r).clamp(6.0, hf.extent() - 6.0),
                    (d.home.y + goal_ang.sin() * goal_r).clamp(6.0, hf.extent() - 6.0),
                );
                if let Some(grid) = nav.0.as_ref() {
                    if let Some(path) = worldgen::find_path(grid, (here.x, here.y), goal, 3000)
                    {
                        d.path = path;
                        d.wp = 0;
                    }
                }
                if d.path.is_empty() {
                    d.yaw = rng.range(0.0, std::f32::consts::TAU);
                }
            }
        }

        let speed = match d.state {
            DeerState::Graze | DeerState::Rest | DeerState::Alert => 0.0,
            DeerState::Amble => 0.9,
            DeerState::Flee => 4.2,
        };
        if speed > 0.0 {
            // Follow the plotted waypoints when there are any; else straight + bounce.
            if !d.path.is_empty() {
                let here = Vec2::new(tf.translation.x - off, tf.translation.z - off);
                while d.wp < d.path.len() {
                    let w = Vec2::new(d.path[d.wp].0, d.path[d.wp].1);
                    if here.distance(w) < 1.2 {
                        d.wp += 1;
                    } else {
                        break;
                    }
                }
                if d.wp >= d.path.len() {
                    d.state = DeerState::Graze;
                    d.timer = 3.0;
                    d.path.clear();
                } else {
                    let w = Vec2::new(d.path[d.wp].0, d.path[d.wp].1);
                    let want = f32::atan2(-(w.y - here.y), w.x - here.x);
                    // Turn-rate limit so the walk arcs instead of snapping.
                    let mut diff = (want - d.yaw).rem_euclid(std::f32::consts::TAU);
                    if diff > std::f32::consts::PI {
                        diff -= std::f32::consts::TAU;
                    }
                    d.yaw += diff.clamp(-2.8 * dt, 2.8 * dt);
                }
            }
            let dir = Vec3::new(d.yaw.cos(), 0.0, -d.yaw.sin());
            let next = tf.translation + dir * speed * dt;
            let mx = next.x - off;
            let mz = next.z - off;
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
                d.yaw += 1.7;
                d.path.clear();
            }
        }

        // -- Pose blending ------------------------------------------------------
        let run = (speed / 4.2).clamp(0.0, 1.0);
        d.gait += ((if speed > 0.0 { 1.0 } else { 0.0 }) - d.gait) * (dt * 5.0).min(1.0);
        let want_rest = if matches!(d.state, DeerState::Rest) { 1.0 } else { 0.0 };
        d.rest += (want_rest - d.rest) * (dt * 1.6).min(1.0);

        // Body: bob with the gait, rock with the gallop, sink when lying.
        let rate = 5.0 + 6.5 * run;
        let bob = (t * rate * 2.0 + d.phase).sin() * 0.03 * d.gait;
        let pitch = (t * rate + d.phase + 0.7).sin() * 0.075 * run * d.gait;
        tf.translation.y =
            hf.sample_world(tf.translation.x - off, tf.translation.z - off) + bob
                - 0.40 * d.rest;
        tf.rotation = Quat::from_rotation_y(d.yaw) * Quat::from_rotation_z(pitch);

        // Head: grazing nose-down with nibble bobs and periodic look-up scans;
        // Alert/Rest carry it high.
        let scanning = (((t * 0.13 + d.phase).fract() < 0.20) as u32) as f32;
        let want_dip = match d.state {
            DeerState::Graze => 1.0 - 0.85 * scanning,
            _ => 0.0,
        };
        d.dip += (want_dip - d.dip) * (dt * 3.0).min(1.0);
        if let Ok(mut htf) = parts.get_mut(d.head) {
            htf.translation = Vec3::from_array(creature_mesh::DEER_NECK);
            let nibble = (t * 3.3 + d.phase).sin() * 0.10 * d.dip;
            htf.rotation = Quat::from_rotation_z(-1.75 * d.dip + nibble);
        }
        // Legs: diagonal walk -> grouped gallop by `run`; folded under when lying.
        let amp = (0.42 + 0.42 * run) * d.gait * (1.0 - d.rest);
        for (i, leg) in d.legs.iter().enumerate() {
            if let Ok(mut ltf) = parts.get_mut(*leg) {
                let front = i < 2;
                let diag = if i == 0 || i == 3 { 0.0 } else { std::f32::consts::PI };
                let gallop = if front { 0.0 } else { std::f32::consts::PI * 0.55 };
                let phase = diag * (1.0 - run) + gallop * run;
                // Fold: front legs tuck backward, hind legs forward.
                let fold = if front { 1.5 } else { -1.5 } * d.rest;
                ltf.translation = Vec3::from_array(creature_mesh::DEER_HIPS[i]);
                ltf.rotation =
                    Quat::from_rotation_z((t * rate + phase + d.phase).sin() * amp + fold);
            }
        }
    }
}

fn drive_butterflies(
    time: Res<Time>,
    world: Option<Res<GeneratedWorld>>,
    mut flies: Query<(&mut Butterfly, &mut Transform), Without<FlyCam>>,
    mut parts: Query<&mut Transform, (Without<Butterfly>, Without<FlyCam>)>,
    cam: Query<&Transform, With<FlyCam>>,
) {
    let Some(world) = world else { return };
    let hf = &world.0.height;
    let off = world_offset(hf);
    let t = time.elapsed_secs();
    let dt = time.delta_secs();
    let camp = cam_pos(&cam);
    for (mut fly, mut tf) in &mut flies {
        if fly.home.distance(camp) > 160.0 {
            continue;
        }
        let p = fly.phase;
        // Perch cycle: settle on the grass for a while, then flutter off. A close
        // camera flushes them airborne.
        let wants_sit = ((((t * 0.045 + p * 0.7).fract() < 0.35)
            && tf.translation.distance(camp) > 6.0) as u32) as f32;
        fly.sit += (wants_sit - fly.sit) * (dt * 1.4).min(1.0);

        // Airborne wander around the home flower patch.
        let air = fly.home
            + Vec3::new(
                (t * 0.53 + p).sin() * 3.2 + (t * 1.7 + p * 2.0).sin() * 0.5,
                (t * 1.1 + p).sin() * 0.5 + (t * 3.9 + p).sin() * 0.14,
                (t * 0.61 + p * 1.3).cos() * 3.2 + (t * 1.9 + p).cos() * 0.5,
            );
        // Perch: a fixed spot on the ground near home.
        let perch = {
            let px = fly.home.x + (p * 7.9).sin() * 2.0;
            let pz = fly.home.z + (p * 5.3).cos() * 2.0;
            Vec3::new(px, hf.sample_world(px - off, pz - off) + 0.05, pz)
        };
        let k = fly.sit * fly.sit * (3.0 - 2.0 * fly.sit);
        let pos = air.lerp(perch, k);
        let vel = pos - tf.translation;
        if vel.length_squared() > 1e-6 && k < 0.8 {
            let yaw = f32::atan2(-vel.z, vel.x);
            tf.rotation = Quat::from_rotation_y(yaw);
        }
        tf.translation = pos;
        // Wings: fast flight flap -> slow, nearly-closed fanning while perched.
        let flight = (t * 11.0 + p * 5.0).sin() * 1.1;
        let fanning = 1.15 + (t * 1.3 + p).sin() * 0.30;
        let angle = flight * (1.0 - k) + fanning * k;
        for (side, w_ent) in [(1.0f32, fly.wings[0]), (-1.0, fly.wings[1])] {
            if let Ok(mut wtf) = parts.get_mut(w_ent) {
                wtf.translation = Vec3::new(-0.01, 0.005, 0.0);
                wtf.rotation = Quat::from_rotation_y(-side * std::f32::consts::FRAC_PI_2)
                    * Quat::from_rotation_z(side * angle);
            }
        }
    }
}

/// Distant wolf howls after dark — sells the night even though no wolf is modelled.
/// deer-1/2 + wolf-1/2 are the Warbell clips; volumes stay low (ambience, not jumpscare).
fn night_howls(
    time: Res<Time>,
    clock: Res<DayClock>,
    mut next_in: Local<f32>,
    mut commands: Commands,
    asset: Res<AssetServer>,
) {
    let elev = sun_dir(clock.t).y;
    if elev > -0.08 {
        return;
    }
    if *next_in <= 0.0 {
        *next_in = 50.0; // first howl a while after dusk
        return;
    }
    *next_in -= time.delta_secs();
    if *next_in <= 0.0 {
        let mut rng = worldgen::rng::Rng::new((time.elapsed_secs() * 733.0) as u32);
        let clip = if rng.next_u32() % 2 == 0 { "audio/wolf-1.ogg" } else { "audio/wolf-2.ogg" };
        commands.spawn((
            AudioPlayer(asset.load::<AudioSource>(clip)),
            PlaybackSettings::DESPAWN.with_volume(Volume::Linear(rng.range(0.10, 0.22))),
        ));
        *next_in = rng.range(70.0, 160.0);
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
