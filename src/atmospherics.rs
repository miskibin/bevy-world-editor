//! Cinematic atmospherics post pass — analytic height fog + sun in-scatter + drifting
//! fog-density noise + cloud light patches, one fullscreen pass. Ported from Warbell
//! (src/atmospherics.rs there; shader copied verbatim). Differences here: no godrays pass
//! to order against (chain is tonemapping → smaa → atmospherics → dof), the sun is
//! static, and the env override is `WED_ATMO`.

use bevy::{
    anti_alias::smaa::smaa,
    core_pipeline::{Core3d, Core3dSystems, FullscreenShader, prepass::ViewPrepassTextures},
    pbr::DistanceFog,
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_resource::{
            binding_types::{sampler, texture_2d, texture_depth_2d, uniform_buffer},
            *,
        },
        diagnostic::RecordDiagnostics,
        renderer::{RenderContext, RenderDevice, ViewQuery},
        view::ViewTarget,
    },
};

use crate::sky::Sun;

const SHADER_ASSET_PATH: &str = "shaders/atmospherics.wgsl";

/// Per-camera atmospherics settings (also the shader uniform). Field ORDER must match the
/// WGSL `Settings` struct exactly.
#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
pub struct Atmospherics {
    pub world_from_clip: Mat4,
    pub cam_pos: Vec3,
    /// Base fog density per world unit at `base_height`.
    pub density: f32,
    pub sun_dir: Vec3,
    /// How fast the fog thins with altitude (per world unit).
    pub height_falloff: f32,
    pub fog_color: Vec3,
    /// Sun-glow lobe exponent — low = wide golden wash.
    pub inscatter_exp: f32,
    pub glow_color: Vec3,
    pub time: f32,
    /// Cloud light-patch modulation depth (0 = off).
    pub cloud_strength: f32,
    /// Cloud noise frequency in world XZ (1/units).
    pub cloud_scale: f32,
    /// Fog-density noise modulation (0 = uniform veil).
    pub noise_strength: f32,
    /// Fog-free radius around the camera.
    pub fog_start: f32,
    /// Max fog opacity — geometry never fully swallowed.
    pub fog_max: f32,
    /// 0..1 master gate.
    pub fade: f32,
    /// World Y where fog is densest (our lake level).
    pub base_height: f32,
    pub _pad: f32,
}

impl Default for Atmospherics {
    fn default() -> Self {
        Self {
            world_from_clip: Mat4::IDENTITY,
            cam_pos: Vec3::ZERO,
            // Tuned for the 1 km map + 170 m relief (Warbell's numbers, rescaled: taller
            // world → gentler falloff, valley fog pooling low).
            // 0.0055 (was 0.0020) — user: the haze read far too weak on the 1 km map.
            density: 0.0055,
            sun_dir: Vec3::Y,
            height_falloff: 0.022,
            fog_color: Vec3::new(0.78, 0.80, 0.72),
            inscatter_exp: 4.0,
            glow_color: Vec3::new(1.0, 0.88, 0.65),
            time: 0.0,
            cloud_strength: 0.10,
            cloud_scale: 0.014,
            noise_strength: 0.40,
            fog_start: 55.0,
            fog_max: 0.70,
            fade: 0.0,
            base_height: 8.0,
            _pad: 0.0,
        }
    }
}

/// `WED_ATMO="density,falloff,inscatter_exp,fog_start,fog_max,noise,cloud_strength,cloud_scale"`
/// startup override for screenshot-harness tuning.
pub fn default_atmospherics() -> Atmospherics {
    let mut a = Atmospherics::default();
    if let Ok(s) = std::env::var("WED_ATMO") {
        let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if v.len() == 8 {
            a.density = v[0];
            a.height_falloff = v[1];
            a.inscatter_exp = v[2];
            a.fog_start = v[3];
            a.fog_max = v[4];
            a.noise_strength = v[5];
            a.cloud_strength = v[6];
            a.cloud_scale = v[7];
        }
    }
    a
}

/// Panel toggle + strength (scales `fade`).
#[derive(Resource)]
pub struct AtmoSettings {
    pub enabled: bool,
    pub strength: f32,
}

