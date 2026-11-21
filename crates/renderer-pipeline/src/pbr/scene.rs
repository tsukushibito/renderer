//! Shared preparation and Graph declarations for the standard PBR recipes.

use renderer::capacity::{GraphRequirements, Preparation, WorkingSet};
use renderer::graph::{
    Binding, BufferDesc, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageDesc,
    ImageHandle, ImageView, RasterPass, SampleCount,
};
use renderer::program::{ManagedRasterProgram, RasterPipelineRequest, SamplerDescriptor};

use crate::passes::{
    output::{Exposure, OutputImages, PreparedOutput},
    pbr::{CAMERA_BYTES, TransformTechnique, transformed_programs},
};

use super::{
    AlphaMode, Bounds, CASCADE_COUNT, CameraStorage, Config, Draw, Environment, Frame,
    FrameShadows, PreparedInstances, PreparedView, SceneImages, SceneLimits, ShadowSettings,
    ShadowSource, Sun, ViewInput,
    draw::{Batch, Bindings, image_binding, sampler_binding},
    instances::{InstanceBuffer, InstanceTier},
    raster::{SHADOW_IMAGES, add_shadow_pass},
};

/// Immutable Programs already accepted through the Renderer's trust boundary.
/// Custom Forward Programs must match the ABI of their respective roles.
pub struct Programs {
    pub opaque: ManagedRasterProgram,
    pub blend: ManagedRasterProgram,
    pub shadow: ManagedRasterProgram,
}

pub(crate) struct ScenePrograms {
    world: Programs,
    model: Option<Programs>,
}

impl ScenePrograms {
    pub(crate) fn new(world: Programs, model: Option<(InstanceTier, TransformTechnique)>) -> Self {
        Self {
            world,
            model: model.map(|(tier, technique)| transformed_programs(tier, technique)),
        }
    }

