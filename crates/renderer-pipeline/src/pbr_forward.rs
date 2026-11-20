//! Standard PBR Forward with directional shadows and HDR output.
//!
//! Prepare the recipe before creating the Renderer. To insert an HDR pass,
//! call the scene and output stages separately.

pub use crate::pbr::raster::{add_scene_passes, add_shadow_pass};

use renderer::capacity::{GraphRequirements, Preparation, WorkingSet};
use renderer::graph::{
    Binding, BufferDesc, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageDesc,
    ImageHandle, ImageView, RasterPass, SampleCount,
};
use renderer::program::{ManagedRasterProgram, RasterPipelineRequest, SamplerDescriptor};

use crate::passes::output::{OutputImages, PreparedOutput, SdrOutput};
use crate::passes::pbr::{
    CAMERA_BYTES, blend_program, opaque_program, shadow_program, transformed_opaque_program,
    transformed_shadow_program,
};
use crate::pbr::{
    AlphaMode, Bounds, CASCADE_COUNT, Camera, CameraStorage, Draw, Environment, Frame, FrameImages,
    MAX_TRANSFORMS, PreparedInstances, PreparedView, SceneImages, SceneLimits, ShadowSettings,
    ShadowSource, Sun,
    draw::{Batch, Bindings, image_binding, sampler_binding},
    raster::SHADOW_IMAGES,
};

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub extent: [u32; 2],
    pub clear_color: [f32; 4],
    pub shadows: ShadowSettings,
    pub output: SdrOutput,
    pub samples: SampleCount,
    pub camera_storage: CameraStorage,
}

/// Immutable Programs already accepted through the Renderer's trust boundary.
/// Each Program must have the corresponding standard Forward ABI.
pub struct Programs {
    pub opaque: ManagedRasterProgram,
    pub blend: ManagedRasterProgram,
    pub shadow: ManagedRasterProgram,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            opaque: opaque_program(),
            blend: blend_program(),
            shadow: shadow_program(),
        }
    }
}

