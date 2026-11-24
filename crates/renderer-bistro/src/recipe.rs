//! A Rust rendering pipeline consuming Bistro scene and material data.

#![cfg_attr(
    not(any(feature = "vulkan", all(windows, feature = "d3d12"))),
    allow(dead_code)
)]

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use crate::shared::memory;
#[cfg(feature = "tracy")]
use crate::shared::trace;
use crate::shared::{asset, camera, deferred, ibl, instancing, lighting, program, shadow};
#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
use renderer_example_support::clock;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
use renderer_example_support::{window_camera, window_input};

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod backend;
mod lifecycle;
mod lighting_pass;
use lighting_pass::LightingPass;
#[cfg(test)]
#[allow(
    clippy::duplicate_mod,
    reason = "the same fixture asserts both independently built Graphs"
)]
#[path = "shared/material_tests.rs"]
mod material_tests;
mod pipeline;
mod run;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod window;

mod recipes;
mod shader_assets;

use renderer::passes::bloom::Bloom;
use sha2::{Digest, Sha256};
use std::{borrow::Cow, path::PathBuf, time::Duration};

use crate::Error;

const DEFAULT_API: &str = if cfg!(all(windows, feature = "d3d12", not(feature = "vulkan"))) {
    "d3d12"
} else {
    "vulkan"
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipelineMode {
    Forward,
    ForwardPlus,
    ForwardReference,
    Deferred,
}

impl PipelineMode {
    fn name(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::ForwardPlus => "forward-plus",
            Self::ForwardReference => "forward-reference",
            Self::Deferred => "deferred",
        }
    }

    fn uses_local_lights(self) -> bool {
        matches!(self, Self::ForwardPlus | Self::ForwardReference)
    }

    fn forward_program(self, blend: bool) -> renderer_core::program::ManagedRasterProgram {
        if self.uses_local_lights() {
            program::forward_plus::raster_program(self == Self::ForwardPlus, blend)
        } else if blend {
            program::blend_program()
        } else {
            program::opaque_program()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmissionMode {
    Direct,
    Packed,
    Indirect,
}

impl SubmissionMode {
    fn name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Packed => "packed",
            Self::Indirect => "indirect",
        }
    }
}

#[derive(Clone, Debug)]
struct Options {
    api: String,
    manifest: Option<PathBuf>,
    ibl_manifest: Option<PathBuf>,
    reflection_probes: Option<PathBuf>,
    reflection_debug: renderer::pbr::ReflectionProbeView,
    reflection_spheres: Option<[f32; 3]>,
    reflection_spheres_to: Option<[f32; 3]>,
    baked_lighting: Option<PathBuf>,
    scenario_blend: f32,
    lighting_pass: LightingPass,
    gi_debug: renderer::pbr::BakedLightingView,
    sweep: bool,
    ball: Option<[f32; 3]>,
    fixture: bool,
    look: camera::Look,
    view: String,
    width: u32,
    height: u32,
    frames: usize,
    warmup: usize,
    inflight: usize,
    gpu_timing: bool,
    steady_state_cpu: bool,
    alpha_only: bool,
    force_opaque: bool,
    capture_all_views: bool,
    route_capture: Option<camera::RouteCapture>,
    capture_bloom_bypass: bool,
    windowed: bool,
    window_smoke: bool,
    frame_limit: Option<usize>,
    vsync: bool,
    resource_checks: bool,
    bloom: Bloom,
    sample_count: renderer_core::graph::SampleCount,
    pipeline: PipelineMode,
    shader_assets: Option<shader_assets::Selection>,
    instances: instancing::Mode,
    submission: SubmissionMode,
    #[cfg(feature = "development-validation")]
    omit_material: Option<usize>,
    #[cfg(feature = "development-validation")]
    offset_shadow_receiver: bool,
    aa: bool,
    shadows: bool,
    reuse_shadows: bool,
    rect_lights: RectLights,
    capture_shadows: bool,
    #[cfg(feature = "tracy")]
    capture_handshake: bool,
    output: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RectLights {
    Off,
    Unshadowed,
    Shadowed,
}
impl Options {
    fn condition_bytes(&self) -> &'static [u8] {
        match self.look {
            camera::Look::O3deAtrium => include_bytes!("../../../tools/sponza/conditions.json"),
            _ => include_bytes!("../../../tools/bistro/conditions.json"),
        }
    }
    fn uses_light_grid(&self) -> bool {
        self.pipeline == PipelineMode::ForwardReference
            || (self.pipeline == PipelineMode::ForwardPlus && self.baked_lighting.is_none())
    }
    fn uses_local_lights(&self) -> bool {
        if self.look == camera::Look::O3deAtrium {
            self.rect_lights != RectLights::Off
        } else {
            self.uses_light_grid()
        }
    }
    fn diffuse_lighting(&self, blend: f32) -> renderer::pbr::DiffuseLighting {
        if self.baked_lighting.is_some() {
            renderer::pbr::DiffuseLighting::BakedLighting {
                blend,
                view: self.gi_debug,
            }
        } else {
            renderer::pbr::DiffuseLighting::EnvironmentMap
        }
    }
    fn camera_lighting(
        &self,
        conditions: &camera::Conditions,
        camera: camera::Camera,
        frame: usize,
    ) -> Result<camera::CameraInput, Error> {
        let mut input = conditions.frame_input(camera, self.width, self.height)?;
        if self.baked_lighting.is_some() || self.reflection_probes.is_some() {
            let t = if self.sweep {
                (frame as f32 / (self.frames.saturating_sub(1).max(1) as f32)).clamp(0., 1.)
            } else {
                self.scenario_blend
            };
            let light = camera::BakeScenario::for_look(t, self.look);
            input.direction_to_light = camera::normalize(light.direction)?;
            input.irradiance = light.sun;
            input.scenario_blend = t;
        }
        if !self.lighting_pass.has_sun() {
            input.irradiance = [0.; 3];
        }
        Ok(input)
    }
    fn sweep_clear(&self, t: f32) -> Option<[f32; 4]> {
        if self.lighting_pass != LightingPass::Beauty {
            return Some([0., 0., 0., 1.]);
        }
        self.sweep.then(|| {
            let [r, g, b] = camera::BakeScenario::for_look(t, self.look).sky;
            [r, g, b, 1.]
        })
    }
    fn prepare_lighting(
        &self,
        scene: &mut asset::Scene,
        conditions: &mut camera::Conditions,
        env: &mut ibl::Environment,
        look: &mut serde_json::Value,
    ) -> Result<(), Error> {
        self.apply_baked_lighting(scene, conditions, env, look)?;
        // Bind static placements before adding dynamic probe receivers.
        if let Some(pos) = self.ball {
            scene.add_inspection_sphere(pos, 0.85, 0.);
        }
        if let Some(pos) = self.reflection_spheres {
            for (dz, roughness) in [(-0.5, 0.08), (0.5, 0.55)] {
                scene.add_inspection_sphere([pos[0], pos[1], pos[2] + dz], roughness, 1.);
            }
        }
        self.lighting_pass
            .apply(scene, env, self.baked_lighting.is_some())?;
        look["lighting_pass"] = serde_json::json!(format!("{:?}", self.lighting_pass));
        if self.lighting_pass != LightingPass::Beauty {
            look["ibl_strength"] = serde_json::json!(env.nits_per_unit);
            look["background_linear_rgb"] = serde_json::json!([0.0, 0.0, 0.0]);
            if !self.lighting_pass.has_sun() {
                look["irradiance_rgb"] = serde_json::json!([0.0, 0.0, 0.0]);
            }
        }
        Ok(())
    }
    fn apply_baked_lighting(
        &self,
        scene: &mut asset::Scene,
        conditions: &mut camera::Conditions,
        env: &mut ibl::Environment,
        look: &mut serde_json::Value,
    ) -> Result<(), Error> {
        if self.reflection_probes.is_none() && self.baked_lighting.is_none() {
            return Ok(());
        }
        let base = camera::bake_identity(&run::loaded_cook_hash(scene, self.fixture)?, self.look);
        let hdr = camera::environment_bake_identity(&base, env);
        let matches_bake =
            |id: &[u8]| base.as_bytes() == id || hdr.as_ref().is_some_and(|s| s.as_bytes() == id);
        if let Some(path) = &self.reflection_probes {
            let bytes = std::fs::read(path)?;
            let (identity, file) = bytes
                .split_at_checked(64)
                .ok_or("truncated reflection probe asset")?;
            if !matches_bake(identity) {
                return Err(
                    "reflection probes belong to a different Bistro cook or lighting preset".into(),
                );
            }
            let atlas = renderer::pbr::ReflectionProbes::from_bytes(file)?;
            let [width, height] = atlas.extent();
            let bytes = atlas.into_rgba32f();
            env.reflection_probes = Some(ibl::Image {
                format: renderer_core::ImageFormat::Rgba32Float,
                width,
                height,
                mips: std::iter::once(0..bytes.len()).collect(),
                bytes,
            });
            env.identity["reflection_probes_sha256"] = serde_json::json!(digest(file));
            look["reflection_probes_sha256"] = env.identity["reflection_probes_sha256"].clone();
            look["reflection_view"] = serde_json::json!(format!("{:?}", self.reflection_debug));
        }
        if let Some(path) = &self.baked_lighting {
            let bytes = std::fs::read(path)?;
            let (identity, file) = bytes
                .split_at_checked(64)
                .ok_or("invalid Bistro probe asset")?;
            let lighting = renderer::pbr::SurfaceLightmaps::from_bytes(file)?;
            let (pages, placements, volume) = lighting.into_parts();
            let matches = match volume.scenario_count() {
                1 | 2 => matches_bake(identity),
                3 => std::iter::once(&base).chain(hdr.iter()).any(|id| {
                    camera::daylight_sequence_identity(id, self.look).as_bytes() == identity
                }),
                _ => false,
            };
            if !matches {
                return Err(
                    "baked lighting belongs to a different Bistro cook or lighting preset".into(),
                );
            }
            env.identity["diffuse_scenarios"] = serde_json::json!(volume.scenario_count());
            env.identity["diffuse_sh_order"] =
                serde_json::json!(format!("{:?}", volume.sh_order()));
            let mut triangles = 0u64;
            for (id, address) in placements {
                let p = scene
                    .primitives
                    .get_mut(id as usize)
                    .ok_or("unknown lightmap placement")?;
                if p.lightmap.is_none() || scene.materials[p.material].blend {
                    return Err("invalid baked placement".into());
                }
                triangles += u64::from(p.index_count / 3);
                p.surface = Some(address);
            }
            if scene.primitives.iter().any(|p| {
                let m = &scene.materials[p.material];
                !m.blend && !m.alpha_zero && p.surface.is_none()
            }) {
                return Err("incomplete static lightmap coverage".into());
            }
            env.lightmaps = pages
                .into_iter()
                .map(|page| {
                    let [width, height] = page.extent();
                    let bytes = page.into_rgba32f();
                    ibl::Image {
                        format: renderer_core::ImageFormat::Rgba32Float,
                        width,
                        height,
                        mips: std::iter::once(0..bytes.len()).collect(),
                        bytes,
                    }
                })
                .collect();
            env.identity["static_triangles"] = serde_json::json!(triangles);
            let [width, height] = volume.extent();
            let bytes = volume.into_rgba32f();
            env.diffuse = ibl::Image {
                format: renderer_core::ImageFormat::Rgba32Float,
                width,
                height,
                mips: std::iter::once(0..bytes.len()).collect(),
                bytes,
            };
            env.identity["baked_lighting_sha256"] = serde_json::json!(digest(file));
        }
        env.identity["scenario_blend"] = serde_json::json!(self.scenario_blend);
        let light = camera::BakeScenario::for_look(self.scenario_blend, self.look);
        conditions.light_direction = light.direction;
        conditions.light_color = light.sun;
        conditions.background = light.sky;
        look["scenario_blend"] = serde_json::json!(self.scenario_blend);
        look["time_sweep"] = serde_json::json!(self.sweep);
        look["baked_lighting_sha256"] = env.identity["baked_lighting_sha256"].clone();
        look["gi_view"] = serde_json::json!(format!("{:?}", self.gi_debug));
        look["reflection_spheres"] = serde_json::json!(self.reflection_spheres);
        look["reflection_spheres_to"] = serde_json::json!(self.reflection_spheres_to);
        look["white_sphere"] = serde_json::json!(self.ball);
        look["direction_to_light"] = serde_json::json!(conditions.light_direction);
        look["irradiance_rgb"] = serde_json::json!(conditions.light_color);
        look["background_linear_rgb"] = serde_json::json!(conditions.background);
        Ok(())
    }
    #[cfg(feature = "tracy")]
    fn capture_settings(&self) -> trace::CaptureSettings<'_> {
        trace::CaptureSettings {
            api: &self.api,
            warmup: self.warmup,
            frames: self.frames,
            fixture: self.fixture,
            manifest: self.manifest.as_deref(),
            output: &self.output,
        }
    }

    fn forward_program(
        &self,
        blend: bool,
    ) -> Cow<'_, renderer_core::program::ManagedRasterProgram> {
        match self
            .shader_assets
            .as_ref()
            .and_then(|assets| assets.programs.as_ref())
        {
            Some(programs) => Cow::Borrowed(if blend {
                &programs.blend
            } else {
                &programs.forward
            }),
            None => Cow::Owned(self.pipeline.forward_program(blend)),
        }
    }

    fn shadow_program(&self) -> Cow<'_, renderer_core::program::ManagedRasterProgram> {
        match self
            .shader_assets
            .as_ref()
            .and_then(|assets| assets.programs.as_ref())
        {
            Some(programs) => Cow::Borrowed(&programs.shadow),
            None => Cow::Owned(program::shadow_program()),
        }
    }

    fn sdr_output(&self) -> renderer::passes::output::SdrOutput {
        use renderer::passes::output::{SdrEncoding, SdrOutput};
        SdrOutput {
            encoding: if self.windowed {
                SdrEncoding::Linear
            } else {
                SdrEncoding::Srgb
            },
            fxaa: self.aa,
        }
    }

    fn captures_aa_off(&self) -> bool {
        !self.windowed && self.aa
    }

    fn validate_lighting_extent(&self) -> Result<(), Error> {
        if self.pipeline.uses_local_lights() {
            program::forward_plus::active_tile_count(self.width, self.height)?;
        }
        Ok(())
    }

    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        let mut result = Self {
            api: DEFAULT_API.into(),
            manifest: None,
            ibl_manifest: None,
            reflection_probes: None,
            reflection_debug: Default::default(),
            reflection_spheres: None,
            reflection_spheres_to: None,
            baked_lighting: None,
            scenario_blend: 0.,
            lighting_pass: LightingPass::Beauty,
            gi_debug: Default::default(),
            sweep: false,
            ball: None,
            fixture: false,
            look: camera::Look::Reference,
            view: String::new(),
            width: 1920,
            height: 1080,
            frames: 120,
            warmup: 24,
            inflight: 2,
            gpu_timing: false,
            steady_state_cpu: false,
            alpha_only: false,
            force_opaque: false,
            capture_all_views: false,
            route_capture: None,
            capture_bloom_bypass: false,
            windowed: false,
            window_smoke: false,
            frame_limit: None,
            vsync: true,
            resource_checks: false,
            bloom: Bloom::Off,
            sample_count: renderer_core::graph::SampleCount::X1,
            pipeline: PipelineMode::Forward,
            shader_assets: None,
            instances: instancing::Mode::Off,
            submission: SubmissionMode::Direct,
            #[cfg(feature = "development-validation")]
            omit_material: None,
            #[cfg(feature = "development-validation")]
            offset_shadow_receiver: false,
            aa: true,
            shadows: true,
            rect_lights: RectLights::Shadowed,
            reuse_shadows: true,
            capture_shadows: false,
            #[cfg(feature = "tracy")]
            capture_handshake: false,
            output: PathBuf::from(".tools/bistro-runtime"),
        };
        let mut args = arguments.into_iter();
        let mut selected_view = None;
        let mut specified_size = false;
        let mut shader_manifest = None;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--rect-lights" => {
                    result.rect_lights = match args.next().as_deref() {
                        Some("off") => RectLights::Off,
                        Some("unshadowed") => RectLights::Unshadowed,
                        Some("shadowed") => RectLights::Shadowed,
                        _ => return Err("expected off, unshadowed or shadowed".into()),
                    }
                }
                "--fixture" => result.fixture = true,
                "--gpu-timing" => result.gpu_timing = true,
                "--steady-state-cpu" => result.steady_state_cpu = true,
                "--alpha-only" => result.alpha_only = true,
                "--capture-all-views" => result.capture_all_views = true,
                "--capture-route-samples" => {
                    result.route_capture = Some(camera::RouteCapture::Samples)
                }
                "--capture-route-clip" => result.route_capture = Some(camera::RouteCapture::Clip),
                "--capture-bloom-bypass" => result.capture_bloom_bypass = true,
                "--windowed" => result.windowed = true,
                "--window-smoke" => {
                    result.windowed = true;
                    result.window_smoke = true;
                }
                "--vsync" => {
                    result.vsync = match args.next().as_deref() {
                        Some("on") => true,
                        Some("off") => false,
                        _ => return Err("--vsync needs on or off".into()),
                    };
                }
                #[cfg(feature = "development-validation")]
                "--omit-material" => {
                    result.omit_material =
                        Some(args.next().ok_or("missing material index")?.parse()?);
                }
                #[cfg(feature = "development-validation")]
                "--offset-shadow-receiver" => result.offset_shadow_receiver = true,
                "--resource-checks" => result.resource_checks = true,
                "--bloom" => {
                    result.bloom = match args.next().as_deref() {
                        Some("off") => Bloom::Off,
                        Some("tent") => Bloom::Tent,
                        Some("box") => Bloom::Box,
                        _ => return Err("--bloom needs off, tent or box".into()),
                    };
                }
                "--aa-off" => result.aa = false,
                "--pipeline" => {
                    result.pipeline = match args.next().as_deref() {
                        Some("forward") => PipelineMode::Forward,
                        Some("forward-plus") => PipelineMode::ForwardPlus,
                        Some("forward-reference") => PipelineMode::ForwardReference,
                        Some("deferred") => PipelineMode::Deferred,
                        _ => return Err(
                            "--pipeline needs forward, forward-plus, forward-reference or deferred"
                                .into(),
                        ),
                    };
                }
                "--shader-manifest" => {
                    shader_manifest =
                        Some(args.next().ok_or("--shader-manifest needs a path")?.into());
                }
                "--submission" => {
                    result.submission = match args.next().as_deref() {
                        Some("direct") => SubmissionMode::Direct,
                        Some("packed") => SubmissionMode::Packed,
                        Some("indirect") => SubmissionMode::Indirect,
                        _ => return Err("--submission needs direct, packed or indirect".into()),
                    };
                }
                "--msaa" => {
                    use renderer_core::graph::SampleCount;
                    result.sample_count = match args.next().as_deref() {
                        Some("1") => SampleCount::X1,
                        Some("4") => SampleCount::X4,
                        _ => return Err("--msaa needs 1 or 4".into()),
                    };
                }
                "--instances" => {
                    result.instances = match args.next().as_deref() {
                        Some("off") => instancing::Mode::Off,
                        Some("individual") => instancing::Mode::Individual,
                        Some("instanced") => instancing::Mode::Instanced,
                        _ => return Err("--instances needs off, individual or instanced".into()),
                    };
                }
                "--sun-shadows-off" => result.shadows = false,
                "--redraw-shadows" => result.reuse_shadows = false,
                "--capture-shadow-maps" => result.capture_shadows = true,
                #[cfg(feature = "tracy")]
                "--capture-handshake" => result.capture_handshake = true,
                "--api" => result.api = args.next().ok_or("--api needs vulkan or d3d12")?,
                "--manifest" => {
                    result.manifest = Some(args.next().ok_or("--manifest needs a path")?.into())
                }
                "--ibl-manifest" => {
                    result.ibl_manifest =
                        Some(args.next().ok_or("--ibl-manifest needs a path")?.into())
                }
                "--lighting-pass" => {
                    result.lighting_pass = args.next().ok_or("missing lighting pass")?.parse()?
                }
                "--gi-debug" => {
                    result.gi_debug = match args.next().as_deref() {
                        Some("irradiance") => renderer::pbr::BakedLightingView::Irradiance,
                        Some("sources") => renderer::pbr::BakedLightingView::Sources,
                        Some("weights") => renderer::pbr::BakedLightingView::ProbeWeights,
                        _ => return Err("--gi-debug needs irradiance, sources or weights".into()),
                    }
                }
                "--time-sweep" => result.sweep = true,
                "--white-sphere" => {
                    let mut xyz = [0f32; 3];
                    for v in &mut xyz {
                        *v = args.next().ok_or("white sphere needs X Y Z")?.parse()?;
                    }
                    if xyz.iter().any(|v| !v.is_finite()) {
                        return Err("invalid white sphere position".into());
                    }
                    result.ball = Some(xyz);
                }
                "--reflection-probes" => {
                    result.reflection_probes =
                        Some(args.next().ok_or("missing reflection asset")?.into())
                }
                "--reflection-debug" => {
                    result.reflection_debug = match args.next().as_deref() {
                        Some("weights") => renderer::pbr::ReflectionProbeView::Weights,
                        Some("radiance") => renderer::pbr::ReflectionProbeView::Radiance,
                        _ => return Err("expected reflection weights or radiance".into()),
                    }
                }
                "--reflection-spheres" | "--reflection-spheres-to" => {
                    let mut pos = [0f32; 3];
                    for v in &mut pos {
                        *v = args
                            .next()
                            .ok_or("reflection spheres need X Y Z")?
                            .parse()?;
                    }
                    if !pos.into_iter().all(f32::is_finite) {
                        return Err("invalid sphere position".into());
                    }
                    if argument == "--reflection-spheres" {
                        result.reflection_spheres = Some(pos);
                    } else {
                        result.reflection_spheres_to = Some(pos);
                    }
                }
                "--baked-lighting" => {
                    result.baked_lighting =
                        Some(args.next().ok_or("missing baked lighting file")?.into())
                }
                "--scenario-blend" => {
                    result.scenario_blend = args.next().ok_or("missing scenario blend")?.parse()?
                }
                "--look" => result.look = args.next().ok_or("--look needs a preset")?.parse()?,
                "--view" => {
                    selected_view = Some(args.next().ok_or("--view needs a fixed name or route")?);
                }
                "--width" => {
                    result.width = args.next().ok_or("missing width")?.parse()?;
                    specified_size = true;
                }
                "--height" => {
                    result.height = args.next().ok_or("missing height")?.parse()?;
                    specified_size = true;
                }
                "--frames" => {
                    result.frames = args.next().ok_or("missing frames")?.parse()?;
                    result.frame_limit = Some(result.frames);
                }
                "--warmup" => result.warmup = args.next().ok_or("missing warmup")?.parse()?,
                "--inflight" => result.inflight = args.next().ok_or("missing inflight")?.parse()?,
                "--output" => result.output = args.next().ok_or("missing output directory")?.into(),
                "--alpha-override" => {
                    if args.next().as_deref() != Some("opaque") {
                        return Err("only --alpha-override opaque is supported".into());
                    }
                    result.force_opaque = true;
                }
                _ => return Err(format!("unknown option: {argument}").into()),
            }
        }
        result.view = selected_view.unwrap_or_else(|| match result.look {
            camera::Look::O3deAtrium => "renderer".into(),
            _ => "street".into(),
        });
        if !matches!(result.api.as_str(), "vulkan" | "d3d12") {
            return Err("API must be vulkan or d3d12".into());
        }
        if result.fixture == result.manifest.is_some() {
            return Err("select exactly one of --fixture or --manifest".into());
        }
        if let Some(manifest) = shader_manifest {
            if result.pipeline != PipelineMode::Forward
                || result.sample_count != renderer_core::graph::SampleCount::X1
                || result.submission != SubmissionMode::Direct
                || result.instances.enabled()
            {
                return Err(
                    "--shader-manifest requires Forward 1x, direct submission and instancing off"
                        .into(),
                );
            }
            result.shader_assets = Some(shader_assets::Selection {
                manifest,
                programs: None,
            });
        }
        if result.pipeline == PipelineMode::Deferred
            && result.sample_count != renderer_core::graph::SampleCount::X1
        {
            return Err("the deferred pipeline requires --msaa 1".into());
        }
        if result.instances.enabled()
            && (result.pipeline == PipelineMode::ForwardReference
                || result.submission != SubmissionMode::Direct
                || result.alpha_only
                || result.force_opaque
                || result.resource_checks)
        {
            return Err("--instances requires a standard PBR technique, direct submission and original material inputs without resource probes".into());
        }
        #[cfg(feature = "development-validation")]
        if result.instances.enabled() && result.omit_material.is_some() {
            return Err("--instances requires all original materials".into());
        }
        if result.look == camera::Look::O3deAtrium
            && (result.pipeline == PipelineMode::ForwardReference || result.shader_assets.is_some())
        {
            return Err("Sponza rectangles require a standard PBR pipeline".into());
        }
        if result.fixture && result.look != camera::Look::Reference {
            return Err("the selected lighting preset requires a scene manifest".into());
        }
        if result.fixture && !specified_size {
            result.width = 128;
            result.height = 96;
        }
        if result.width == 0
            || result.height == 0
            || result.width > 8192
            || result.height > 8192
            || result.frames == 0
            || result.frames > 100_000
            || result.warmup > 100_000
            || !(1..=4).contains(&result.inflight)
        {
            return Err("invalid extent/frame/in-flight limits".into());
        }
        #[cfg(feature = "development-validation")]
        if result.offset_shadow_receiver && result.baked_lighting.is_some() {
            return Err(
                "the offset-shadow-receiver control shader does not sample surface lightmaps"
                    .into(),
            );
        }
        if result.steady_state_cpu && result.warmup < result.inflight {
            return Err("stationary CPU measurement requires a fully warmed pipeline".into());
        }
        if result.reflection_spheres_to.is_some()
            && (result.reflection_spheres.is_none()
                || result.instances.enabled()
                || result.submission != SubmissionMode::Direct
                || result.pipeline == PipelineMode::ForwardReference
                || result.frames < 2
                || result.shader_assets.is_some())
        {
            return Err("moving inspection spheres require a start position, at least two frames and the standard direct pipeline without the instance grid".into());
        }
        if !(0.0..=1.0).contains(&result.scenario_blend)
            || (((result.baked_lighting.is_none() && result.reflection_probes.is_none())
                || result.sweep)
                && result.scenario_blend != 0.)
            || ((result.baked_lighting.is_some() || result.reflection_probes.is_some())
                && result.shader_assets.is_some())
        {
            return Err(
                "baked lighting requires built-in shaders and a fixed blend in [0,1] or --time-sweep"
                    .into(),
            );
        }
        if !matches!(result.gi_debug, renderer::pbr::BakedLightingView::Shaded)
            && result.reflection_debug != renderer::pbr::ReflectionProbeView::Shaded
        {
            return Err("select one lighting inspection view at a time".into());
        }
        if result.reflection_probes.is_none()
            && result.reflection_debug != renderer::pbr::ReflectionProbeView::Shaded
        {
            return Err("reflection inspection requires --reflection-probes".into());
        }
        if result.baked_lighting.is_none()
            && ((result.sweep && result.reflection_probes.is_none())
                || !matches!(result.gi_debug, renderer::pbr::BakedLightingView::Shaded))
        {
            return Err("GI inspection requires --baked-lighting".into());
        }
        if result.lighting_pass != LightingPass::Beauty
            && (result.pipeline != PipelineMode::Forward
                || result.shader_assets.is_some()
                || !matches!(result.gi_debug, renderer::pbr::BakedLightingView::Shaded)
                || result.reflection_debug != renderer::pbr::ReflectionProbeView::Shaded)
        {
            return Err(
                "lighting passes require standard forward shading without other inspection views"
                    .into(),
            );
        }
        result.validate_lighting_extent()?;
        if result.route_capture.is_some() && result.fixture {
            return Err("route samples require a cooked Bistro scene".into());
        }
        if result.capture_bloom_bypass && result.bloom == Bloom::Off {
            return Err("Bloom bypass capture requires an active Bloom mode".into());
        }
        if result.windowed
            && (result.capture_all_views
                || result.route_capture.is_some()
                || result.capture_bloom_bypass
                || result.capture_shadows
                || result.resource_checks
                || result.steady_state_cpu
                || result.alpha_only
                || result.force_opaque)
        {
            return Err("offscreen capture and resource probes require offscreen mode".into());
        }
        if result.resource_checks
            && (!cfg!(feature = "memory-profiling")
                || result.inflight < 2
                || result.alpha_only
                || result.force_opaque)
        {
            return Err("resource checks require memory-profiling, at least two in-flight slots and original full-scene alpha".into());
        }
        Ok(result)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