    pub(crate) fn raster_requests(
        &self,
        samples: SampleCount,
        include_shadows: bool,
    ) -> Vec<RasterPipelineRequest> {
        let mut raster = Vec::with_capacity(
            (2 + usize::from(include_shadows)) * (1 + usize::from(self.model.is_some())),
        );
        for programs in std::iter::once(&self.world).chain(self.model.iter()) {
            raster.push(pipeline(programs.opaque.clone(), samples));
            raster.push(pipeline(programs.blend.clone(), samples));
            if include_shadows {
                raster.push(pipeline(programs.shadow.clone(), SampleCount::X1));
            }
        }
        raster
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrawMode {
    Direct,
    Indirect,
}

/// Immutable recipe settings and scratch reused when declaring frames. The
/// Graph owns all recorded work, so prepare another frame after construction.
pub(crate) struct PreparedScene {
    config: Config,
    limits: SceneLimits,
    preparation: Preparation,
    output: Option<PreparedOutput>,
    programs: ScenePrograms,
    opaque_uses_lighting: bool,
    mode: DrawMode,
    instances: Option<PreparedInstances>,
    opaque: Batch,
    transparent: Batch,
    shadow: Batch,
}

impl PreparedScene {
    pub(crate) fn new(
        config: Config,
        limits: SceneLimits,
        programs: Programs,
        opaque_uses_lighting: bool,
        mode: DrawMode,
        model_technique: Option<TransformTechnique>,
    ) -> Result<Self, GraphError> {
        let tier = InstanceTier::for_limit(limits.max_transforms)?;
        if tier.is_some() && model_technique.is_none() {
            return Err(GraphError::invalid_input(
                "custom PBR Programs support only world-space indexed draws",
            ));
        }
        let programs = ScenePrograms::new(programs, tier.zip(model_technique));
        let [width, height] = config.extent;
        if !config.clear_color.into_iter().all(f32::is_finite) {
            return Err(GraphError::invalid_input("PBR clear color must be finite"));
        }
        let mut preparation = Preparation::new(limits.frames_in_flight);
        let transient_camera = config.camera_storage == CameraStorage::Transient;
        let upload_camera = config.camera_storage != CameraStorage::PreUploaded;
        if transient_camera {
            preparation.reserve_buffer(camera_desc(), 1)?;
        }
        let msaa = config.samples == SampleCount::X4;
        let hdr = ImageDesc::rgba16_float(width, height);
        preparation.reserve_image(hdr, 1)?;
        if msaa {
            preparation.reserve_image(hdr.with_sample_count(SampleCount::X4), 1)?;
        }
        preparation.reserve_image(
            ImageDesc::depth32(width, height).with_sample_count(config.samples),
            1,
        )?;
        if let Some(settings) = config.shadows {
            settings.validate()?;
            preparation.reserve_image(shadow_desc(settings), CASCADE_COUNT)?;
        }

        let render_shadows = config.shadows.is_some();
        let raster = programs.raster_requests(config.samples, render_shadows);
        let cascades = if render_shadows { CASCADE_COUNT } else { 0 };
        let casters = if render_shadows {
            limits.max_casters_per_cascade
        } else {
            0
        };
        let transforms = tier.is_some();
        let pages = tier.map_or(0, |tier| limits.max_transforms.div_ceil(tier.records()));
        let instances = tier
            .map(|tier| PreparedInstances::reserve(&mut preparation, tier, limits.max_transforms))
            .transpose()?;

        let shadow_draws = checked(casters.checked_mul(CASCADE_COUNT))?;
        let scene_accesses = std::iter::once(&programs.world)
            .chain(programs.model.iter())
            .map(|programs| {
                programs
                    .opaque
                    .bindings()
                    .len()
                    .max(programs.blend.bindings().len())
            })
            .max()
            .expect("the world-space Programs are always prepared")
            + 2
            + usize::from(mode == DrawMode::Indirect);
        let shadow_accesses = programs
            .model
            .as_ref()
            .unwrap_or(&programs.world)
            .shadow
            .bindings()
            .len()
            + 2;
        let accesses = checked(limits.max_draws.checked_mul(scene_accesses))?;
        let accesses =
            checked(accesses.checked_add(checked(shadow_draws.checked_mul(shadow_accesses))?))?;
        let material_bindings: usize = std::iter::once(&programs.world)
            .chain(programs.model.iter())
            .map(|programs| {
                programs.opaque.bindings().len()
                    + programs.blend.bindings().len()
                    + if render_shadows {
                        programs.shadow.bindings().len()
                    } else {
                        0
                    }
            })
            .sum();

        // Each material first uses the usual per-program cache. Additional
        // page/material pairs cannot exceed the number of recorded commands.
        let extra_pairs = limits.max_materials.saturating_mul(pages.saturating_sub(1));
        let extra_bindings = match &programs.model {
            Some(model) => {
                let scene =
                    checked(extra_pairs.min(limits.max_draws).checked_mul(
                        model.opaque.bindings().len() + model.blend.bindings().len(),
                    ))?;
                let shadows = checked(
                    extra_pairs
                        .min(shadow_draws)
                        .checked_mul(model.shadow.bindings().len()),
                )?;
                checked(scene.checked_add(shadows))?
            }
            None => 0,
        };

        preparation.add_requirements(
            GraphRequirements {
                resources: cascades + 2 + usize::from(transient_camera) + usize::from(msaa),
                passes: cascades + 2 + usize::from(upload_camera),
                accesses: checked(accesses.checked_add(cascades + 12))?,
                draws: checked(limits.max_draws.checked_add(shadow_draws))?,
                raster_constant_words: if transforms {
                    checked(
                        shadow_draws
                            .checked_mul(2)
                            .and_then(|n| n.checked_add(limits.max_draws)),
                    )?
                } else {
                    shadow_draws
                },
                bindings: checked(
                    limits
                        .max_materials
                        .checked_mul(material_bindings)
                        .and_then(|n| n.checked_add(extra_bindings)),
                )?,
                attachments: cascades + 4 + usize::from(msaa),
                name_bytes: 256,
                upload_bytes: if upload_camera { CAMERA_BYTES } else { 0 },
                ..Default::default()
            },
            WorkingSet {
                raster: &raster,
                ..Default::default()
            },
        )?;
        let output = config
            .output
            .map(|output| PreparedOutput::prepare(&mut preparation, config.extent, output))
            .transpose()?;
        Ok(Self {
            config,
            limits,
            preparation,
            output,
            programs,
            opaque_uses_lighting,
            mode,
            instances,
            opaque: Batch::prepare(
                limits.max_materials,
                limits.max_draws,
                pages,
                limits.max_draws,
            )?,
            transparent: Batch::prepare(
                limits.max_materials,
                limits.max_draws,
                pages,
                limits.max_draws,
            )?,
            shadow: Batch::prepare(
                if render_shadows {
                    limits.max_materials
                } else {
                    0
                },
                casters,
                pages,
                shadow_draws,
            )?,
        })
    }

    pub(crate) fn config(&self) -> Config {
        self.config
    }

    pub(crate) fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub(crate) fn preparation_mut(&mut self) -> &mut Preparation {
        &mut self.preparation
    }

    /// Calculate view and cascade projections before selecting draws.
    pub(crate) fn prepare_view(
        &self,
        view: ViewInput,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        PreparedView::prepare(
            view,
            sun,
            bounds,
            environment,
            self.config.extent,
            self.config.shadows,
        )
    }

    pub(crate) fn add_scene_using<const N: usize>(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        additional: impl FnOnce(&mut Graph, BufferHandle) -> Result<[Binding; N], GraphError>,
        append_scene: impl FnOnce(
            &mut Graph,
            RasterScene<'_>,
        ) -> Result<(ImageHandle, Option<ImageHandle>), GraphError>,
    ) -> Result<SceneImages, GraphError> {
        self.add_scene_using_programs(graph, frame, None, additional, append_scene)
    }

    pub(crate) fn add_scene_using_programs<const N: usize>(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        programs: Option<&ScenePrograms>,
        additional: impl FnOnce(&mut Graph, BufferHandle) -> Result<[Binding; N], GraphError>,
        append_scene: impl FnOnce(
            &mut Graph,
            RasterScene<'_>,
        ) -> Result<(ImageHandle, Option<ImageHandle>), GraphError>,
    ) -> Result<SceneImages, GraphError> {
        let transform_end = self.check_frame(graph, &frame)?;
        let camera = match frame.camera_target {
            Some(buffer) => buffer,
            None => graph.buffer("pbr camera", camera_desc())?,
        };
        if self.config.camera_storage != CameraStorage::PreUploaded {
            graph.upload_buffer("pbr camera upload", camera, 0, frame.view.uniform_bytes())?;
        }
        let transforms = match &mut self.instances {
            Some(instances) if transform_end != 0 => {
                Some(instances.append(graph, &frame.transforms[..transform_end])?)
            }
            _ => None,
        };
        let (shadow_views, shadow_images) = match (self.config.shadows, frame.shadows) {
            (Some(settings), FrameShadows::Cascades(sources)) => {
                let views =
                    self.add_shadows(graph, camera, &frame, settings, sources, transforms)?;
                (views, Some(views.map(ImageView::image)))
            }
            (None, FrameShadows::Disabled(placeholder)) => {
                ([placeholder.view(); CASCADE_COUNT], None)
            }
            _ => unreachable!("shadow inputs were checked before graph mutation"),
        };
        let programs = programs.unwrap_or(&self.programs);
        let lighting = lighting_bindings(frame.view.environment(), shadow_views);
        let extra = additional(graph, camera)?;
        let mut extended = [lighting[0]; renderer::program::MAX_RASTER_BINDINGS - 7];
        extended[..lighting.len()].copy_from_slice(&lighting);
        let count = lighting.len() + N;
        extended[lighting.len()..count].copy_from_slice(&extra);
        let opaque_bindings = Bindings::Scene {
            camera,
            lighting: if self.opaque_uses_lighting {
                &extended[..count]
            } else {
                &[]
            },
        };
        let transparent_bindings = Bindings::Scene {
            camera,
            lighting: &extended[..count],
        };
        let opaque_model = programs.model.as_ref().map(|model| &model.opaque);
        let blend_model = programs.model.as_ref().map(|model| &model.blend);
        self.opaque.reset_bindings(frame.materials.len());
        self.transparent.reset_bindings(frame.materials.len());
        self.opaque.record(
            graph,
            &programs.world.opaque,
            opaque_bindings,
            frame.materials,
            frame.opaque,
            opaque_model.zip(transforms),
        )?;
        self.transparent.record(
            graph,
            &programs.world.blend,
            transparent_bindings,
            frame.materials,
            frame.transparent,
            blend_model.zip(transforms),
        )?;
        let (hdr, depth) = append_scene(
            graph,
            RasterScene {
                camera,
                lighting,
                has_transparency: !frame.transparent.is_empty()
                    || frame.forward_draws.is_some_and(|draws| draws.has_blend()),
                forward_draws: frame.forward_draws,
                opaque: &self.opaque,
                transparent: &self.transparent,
                programs,
            },
        )?;
        Ok(SceneImages {
            hdr,
            depth,
            shadows: shadow_images,
            camera,
            gbuffer: None,
        })
    }

    /// Tone map and encode the scene's HDR result or a compatible custom HDR
    /// result with this recipe's prepared output settings.
    pub(crate) fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        let (output, exposure) = self.checked_output(exposure)?;
        output.append_validated(graph, graph.image_view(hdr, 0, 0)?, exposure)
    }

    pub(crate) fn checked_output(
        &self,
        exposure: f32,
    ) -> Result<(PreparedOutput, Exposure), GraphError> {
        let output = self
            .output
            .ok_or_else(|| GraphError::invalid_input("standard SDR output was not prepared"))?;
        Ok((output, PreparedOutput::validate_exposure(exposure)?))
    }

    fn check_frame(&self, graph: &Graph, frame: &Frame<'_>) -> Result<usize, GraphError> {
        if !frame.view.matches(self.config.extent, self.config.shadows) {
            return Err(GraphError::invalid_input(
                "PBR view is incompatible with the prepared recipe",
            ));
        }
        match (self.config.camera_storage, frame.camera_target) {
            (CameraStorage::Transient, None) => {}
            (CameraStorage::External | CameraStorage::PreUploaded, Some(camera))
                if graph.buffer_desc(camera)?.size >= CAMERA_BYTES as u64 => {}
            _ => {
                return Err(GraphError::invalid_input(
                    "PBR camera target does not match the prepared storage mode",
                ));
            }
        }
        check_limit(
            GraphCapacityKind::Bindings,
            self.limits.max_materials,
            Some(frame.materials.len()),
        )?;
        check_limit(
            GraphCapacityKind::Draws,
            self.limits.max_transforms,
            Some(frame.transforms.len()),
        )?;
        let has_model = self.instances.is_some();
        let (opaque_end, opaque_commands) =
            validate_draws(frame.opaque, self.mode, has_model, frame, DrawPass::Opaque)?;
        let (transparent_end, transparent_commands) = validate_draws(
            frame.transparent,
            DrawMode::Direct,
            has_model,
            frame,
            DrawPass::Transparent,
        )?;
        check_limit(
            GraphCapacityKind::Draws,
            self.limits.max_draws,
            opaque_commands.checked_add(transparent_commands),
        )?;
        let mut transform_end = opaque_end.max(transparent_end);
        match (self.config.shadows, frame.shadows) {
            (Some(settings), FrameShadows::Cascades(sources)) => {
                validate_shadow_sources(graph, sources, shadow_desc(settings))?;
                for (cascade, source) in sources.into_iter().enumerate() {
                    if let ShadowSource::Render { casters, .. } = source {
                        let (end, commands) = validate_draws(
                            casters,
                            DrawMode::Direct,
                            has_model,
                            frame,
                            DrawPass::Shadow(cascade),
                        )?;
                        check_limit(
                            GraphCapacityKind::Draws,
                            self.limits.max_casters_per_cascade,
                            Some(commands),
                        )?;
                        transform_end = transform_end.max(end);
                    }
                }
            }
            (None, FrameShadows::Disabled(placeholder)) => {
                graph.image_desc(placeholder.view().image())?;
            }
            _ => {
                return Err(GraphError::invalid_input(
                    "PBR shadow inputs do not match the prepared shadow setting",
                ));
            }
        }
        Ok(transform_end)
    }

    fn add_shadows(
        &mut self,
        graph: &mut Graph,
        camera: BufferHandle,
        frame: &Frame<'_>,
        settings: ShadowSettings,
        sources: [ShadowSource<'_>; CASCADE_COUNT],
        transforms: Option<InstanceBuffer>,
    ) -> Result<[ImageView; CASCADE_COUNT], GraphError> {
        let desc = shadow_desc(settings);
        let program = &self.programs.world.shadow;
        let instanced = self.programs.model.as_ref().map(|model| &model.shadow);
        let materials = frame.materials;
        self.shadow.reset_bindings(materials.len());
        let mut views = [None; CASCADE_COUNT];
        for (cascade, source) in sources.into_iter().enumerate() {
            let image = match source {
                ShadowSource::Reuse(image)
                | ShadowSource::Render {
                    target: Some(image),
                    ..
                } => image,
                ShadowSource::Render { target: None, .. } => {
                    graph.image(SHADOW_IMAGES[cascade], desc)?
                }
            };
            let view = graph.image_view(image, 0, 0)?;
            if let ShadowSource::Render { casters, .. } = source {
                self.shadow.record(
                    graph,
                    program,
                    Bindings::Shadow { camera, cascade },
                    materials,
                    casters,
                    instanced.zip(transforms),
                )?;
                add_shadow_pass(graph, cascade, view, |pass| {
                    self.shadow.draw(pass, program, instanced)?;
                    if let Some(draws) = frame.forward_draws {
                        draws.draw_shadow(pass, cascade)?;
                    }
                    Ok(())
                })?;
            }
            views[cascade] = Some(view);
        }
        Ok(views.map(Option::unwrap))
    }
}

pub(crate) struct RasterScene<'a> {
    pub(crate) camera: BufferHandle,
    pub(crate) lighting: [Binding; 9],
    pub(crate) has_transparency: bool,
    forward_draws: Option<&'a crate::pipeline::ForwardDraws>,
    opaque: &'a Batch,
    transparent: &'a Batch,
    programs: &'a ScenePrograms,
}

impl RasterScene<'_> {
    pub(crate) fn has_forward_opaque(&self) -> bool {
        self.forward_draws.is_some_and(|draws| draws.has_opaque())
    }
    pub(crate) fn draw_forward_opaque(
        &self,
        pass: &mut RasterPass<'_, '_>,
    ) -> Result<(), GraphError> {
        if let Some(draws) = self.forward_draws {
            draws.draw_opaque(pass)?
        }
        Ok(())
    }
    pub(crate) fn draw_opaque(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        self.draw_pbr_opaque(pass)?;
        self.draw_forward_opaque(pass)
    }
    pub(crate) fn draw_pbr_opaque(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        self.opaque.draw(
            pass,
            &self.programs.world.opaque,
            self.programs.model.as_ref().map(|model| &model.opaque),
        )
    }