impl Default for AtmoSettings {
    fn default() -> Self {
        AtmoSettings { enabled: true, strength: 1.0 }
    }
}

pub struct AtmosphericsPlugin;

impl Plugin for AtmosphericsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ExtractComponentPlugin::<Atmospherics>::default(),
            UniformComponentPlugin::<Atmospherics>::default(),
        ))
        .init_resource::<AtmoSettings>()
        .add_systems(Update, drive_atmospherics);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(RenderStartup, init_pipeline);
        // Pinned chain: tonemapping → smaa → atmospherics → dof (see Warbell's flicker
        // note — every post_process_write ping-pong MUST be explicitly ordered).
        render_app.add_systems(
            Core3d,
            atmospherics_pass
                .in_set(Core3dSystems::PostProcess)
                .after(smaa)
                .before(crate::dof::dof_pass),
        );
    }
}

fn drive_atmospherics(
    time: Res<Time>,
    settings: Res<AtmoSettings>,
    sun: Query<&GlobalTransform, With<Sun>>,
    mut cams: Query<(&GlobalTransform, &Camera, &DistanceFog, &mut Atmospherics)>,
) {
    let Ok(sun_tf) = sun.single() else {
        return;
    };
    let dir = sun_tf.translation().normalize_or_zero();
    let fade = if settings.enabled { settings.strength } else { 0.0 };
    for (cam_tf, cam, fog, mut atmo) in &mut cams {
        let view = cam_tf.to_matrix();
        atmo.world_from_clip = view * cam.clip_from_view().inverse();
        atmo.cam_pos = cam_tf.translation();
        atmo.sun_dir = dir;
        let f = fog.color.to_linear();
        atmo.fog_color = Vec3::new(f.red, f.green, f.blue);
        let g = fog.directional_light_color.to_linear();
        atmo.glow_color = Vec3::new(g.red, g.green, g.blue);
        atmo.time = time.elapsed_secs();
        atmo.fade = fade;
    }
}

pub(crate) fn atmospherics_pass(
    view: ViewQuery<(
        &ViewTarget,
        &ViewPrepassTextures,
        &Atmospherics,
        &DynamicUniformIndex<Atmospherics>,
    )>,
    pipeline_res: Res<AtmosphericsPipeline>,
    pipeline_cache: Res<PipelineCache>,
    uniforms: Res<ComponentUniforms<Atmospherics>>,
    mut ctx: RenderContext,
) {
    let (view_target, prepass, _settings, settings_index) = view.into_inner();
    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.pipeline_id) else {
        return;
    };
    let Some(settings_binding) = uniforms.uniforms().binding() else {
        return;
    };
    let Some(depth_view) = prepass.depth_view() else {
        return;
    };

    let post_process = view_target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        "atmospherics_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline_res.layout),
        &BindGroupEntries::sequential((
            post_process.source,
            &pipeline_res.sampler,
            depth_view,
            settings_binding.clone(),
        )),
    );

    // Custom passes are invisible to RenderDiagnosticsPlugin unless we record a span —
    // and an uninstrumented pass reads as "free" in the F2 table (it wasn't).
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let time_span = diagnostics.time_span(ctx.command_encoder(), "atmospherics");
    let mut render_pass = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
        label: Some("atmospherics_pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: post_process.destination,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        ..default()
    });

    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
    render_pass.draw(0..3, 0..1);
    drop(render_pass);
    time_span.end(ctx.command_encoder());
}

#[derive(Resource)]
pub(crate) struct AtmosphericsPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    pipeline_id: CachedRenderPipelineId,
}

fn init_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
    pipeline_cache: Res<PipelineCache>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "atmospherics_bind_group_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_depth_2d(),
                uniform_buffer::<Atmospherics>(true),
            ),
        ),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor::default());
    let shader = asset_server.load(SHADER_ASSET_PATH);
    let vertex_state = fullscreen_shader.to_vertex_state();
    let pipeline_id = pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("atmospherics_pipeline".into()),
        layout: vec![layout.clone()],
        vertex: vertex_state,
        fragment: Some(FragmentState {
            shader,
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::Rgba16Float,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
            ..default()
        }),
        ..default()
    });
    commands.insert_resource(AtmosphericsPipeline { layout, sampler, pipeline_id });
}
