//! Standard single-sample PBR Forward, with directional shadows and HDR output.
//!
//! At startup, prepare the recipe and include its requirements when preparing
//! the Renderer. At frame construction, the application provides graph-local
//! mesh, material and environment handles, with transparent draws already
//! ordered back to front. The same scene and output stages can be called
//! separately to insert additional HDR passes.

mod draw;
mod history;
mod raster;
mod view;

pub use draw::{Draw, Material};
#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
pub use history::ShadowHistoryError;
pub use history::{ShadowFrame, ShadowHistory, ShadowKey, ShadowUpdates};
pub use raster::{add_scene_passes, add_shadow_pass};
pub use view::{AlphaMode, Bounds, Camera, MaterialFactors, ShadowSettings, Sun};

use std::num::NonZeroUsize;

use renderer::capacity::{GraphRequirements, Preparation, WorkingSet};
use renderer::graph::{
    BufferDesc, BufferHandle, Graph, GraphCapacityKind, GraphError, ImageDesc, ImageHandle,
    ImageView, SampleCount,
};
use renderer::program::SamplerDescriptor;

use crate::passes::output::{OutputImages, PreparedOutput, SdrOutput};
use crate::passes::pbr::{CAMERA_BYTES, blend_program, opaque_program, shadow_program};
use draw::{Batch, Bindings, image_binding, sampler_binding};

pub const CASCADE_COUNT: usize = 4;

/// Size in bytes of the standard camera and lighting uniform. This is useful
/// when providing an existing buffer for [`Frame::camera_target`].
pub const VIEW_BYTES: usize = CAMERA_BYTES;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub extent: [u32; 2],
    pub clear_color: [f32; 4],
    pub shadows: ShadowSettings,
    pub output: SdrOutput,
}

/// Bounds used for preparation and checked again from the borrowed frame
/// slices before recording. The scene draw count combines opaque and blended
/// draws. The shadow draw limit applies to each cascade independently.
#[derive(Clone, Copy, Debug)]
pub struct SceneLimits {
    pub frames_in_flight: NonZeroUsize,
    pub max_draws: usize,
    pub max_materials: usize,
    pub max_casters_per_cascade: usize,
}

/// Graph-local linear HDR environment inputs. Irradiance and reflection are
/// 2D equirectangular images. The reflection mips are ordered by roughness;
/// BRDF RG stores the two preintegrated split-sum terms.
#[derive(Clone, Copy, Debug)]
pub struct Environment {
    pub irradiance: ImageView,
    pub reflection: ImageView,
    pub brdf_lut: ImageView,
    pub strength: f32,
    pub yaw_radians: f32,
    pub max_prefilter_lod: u32,
}

/// A view calculated once before selecting cascade draws or cached sources.
pub struct PreparedView {
    data: view::ViewData,
    uniform: [u8; CAMERA_BYTES],
    environment: Environment,
    extent: [u32; 2],
    shadows: ShadowSettings,
}

impl PreparedView {
    pub fn matrix(&self) -> &[[f32; 4]; 4] {
        self.data.matrix()
    }

    pub fn shadow_matrices(&self) -> &[[[f32; 4]; 4]; CASCADE_COUNT] {
        self.data.shadow_matrices()
    }

    /// Key each cascade by its exact projection, whether shadows are enabled,
    /// and the caller's revision for its shadow caster inputs.
    pub fn shadow_keys(
        &self,
        caster_revisions: [u64; CASCADE_COUNT],
    ) -> [ShadowKey; CASCADE_COUNT] {
        std::array::from_fn(|cascade| {
            ShadowKey::for_view(
                self.shadow_matrices()[cascade],
                caster_revisions[cascade],
                self.shadows.enabled,
            )
        })
    }

    pub fn uniform_bytes(&self) -> &[u8; CAMERA_BYTES] {
        &self.uniform
    }
}

/// Choose which image a cascade supplies to the scene. Handles belong to the
/// current Graph. Import a prior frame's valid snapshot before selecting reuse.
/// [`ShadowHistory`] can manage these images across submissions.
#[derive(Clone, Copy, Debug)]
pub enum ShadowSource<'a> {
    Render {
        /// Omit to use a prepared transient D32 image.
        target: Option<ImageHandle>,
        casters: &'a [Draw],
    },
    /// The caller establishes that the image still matches this view's
    /// cascade projection, caster geometry and material coverage.
    Reuse(ImageHandle),
}

pub struct Frame<'a> {
    pub view: &'a PreparedView,
    pub materials: &'a [Material],
    /// Opaque and alpha-masked draws.
    pub opaque: &'a [Draw],
    /// Alpha-blended draws, already ordered back to front.
    pub transparent: &'a [Draw],
    pub shadows: [ShadowSource<'a>; CASCADE_COUNT],
    /// Use an existing graph-local buffer's 464-byte prefix, or upload into
    /// a transient buffer. Export or refresh caller-owned history separately.
    pub camera_target: Option<BufferHandle>,
    /// Positive linear exposure passed to the Neutral tone mapper.
    pub exposure: f32,
}