    pub(crate) fn draw_transparent(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        if let Some(draws) = self.forward_draws {
            // StandardPipeline records one PBR command per sorted object, so
            // these ranges also index the recorded commands.
            return draws.draw_color(pass, |pass, range| {
                self.transparent.draw_range(
                    pass,
                    &self.programs.world.blend,
                    self.programs.model.as_ref().map(|model| &model.blend),
                    range,
                )
            });
        }
        self.transparent.draw(
            pass,
            &self.programs.world.blend,
            self.programs.model.as_ref().map(|model| &model.blend),
        )
    }
}

fn lighting_bindings(
    environment: Environment,
    shadows: [ImageView; CASCADE_COUNT],
) -> [Binding; 9] {
    [
        image_binding(2, 0, environment.irradiance),
        image_binding(2, 1, environment.reflection),
        image_binding(2, 2, environment.brdf_lut),
        sampler_binding(2, 3, SamplerDescriptor::LinearRepeatUClampV),
        image_binding(3, 0, shadows[0]),
        image_binding(3, 1, shadows[1]),
        image_binding(3, 2, shadows[2]),
        image_binding(3, 3, shadows[3]),
        sampler_binding(3, 4, SamplerDescriptor::NearestClamp),
    ]
}

#[derive(Clone, Copy)]
enum DrawPass {
    Opaque,
    Transparent,
    Shadow(usize),
}