struct InstancePrograms {
    opaque: ManagedRasterProgram,
    shadow: ManagedRasterProgram,
    table: PreparedInstances,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrawMode {
    Direct,
    Indirect,
    Transformed,
}

/// Immutable recipe settings and scratch reused when declaring frames. The
/// Graph owns all recorded work, so prepare another frame after construction.
pub struct Prepared {
    config: Config,
    limits: SceneLimits,
    preparation: Preparation,
    output: PreparedOutput,
    programs: Programs,
    opaque_uses_lighting: bool,
    mode: DrawMode,
    instances: Option<InstancePrograms>,
    opaque: Batch,
    transparent: Batch,
    shadow: Batch,
}

pub fn prepare(config: Config, limits: SceneLimits) -> Result<Prepared, GraphError> {
    prepare_recipe(config, limits, Programs::default(), true)
}

/// Prepare Forward 1× with custom Programs for ordinary indexed draws.
pub fn prepare_with_programs(
    config: Config,
    limits: SceneLimits,
    programs: Programs,
) -> Result<Prepared, GraphError> {
    if config.samples != SampleCount::X1 {
        return Err(GraphError::invalid_input(
            "custom PBR Programs require Forward 1×",
        ));
    }
    let expected = Programs::default();
    for (actual, expected) in [
        (&programs.opaque, &expected.opaque),
        (&programs.blend, &expected.blend),
        (&programs.shadow, &expected.shadow),
    ] {
        validate_program(actual, expected)?;
    }
    prepare_variant(config, limits, programs, true, DrawMode::Direct)
}

/// Prepare both ordinary indexed geometry and model-space instance ranges in
/// the opaque and shadow passes. The transparent pass uses ordinary draws.
pub fn prepare_with_transforms(
    config: Config,
    limits: SceneLimits,
) -> Result<Prepared, GraphError> {
    prepare_variant(
        config,
        limits,
        Programs::default(),
        true,
        DrawMode::Transformed,
    )
}

pub(crate) fn prepare_recipe(
    config: Config,
    limits: SceneLimits,
    programs: Programs,
    opaque_uses_lighting: bool,
) -> Result<Prepared, GraphError> {
    prepare_variant(
        config,
        limits,
        programs,
        opaque_uses_lighting,
        DrawMode::Indirect,
    )
}

fn prepare_variant(
    config: Config,
    limits: SceneLimits,
    programs: Programs,
    opaque_uses_lighting: bool,
    mode: DrawMode,
) -> Result<Prepared, GraphError> {
    config.shadows.validate()?;
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
    preparation.reserve_image(shadow_desc(config.shadows), CASCADE_COUNT)?;

    let mut raster = Vec::with_capacity(5);
    raster.push(pipeline(programs.opaque.clone(), config.samples));
    raster.push(pipeline(programs.blend.clone(), config.samples));

    let render_shadows = config.shadows.enabled;
    let casters = if render_shadows {
        limits.max_casters_per_cascade
    } else {
        0
    };
    if render_shadows {
        raster.push(pipeline(programs.shadow.clone(), SampleCount::X1));
    }
    let transforms = mode == DrawMode::Transformed;
    let instances = if transforms {
        let opaque = transformed_opaque_program();
        let shadow = transformed_shadow_program();
        raster.push(pipeline(opaque.clone(), config.samples));
        if render_shadows {
            raster.push(pipeline(shadow.clone(), SampleCount::X1));
        }
        Some(InstancePrograms {
            opaque,
            shadow,
            table: PreparedInstances::reserve(&mut preparation)?,
        })
    } else {
        None
    };

    let shadow_draws = checked(casters.checked_mul(CASCADE_COUNT))?;
    let scene_accesses = programs
        .opaque
        .bindings()
        .len()
        .max(programs.blend.bindings().len())
        + 2
        + usize::from(mode != DrawMode::Direct);
    let shadow_accesses = programs.shadow.bindings().len() + 2 + usize::from(transforms);
    let accesses = checked(limits.max_draws.checked_mul(scene_accesses))?;
    let accesses =
        checked(accesses.checked_add(checked(shadow_draws.checked_mul(shadow_accesses))?))?;
    let shadow_bindings = if render_shadows {
        programs.shadow.bindings().len()
    } else {
        0
    };
    let mut material_bindings =
        programs.opaque.bindings().len() + programs.blend.bindings().len() + shadow_bindings;
    if let Some(instances) = &instances {
        material_bindings += instances.opaque.bindings().len();
        if render_shadows {
            material_bindings += instances.shadow.bindings().len();
        }
    }

    preparation.add_requirements(
        GraphRequirements {
            resources: CASCADE_COUNT + 2 + usize::from(transient_camera) + usize::from(msaa),
            passes: CASCADE_COUNT + 3 + usize::from(upload_camera),
            accesses: checked(accesses.checked_add(16))?,
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
            bindings: checked(limits.max_materials.checked_mul(material_bindings))?,
            attachments: 10 + usize::from(msaa),
            name_bytes: 256,
            upload_bytes: if upload_camera { CAMERA_BYTES } else { 0 },
            ..Default::default()
        },
        WorkingSet {
            raster: &raster,
            ..Default::default()
        },
    )?;
    let output = PreparedOutput::prepare(&mut preparation, config.extent, config.output)?;
    Ok(Prepared {
        config,
        limits,
        preparation,
        output,
        programs,
        opaque_uses_lighting,
        mode,
        instances,
        opaque: Batch::prepare(limits.max_materials, limits.max_draws, transforms)?,
        transparent: Batch::prepare(limits.max_materials, limits.max_draws, false)?,
        shadow: Batch::prepare(
            if render_shadows {
                limits.max_materials
            } else {
                0
            },
            casters,
            transforms,
        )?,
    })
}

fn validate_program(
    actual: &ManagedRasterProgram,
    expected: &ManagedRasterProgram,
) -> Result<(), GraphError> {
    let actual = actual.descriptor();
    let expected = expected.descriptor();
    if actual.vertex_stride != expected.vertex_stride
        || actual.vertex_attributes != expected.vertex_attributes
        || actual.parameters != expected.parameters
        || actual.bindings != expected.bindings
        || actual.binding_bytes != expected.binding_bytes
        || actual.sampling_pairs != expected.sampling_pairs
        || actual.color_outputs != expected.color_outputs
        || actual.blend != expected.blend
        || actual.depth != expected.depth
    {
        return Err(GraphError::invalid_input(
            "Program ABI is incompatible with its PBR Forward role",
        ));
    }
    Ok(())
}

impl Prepared {
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub(crate) fn preparation_mut(&mut self) -> &mut Preparation {
        &mut self.preparation
    }