impl<'a> Frame<'a> {
    /// Use a common caster list for all cascades, transient targets and
    /// unit exposure. The application can supply per-cascade culling or reuse
    /// by changing `shadows` before constructing the frame.
    pub fn new(
        view: &'a PreparedView,
        materials: &'a [Material],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        casters: &'a [Draw],
    ) -> Self {
        Self {
            view,
            materials,
            opaque,
            transparent,
            shadows: [ShadowSource::Render {
                target: None,
                casters,
            }; CASCADE_COUNT],
            camera_target: None,
            exposure: 1.0,
        }
    }
}

/// Graph-local images and camera buffer. Export only the handles needed by
/// later frames and publish the exported snapshots after submit succeeds.
#[derive(Clone, Copy, Debug)]
pub struct SceneImages {
    pub hdr: ImageHandle,
    pub depth: ImageHandle,
    pub shadows: [ImageHandle; CASCADE_COUNT],
    pub camera: BufferHandle,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameImages {
    pub hdr: ImageHandle,
    pub depth: ImageHandle,
    pub sdr: ImageHandle,
    pub shadows: [ImageHandle; CASCADE_COUNT],
    pub camera: BufferHandle,
}

/// Immutable recipe settings and scratch reused when declaring frames. The
/// Graph owns all recorded work, so it can be submitted after this is reused.
pub struct Prepared {
    config: Config,
    limits: SceneLimits,
    preparation: Preparation,
    output: PreparedOutput,
    opaque: Batch,
    transparent: Batch,
    shadow: Batch,
}

/// Reserve everything declared by this recipe. Include external assets,
/// additional passes, native staging and retained history in the application's
/// runtime and Graph limits. This function does not create device resources.
pub fn prepare(config: Config, scene_limits: SceneLimits) -> Result<Prepared, GraphError> {
    config.shadows.validate()?;
    let [width, height] = config.extent;
    if !config.clear_color.into_iter().all(f32::is_finite) {
        return Err(GraphError::invalid_input("PBR clear color must be finite"));
    }
    let mut preparation = Preparation::new(scene_limits.frames_in_flight);
    preparation.reserve_buffer(
        BufferDesc {
            size: CAMERA_BYTES as u64,
        },
        1,
    )?;
    preparation.reserve_image(ImageDesc::rgba16_float(width, height), 1)?;
    preparation.reserve_image(ImageDesc::depth32(width, height), 1)?;
    preparation.reserve_image(shadow_desc(config.shadows), CASCADE_COUNT)?;

    let scene_draws = scene_limits.max_draws;
    let casters = if config.shadows.enabled {
        scene_limits.max_casters_per_cascade
    } else {
        0
    };
    let shadow_draws = checked(casters.checked_mul(CASCADE_COUNT))?;
    let draws = checked(scene_draws.checked_add(shadow_draws))?;
    // A scene draw can read all 16 descriptor slots and both geometry streams.
    // Reserve for repeated accesses before the Graph compresses them.
    let scene_accesses = checked(scene_draws.checked_mul(18))?;
    let shadow_accesses = checked(shadow_draws.checked_mul(6))?;
    let accesses = checked(scene_accesses.checked_add(shadow_accesses))?;
    let accesses = checked(accesses.checked_add(16))?;
    let bindings_per_material = if config.shadows.enabled { 36 } else { 32 };
    let bindings = checked(
        scene_limits
            .max_materials
            .checked_mul(bindings_per_material),
    )?;
    let programs = [
        opaque_program().into(),
        blend_program().into(),
        shadow_program().into(),
    ];
    preparation.add_requirements(
        GraphRequirements {
            resources: 7,
            passes: 8,
            accesses,
            draws,
            raster_constant_words: shadow_draws,
            bindings,
            attachments: 10,
            name_bytes: 256,
            ..Default::default()
        },
        WorkingSet {
            raster: &programs[..if config.shadows.enabled { 3 } else { 2 }],
            ..Default::default()
        },
    )?;
    let output = PreparedOutput::prepare(&mut preparation, config.extent, config.output)?;

    Ok(Prepared {
        config,
        limits: scene_limits,
        preparation,
        output,
        opaque: Batch::prepare(scene_limits.max_materials, scene_draws)?,
        transparent: Batch::prepare(scene_limits.max_materials, scene_draws)?,
        shadow: Batch::prepare(
            if config.shadows.enabled {
                scene_limits.max_materials
            } else {
                0
            },
            casters,
        )?,
    })
}

impl Prepared {
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    /// Calculate the camera transform and four cascade projections without
    /// allocating or editing a Graph. Use these matrices for caster culling
    /// and to determine which history images can be reused.
    pub fn prepare_view(
        &self,
        camera: Camera,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        if environment.max_prefilter_lod >= environment.reflection.mip_level_count() {
            return Err(GraphError::invalid_input(
                "PBR reflection LOD exceeds the reflection view",
            ));
        }
        let data = view::prepare(
            camera,
            sun,
            bounds,
            self.config.extent,
            self.config.shadows,
            [
                environment.strength,
                environment.yaw_radians,
                environment.max_prefilter_lod as f32,
            ],
        )?;
        Ok(PreparedView {
            uniform: data.uniform_bytes(),
            data,
            environment,
            extent: self.config.extent,
            shadows: self.config.shadows,
        })
    }