impl DrawPass {
    fn name(self) -> &'static str {
        match self {
            Self::Opaque => "scene_opaque",
            Self::Transparent => "scene_transparent",
            Self::Shadow(cascade) => ["shadow_0", "shadow_1", "shadow_2", "shadow_3"][cascade],
        }
    }
}

fn validate_draws(
    draws: &[Draw],
    mode: DrawMode,
    has_model: bool,
    frame: &Frame<'_>,
    pass: DrawPass,
) -> Result<(usize, usize), GraphError> {
    let mut commands = draws.len();
    let wants_blend = matches!(pass, DrawPass::Transparent);
    let mut transform_end = 0;
    for (index, draw) in draws.iter().enumerate() {
        let (material, range) = match *draw {
            Draw::WorldSpaceIndexed(draw) => (draw.material, None),
            Draw::WorldSpaceIndirect { material, .. } if mode == DrawMode::Indirect => {
                (material, None)
            }
            Draw::TransformedIndirect {
                material,
                transform,
                ..
            } if has_model && mode == DrawMode::Indirect => (material, Some((transform, 1))),
            Draw::Instanced {
                geometry,
                first_transform,
                instance_count,
            } if has_model => (geometry.material, Some((first_transform, instance_count))),
            _ => {
                let kind = match draw {
                    Draw::WorldSpaceIndirect { .. } | Draw::TransformedIndirect { .. } => {
                        "indirect"
                    }
                    _ => "instanced",
                };
                return Err(GraphError::invalid_input(format!(
                    "PBR draw {index} uses {kind} geometry, which this recipe does not support in {}",
                    pass.name(),
                ))
                .with_pass_name(pass.name()));
            }
        };
        if let Some((first, count)) = range {
            let end = first
                .checked_add(count)
                .and_then(|end| usize::try_from(end).ok())
                .filter(|&end| count != 0 && end <= frame.transforms.len())
                .ok_or_else(|| {
                    GraphError::invalid_input("PBR instance range exceeds the transform table")
                        .with_pass_name(pass.name())
                })?;
            if count > 1 {
                commands = checked(commands.checked_add(draw.command_count() - 1))?;
            }
            transform_end = transform_end.max(end);
        }
        let factors = frame.materials.get(material).ok_or_else(|| {
            GraphError::invalid_input(format!(
                "PBR draw {index} has out-of-bounds material index {material}"
            ))
            .with_pass_name(pass.name())
        })?;
        let alpha_mode = factors.alpha_mode();
        if (alpha_mode == AlphaMode::Blend) != wants_blend {
            return Err(GraphError::invalid_input(format!(
                "{}: PBR draw {index} uses material {material} with incompatible alpha mode {alpha_mode:?}",
                pass.name(),
            ))
            .with_pass_name(pass.name()));
        }
    }
    Ok((transform_end, commands))
}