    /// Calculate view and cascade projections before selecting draws.
    pub fn prepare_view(
        &self,
        camera: Camera,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        PreparedView::prepare(
            camera,
            sun,
            bounds,
            environment,
            self.config.extent,
            self.config.shadows,
        )
    }

    /// Build shadows, scene and SDR output. Discard the Graph on declaration
    /// failure and start the next attempt with a reset Graph.
    pub fn add_passes(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
    ) -> Result<FrameImages, GraphError> {
        let exposure = frame.exposure;
        let scene = self.add_scene(graph, frame)?;
        let output = self.add_output(graph, scene.hdr, exposure)?;
        Ok(FrameImages {
            hdr: scene.hdr,
            depth: scene.depth,
            sdr: output.sdr,
            shadows: scene.shadows,
            camera: scene.camera,
        })
    }

    /// Build the scene and return resolved HDR for further processing.
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        let Config {
            extent,
            clear_color,
            samples,
            ..
        } = self.config;
        self.add_scene_using(
            graph,
            frame,
            |_, _| Ok([]),
            |graph, scene| {
                add_scene_passes(
                    graph,
                    extent,
                    clear_color,
                    samples,
                    scene.has_transparency,
                    |pass| scene.draw_opaque(pass),
                    |pass| scene.draw_transparent(pass),
                )
            },
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
        self.check_frame(graph, &frame)?;
        validate_shadow_sources(graph, frame.shadows, shadow_desc(self.config.shadows))?;
        let camera = match frame.camera_target {
            Some(buffer) => buffer,
            None => graph.buffer("pbr camera", camera_desc())?,
        };
        if self.config.camera_storage != CameraStorage::PreUploaded {
            graph.upload_buffer("pbr camera upload", camera, 0, frame.view.uniform_bytes())?;
        }
        let transforms = match &mut self.instances {
            Some(instances) if !frame.transforms.is_empty() => {
                Some(instances.table.append(graph, frame.transforms)?)
            }
            _ => None,
        };
        let shadow_views =
            self.add_shadows(graph, camera, frame.materials, frame.shadows, transforms)?;
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
        let instanced_program = self.instances.as_ref().map(|instances| &instances.opaque);
        self.opaque.reset_bindings(frame.materials.len());
        self.transparent.reset_bindings(frame.materials.len());
        self.opaque.record(
            graph,
            &self.programs.opaque,
            opaque_bindings,
            frame.materials,
            frame.opaque,
            instanced_program.zip(transforms),
        )?;
        self.transparent.record(
            graph,
            &self.programs.blend,
            transparent_bindings,
            frame.materials,
            frame.transparent,
            None,
        )?;
        let (hdr, depth) = append_scene(
            graph,
            RasterScene {
                camera,
                lighting,
                has_transparency: !frame.transparent.is_empty(),
                opaque: &self.opaque,
                transparent: &self.transparent,
                programs: &self.programs,
                instanced: instanced_program,
            },
        )?;
        Ok(SceneImages {
            hdr,
            depth,
            shadows: shadow_views.map(ImageView::image),
            camera,
        })
    }

    /// Tone map and encode the scene's HDR result or a compatible custom HDR
    /// result with this recipe's prepared output settings.
    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        let input = graph.image_view(hdr, 0, 0)?;
        self.output.add_passes(graph, input, exposure)
    }