const WAIT: Duration = Duration::from_secs(60);

fn print_help() {
    println!(
        "bistro (--fixture | --manifest FILE --ibl-manifest FILE) [OPTIONS]

Scenes and output
  --api vulkan|d3d12             Default: {DEFAULT_API}
  --output DIR                   Default: .tools/bistro-runtime
  --baked-lighting FILE --scenario-blend 0..1     Baked day-to-dusk indirect and direct light
  --reflection-probes FILE [--reflection-debug weights|radiance] [--reflection-spheres X Y Z] [--reflection-spheres-to X Y Z]
  --lighting-pass beauty|base-color|direct|diffuse|diffuse-no-ao|specular|local-specular|emissive
  --time-sweep | --gi-debug irradiance|sources|weights | --white-sphere X Y Z
  --look reference|lumberyard-day|o3de-atrium
  --view bistro-corner|street|overview|material-detail|glass-detail|label-detail|street-mid|street-end|route|gallery|renderer
  --width N --height N
  --windowed | --window-smoke    Requires Windows and the presentation feature
  --vsync on|off                 Default: on

Rendering
  --pipeline forward|forward-plus|forward-reference|deferred
  --shader-manifest JSON         Reviewed shader bundle for Forward 1x, direct submission
  --msaa 1|4
  --bloom off|tent|box
  --instances off|individual|instanced
  --submission direct|packed|indirect
  --aa-off --sun-shadows-off --redraw-shadows

Measurement and captures
  --frames N --warmup N --inflight 1..4
  --rect-lights off|unshadowed|shadowed  Sponza atrium emitters (default shadowed)
  --gpu-timing
  --steady-state-cpu             Requires warmup >= inflight
  --resource-checks              Requires memory-profiling and inflight >= 2
  --capture-all-views --capture-route-samples | --capture-route-clip (2 seconds, 30 fps)
  --capture-bloom-bypass --capture-shadow-maps
  --alpha-only --alpha-override opaque"
    );
    #[cfg(feature = "development-validation")]
    println!("  --omit-material INDEX --offset-shadow-receiver");
    #[cfg(feature = "tracy")]
    println!("  --capture-handshake");
}