fn validate_shadow_sources(
    graph: &Graph,
    sources: [ShadowSource<'_>; CASCADE_COUNT],
    desc: ImageDesc,
) -> Result<(), GraphError> {
    let mut previous = [None; CASCADE_COUNT];
    for (cascade, source) in sources.into_iter().enumerate() {
        let (image, writes) = match source {
            ShadowSource::Reuse(image) => (image, false),
            ShadowSource::Render {
                target: Some(image),
                ..
            } => (image, true),
            ShadowSource::Render { target: None, .. } => continue,
        };
        graph.require_image_desc(image, desc)?;
        if previous[..cascade]
            .iter()
            .flatten()
            .any(|&(other, written)| other == image && (written || writes))
        {
            return Err(GraphError::invalid_input(
                "PBR cascades cannot share a writable shadow target",
            ));
        }
        previous[cascade] = Some((image, writes));
    }
    Ok(())
}

fn check_limit(
    kind: GraphCapacityKind,
    limit: usize,
    required: Option<usize>,
) -> Result<(), GraphError> {
    if required.is_some_and(|count| count <= limit) {
        Ok(())
    } else {
        Err(GraphError::capacity_error(kind, limit, required))
    }
}

fn shadow_desc(settings: ShadowSettings) -> ImageDesc {
    ImageDesc::depth32(settings.resolution, settings.resolution)
}

const fn camera_desc() -> BufferDesc {
    BufferDesc {
        size: CAMERA_BYTES as u64,
    }
}

fn pipeline(program: ManagedRasterProgram, sample_count: SampleCount) -> RasterPipelineRequest {
    RasterPipelineRequest {
        program,
        sample_count,
    }
}

fn checked(value: Option<usize>) -> Result<usize, GraphError> {
    value.ok_or_else(|| GraphError::invalid_input("PBR frame capacity overflow"))
}