    fn check_frame(&self, graph: &Graph, frame: &Frame<'_>) -> Result<(), GraphError> {
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
            self.limits.max_draws,
            frame.opaque.len().checked_add(frame.transparent.len()),
        )?;
        let transform_limit = if self.mode == DrawMode::Transformed {
            MAX_TRANSFORMS
        } else {
            0
        };
        check_limit(
            GraphCapacityKind::Draws,
            transform_limit,
            Some(frame.transforms.len()),
        )?;
        validate_draws(frame.opaque, self.mode, frame, DrawPass::Opaque)?;
        validate_draws(
            frame.transparent,
            DrawMode::Direct,
            frame,
            DrawPass::Transparent,
        )?;
        if self.config.shadows.enabled {
            for (cascade, source) in frame.shadows.into_iter().enumerate() {
                if let ShadowSource::Render { casters, .. } = source {
                    check_limit(
                        GraphCapacityKind::Draws,
                        self.limits.max_casters_per_cascade,
                        Some(casters.len()),
                    )?;
                    let mode = if self.mode == DrawMode::Transformed {
                        self.mode
                    } else {
                        DrawMode::Direct
                    };
                    validate_draws(casters, mode, frame, DrawPass::Shadow(cascade))?;
                }
            }
        }
        Ok(())
    }

    fn add_shadows(
        &mut self,
        graph: &mut Graph,
        camera: BufferHandle,
        materials: &[crate::pbr::Material],
        sources: [ShadowSource<'_>; CASCADE_COUNT],
        transforms: Option<BufferHandle>,
    ) -> Result<[ImageView; CASCADE_COUNT], GraphError> {
        let settings = self.config.shadows;
        let desc = shadow_desc(settings);
        let program = &self.programs.shadow;
        let instanced = self.instances.as_ref().map(|instances| &instances.shadow);
        if settings.enabled {
            self.shadow.reset_bindings(materials.len());
        }
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
                if settings.enabled {
                    self.shadow.record(
                        graph,
                        program,
                        Bindings::Shadow { camera, cascade },
                        materials,
                        casters,
                        instanced.zip(transforms),
                    )?;
                    add_shadow_pass(graph, cascade, view, |pass| {
                        self.shadow.draw(pass, program, instanced)
                    })?;
                } else {
                    add_shadow_pass(graph, cascade, view, |_| Ok(()))?;
                }
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
    opaque: &'a Batch,
    transparent: &'a Batch,
    programs: &'a Programs,
    instanced: Option<&'a ManagedRasterProgram>,
}

impl RasterScene<'_> {
    pub(crate) fn draw_opaque(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        self.opaque
            .draw(pass, &self.programs.opaque, self.instanced)
    }

    pub(crate) fn draw_transparent(&self, pass: &mut RasterPass<'_, '_>) -> Result<(), GraphError> {
        self.transparent.draw(pass, &self.programs.blend, None)
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
    frame: &Frame<'_>,
    pass: DrawPass,
) -> Result<(), GraphError> {
    let wants_blend = matches!(pass, DrawPass::Transparent);
    for (index, draw) in draws.iter().enumerate() {
        let material = match *draw {
            Draw::Indexed(draw) => draw.material,
            Draw::Indirect { material, .. } if mode == DrawMode::Indirect => material,
            Draw::Instanced {
                geometry,
                first_transform,
                instance_count,
            } if mode == DrawMode::Transformed => {
                let range_end = first_transform
                    .checked_add(instance_count)
                    .and_then(|end| usize::try_from(end).ok());
                if instance_count == 0 || range_end.is_none_or(|end| end > frame.transforms.len()) {
                    return Err(GraphError::invalid_input(
                        "PBR instance range exceeds the transform table",
                    ));
                }
                geometry.material
            }
            Draw::Indirect { .. } | Draw::Instanced { .. } => {
                let kind = match draw {
                    Draw::Indirect { .. } => "indirect",
                    _ => "instanced",
                };
                return Err(GraphError::invalid_input(format!(
                    "PBR draw {index} uses {kind} geometry, which this recipe does not support in {}",
                    pass.name(),
                ))
                .with_pass_name(pass.name()));
            }
        };
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
    Ok(())
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