    /// Build shadows, scene draw passes and SDR output on the same Graph.
    /// If any declaration fails, discard that Graph and reuse its pool slot.
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

    /// Build the single-sample scene and leave HDR available for further
    /// processing before calling [`Self::add_output`].
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        self.check_frame(&frame)?;
        validate_shadow_sources(graph, frame.shadows, shadow_desc(self.config.shadows))?;
        let camera = match frame.camera_target {
            Some(buffer) => buffer,
            None => graph.buffer(
                "pbr camera",
                BufferDesc {
                    size: CAMERA_BYTES as u64,
                },
            )?,
        };
        graph.upload_buffer("pbr camera upload", camera, 0, frame.view.uniform_bytes())?;
        let shadow_views = self.add_shadows(graph, camera, frame.materials, frame.shadows)?;
        let environment = frame.view.environment;
        let lighting = [
            image_binding(2, 0, environment.irradiance),
            image_binding(2, 1, environment.reflection),
            image_binding(2, 2, environment.brdf_lut),
            sampler_binding(2, 3, SamplerDescriptor::LinearRepeatUClampV),
            image_binding(3, 0, shadow_views[0]),
            image_binding(3, 1, shadow_views[1]),
            image_binding(3, 2, shadow_views[2]),
            image_binding(3, 3, shadow_views[3]),
            sampler_binding(3, 4, SamplerDescriptor::NearestClamp),
        ];
        let bindings = Bindings::Scene {
            camera,
            lighting: &lighting,
        };
        let opaque = opaque_program();
        let blend = blend_program();
        self.opaque.reset_bindings(frame.materials.len());
        self.transparent.reset_bindings(frame.materials.len());
        self.opaque
            .record(graph, &opaque, bindings, frame.materials, frame.opaque)?;
        self.transparent
            .record(graph, &blend, bindings, frame.materials, frame.transparent)?;
        let (hdr, depth) = add_scene_passes(
            graph,
            self.config.extent,
            self.config.clear_color,
            SampleCount::X1,
            !frame.transparent.is_empty(),
            |pass| pass.draw(&opaque, &self.opaque.draws),
            |pass| pass.draw(&blend, &self.transparent.draws),
        )?;
        Ok(SceneImages {
            hdr,
            depth,
            shadows: shadow_views.map(ImageView::image),
            camera,
        })
    }

    /// Tone map and encode a compatible HDR image. The source may be the
    /// recipe's HDR result or an image produced by a custom pass.
    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        let input = graph.image_view(hdr, 0, 0)?;
        self.output.add_passes(graph, input, exposure)
    }

    fn check_frame(&self, frame: &Frame<'_>) -> Result<(), GraphError> {
        if frame.view.extent != self.config.extent || frame.view.shadows != self.config.shadows {
            return Err(GraphError::invalid_input(
                "PBR view is incompatible with the prepared recipe",
            ));
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
        if self.config.shadows.enabled {
            for source in frame.shadows {
                if let ShadowSource::Render { casters, .. } = source {
                    check_limit(
                        GraphCapacityKind::Draws,
                        self.limits.max_casters_per_cascade,
                        Some(casters.len()),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn add_shadows(
        &mut self,
        graph: &mut Graph,
        camera: BufferHandle,
        materials: &[Material],
        sources: [ShadowSource<'_>; CASCADE_COUNT],
    ) -> Result<[ImageView; CASCADE_COUNT], GraphError> {
        let shadows = self.config.shadows;
        let desc = shadow_desc(shadows);
        let program = shadow_program();
        if shadows.enabled {
            self.shadow.reset_bindings(materials.len());
        }
        let mut views = [None; CASCADE_COUNT];
        for (cascade, source) in sources.into_iter().enumerate() {
            let selected = match source {
                ShadowSource::Reuse(image)
                | ShadowSource::Render {
                    target: Some(image),
                    ..
                } => image,
                ShadowSource::Render { target: None, .. } => {
                    graph.image(raster::SHADOW_IMAGES[cascade], desc)?
                }
            };
            let view = graph.image_view(selected, 0, 0)?;
            if let ShadowSource::Render { casters, .. } = source {
                if shadows.enabled {
                    let bindings = Bindings::Shadow { camera, cascade };
                    self.shadow
                        .record(graph, &program, bindings, materials, casters)?;
                    add_shadow_pass(graph, cascade, view, |pass| {
                        pass.draw(&program, &self.shadow.draws)
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

fn checked(value: Option<usize>) -> Result<usize, GraphError> {
    value.ok_or_else(|| GraphError::invalid_input("PBR frame capacity overflow"))
}