fn require_backend(api: &str) -> Result<(), Error> {
    if (cfg!(feature = "vulkan") && api == "vulkan")
        || (cfg!(all(windows, feature = "d3d12")) && api == "d3d12")
    {
        Ok(())
    } else {
        Err("selected backend was not compiled into this example".into())
    }
}

pub fn main() -> Result<(), Error> {
    let mut arguments = std::env::args().skip(1).peekable();
    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
        print_help();
        return Ok(());
    }
    let mut options = Options::parse(arguments)?;
    if let Some(shader_assets) = &mut options.shader_assets {
        require_backend(&options.api)?;
        shader_assets.prepare(&options.api)?;
    }
    #[cfg(feature = "tracy")]
    let _client = tracy_client::Client::start();
    #[cfg(feature = "tracy")]
    tracy_client::set_thread_name!("Bistro main");
    if options.windowed {
        #[cfg(all(
            windows,
            feature = "presentation",
            any(feature = "vulkan", feature = "d3d12")
        ))]
        {
            require_backend(&options.api)?;
            return window::execute(options);
        }
        #[cfg(not(all(
            windows,
            feature = "presentation",
            any(feature = "vulkan", feature = "d3d12")
        )))]
        return Err("windowed Bistro requires Windows and the presentation feature".into());
    }
    require_backend(&options.api)?;
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    return run::execute(&options);
    #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(values: &[&str]) -> Result<Options, Error> {
        Options::parse(values.iter().map(|value| value.to_string()))
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub(super) fn build_material_test_frame(
        scene: &asset::Scene,
        environment: &ibl::Environment,
        options: &Options,
        uniform: &crate::shared::camera::CameraInput,
        background: [f32; 3],
    ) -> Result<
        (
            renderer_core::runtime::Runtime,
            pipeline::Resident,
            pipeline::Built,
        ),
        Error,
    > {
        let mut budget = pipeline::Budget::new(scene, environment, options, background)?;
        let mut backend = backend::create(options, &budget)?;
        let resident =
            pipeline::Resident::upload(&mut backend, scene, environment, &budget, uniform)?;
        let mut pool = renderer_core::graph::GraphPool::new(budget.graph, 2)?;
        let mut workspace = pipeline::Workspace::new(
            scene,
            budget.prepared.take().expect("prepared fixture pipeline"),
        );
        let built = pipeline::build(
            &mut pool,
            &mut workspace,
            &resident,
            scene,
            options,
            uniform,
            0,
            true,
        )?;
        Ok((backend, resident, built))
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    #[test]
    fn default_api_is_compiled_into_the_example() {
        let defaults = options(&["--fixture"]).unwrap();
        require_backend(&defaults.api).unwrap();
    }

    #[test]
    fn scene_presets_supply_the_default_and_explicit_camera() {
        for args in [
            vec!["--fixture"],
            vec!["--manifest", "sponza", "--look", "o3de-atrium"],
            vec![
                "--manifest",
                "sponza",
                "--look",
                "o3de-atrium",
                "--view",
                "gallery",
            ],
        ] {
            let selected = options(&args).unwrap();
            let conditions = camera::Conditions::parse(
                &serde_json::from_slice(selected.condition_bytes()).unwrap(),
            )
            .unwrap();
            conditions.camera(&selected.view, 0).unwrap();
        }
    }

    #[test]
    fn source_and_capacity_options_are_explicit() {
        assert!(options(&[]).is_err());
        assert!(options(&["--fixture", "--manifest", "x"]).is_err());
        assert!(options(&["--fixture", "--inflight", "0"]).is_err());
        assert!(options(&["--fixture"]).unwrap().reuse_shadows);
        assert!(
            !options(&["--fixture", "--redraw-shadows"])
                .unwrap()
                .reuse_shadows
        );
        assert!(options(&["--fixture", "--width", "0"]).is_err());
        assert!(options(&["--fixture", "--frames", "0"]).is_err());
        let fixture = options(&["--fixture", "--gpu-timing"]).unwrap();
        assert_eq!(
            (fixture.width, fixture.height, fixture.inflight),
            (128, 96, 2)
        );
        assert!(fixture.gpu_timing);
    }

    #[test]
    fn rejects_retired_pipeline_flags() {
        for flag in [
            "--scene-pipeline",
            "--shadow-pipeline",
            "--post-pipeline",
            "--shader-pipeline",
        ] {
            let error = options(&["--fixture", flag, "pipeline.yaml"]).unwrap_err();
            assert!(error.to_string().contains(flag));
        }
    }

    #[test]
    fn shader_manifest_requires_single_sample_direct_forward_without_instancing() {
        assert!(options(&["--fixture", "--shader-manifest"]).is_err());
        let with_bundle = ["--fixture", "--shader-manifest", "bundle.json"];
        let selected = options(&with_bundle).unwrap().shader_assets.unwrap();
        assert_eq!(selected.manifest, PathBuf::from("bundle.json"));
        for conflict in [
            ["--msaa", "4"],
            ["--pipeline", "deferred"],
            ["--submission", "indirect"],
            ["--instances", "instanced"],
        ] {
            assert!(options(&[with_bundle.as_slice(), &conflict].concat()).is_err());
        }
    }

    #[test]
    fn msaa_is_selected_independently_of_fxaa() {
        let defaults = options(&["--fixture"]).unwrap();
        assert_eq!(defaults.sample_count.as_u32(), 1);
        assert!(defaults.aa);
        let msaa = options(&["--fixture", "--msaa", "4", "--aa-off"]).unwrap();
        assert_eq!(msaa.sample_count.as_u32(), 4);
        assert!(!msaa.aa);
        assert!(options(&["--fixture", "--msaa", "2"]).is_err());
    }

    #[test]
    fn instance_comparison_is_opt_in_and_runs_in_standard_recipes() {
        assert_eq!(options(&["--fixture"]).unwrap().instances.draws(), 0);
        for (mode, draws) in [("individual", instancing::COUNT), ("instanced", 1)] {
            let chosen = options(&["--fixture", "--instances", mode, "--msaa", "4"]).unwrap();
            assert_eq!(chosen.instances.draws(), draws);
            assert_eq!(chosen.instances.name(), mode);
            assert!(options(&["--fixture", "--instances", mode, "--pipeline", "deferred"]).is_ok());
            assert!(
                options(&[
                    "--fixture",
                    "--instances",
                    mode,
                    "--pipeline",
                    "forward-plus",
                    "--msaa",
                    "4"
                ])
                .is_ok()
            );
            assert!(
                options(&[
                    "--fixture",
                    "--instances",
                    mode,
                    "--pipeline",
                    "forward-reference"
                ])
                .is_err()
            );
        }
        assert!(options(&["--fixture", "--instances", "wrong"]).is_err());
    }

    #[test]
    fn submission_modes_are_explicit() {
        assert_eq!(
            options(&["--fixture"]).unwrap().submission,
            SubmissionMode::Direct
        );
        assert_eq!(
            options(&["--fixture", "--submission", "packed"])
                .unwrap()
                .submission,
            SubmissionMode::Packed
        );
        assert_eq!(
            options(&["--fixture", "--submission", "indirect"])
                .unwrap()
                .submission,
            SubmissionMode::Indirect
        );
        assert!(options(&["--fixture", "--submission", "unknown"]).is_err());
        assert!(
            options(&[
                "--fixture",
                "--submission",
                "packed",
                "--instances",
                "instanced"
            ])
            .is_err()
        );
    }

    #[test]
    fn deferred_requires_single_sample_attachments() {
        assert_eq!(
            options(&["--fixture"]).unwrap().pipeline,
            PipelineMode::Forward
        );
        let deferred = options(&["--fixture", "--pipeline", "deferred"]).unwrap();
        assert_eq!(deferred.pipeline, PipelineMode::Deferred);
        let error = options(&["--fixture", "--msaa", "4", "--pipeline", "deferred"]).unwrap_err();
        assert_eq!(error.to_string(), "the deferred pipeline requires --msaa 1");
    }

    #[test]
    fn forward_local_lights_support_both_sample_counts_and_check_tile_capacity() {
        for mode in ["forward-plus", "forward-reference"] {
            for samples in ["1", "4"] {
                let opts = options(&["--fixture", "--pipeline", mode, "--msaa", samples]).unwrap();
                assert_eq!(opts.pipeline.name(), mode);
                assert!(opts.pipeline.uses_local_lights());
            }
            assert!(
                options(&[
                    "--fixture",
                    "--pipeline",
                    mode,
                    "--width",
                    "8192",
                    "--height",
                    "8192",
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn capture_handshake_is_only_available_with_tracy() {
        let parsed = options(&["--fixture", "--capture-handshake"]);
        #[cfg(feature = "tracy")]
        assert!(parsed.unwrap().capture_handshake);
        #[cfg(not(feature = "tracy"))]
        assert!(parsed.is_err());
    }

    #[test]
    fn stationary_cpu_requires_a_prepared_pending_window() {
        assert!(!options(&["--fixture"]).unwrap().steady_state_cpu);
        assert!(
            options(&[
                "--fixture",
                "--steady-state-cpu",
                "--warmup",
                "1",
                "--inflight",
                "2"
            ])
            .is_err()
        );
        assert!(
            options(&[
                "--fixture",
                "--steady-state-cpu",
                "--warmup",
                "2",
                "--inflight",
                "2"
            ])
            .unwrap()
            .steady_state_cpu
        );
    }

    #[test]
    fn resource_checks_require_counting_overlap_and_unmodified_alpha() {
        let parsed = options(&["--fixture", "--resource-checks"]);
        assert_eq!(parsed.is_ok(), cfg!(feature = "memory-profiling"));
        assert!(options(&["--fixture", "--resource-checks", "--inflight", "1"]).is_err());
        assert!(options(&["--fixture", "--resource-checks", "--alpha-only"]).is_err());
        assert!(
            options(&[
                "--fixture",
                "--resource-checks",
                "--alpha-override",
                "opaque"
            ])
            .is_err()
        );
    }
}
