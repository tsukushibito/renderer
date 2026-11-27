//! Material, draw and view inputs shared by the standard PBR recipes.

mod culling;
pub(crate) mod indirect_shadow;
#[cfg(feature = "memory-profiling")]
pub use indirect_shadow::ShadowTileStats;
pub use indirect_shadow::{IndirectShadowMode, ShadowCapsule, ShadowProxy};
mod lights;
pub use lights::{LocalLight, PointLight, RectLight, photometric_rgb};
mod defaults;
pub(crate) mod draw;
mod history;
mod probes;
mod surface_lightmaps;
pub use surface_lightmaps::{
    LightmapAddress, LightmapPage, SurfaceLightmapIndex, SurfaceLightmaps, SurfacePageRecord,
};
mod reflections;
pub use probes::{BakedLighting, BakedLightingView, DiffuseLighting, ProbeShOrder};
pub use reflections::{
    LocalReflections, ReflectionProbeDesc, ReflectionProbeView, ReflectionProbes,
};
pub(crate) mod instances;
mod material;
pub(crate) mod raster;
pub(crate) mod rect_shadows;
pub(crate) mod scene;
mod unshadowed;
mod vertex;
mod view;

pub use crate::material::AlphaMode;
pub use crate::pipeline::Technique;
pub use culling::{CascadeClip, Frustum, ViewCulling};
pub use defaults::DefaultResourceError;
pub use defaults::{DefaultResources, DefaultViews, Defaults, PreparedDefaults};
pub use draw::{
    Draw, DrawLightmap, GraphMaterial, IndexedDraw, MaterialSamplers, MaterialTextures,
};
pub use history::ShadowHistoryError;
pub(crate) use history::{MapFrame, MapHistory, MapUpdates};
pub use history::{ShadowFrame, ShadowHistory, ShadowKey, ShadowUpdates};
pub use instances::Transform;
pub use material::{MaterialFactors, MaterialUniform, NormalEncoding, ResidentMaterialUniform};
pub use rect_shadows::{RectShadowFace, RectShadowFrame, RectShadowHistory, RectShadowUpdates};
pub use unshadowed::UnshadowedView;
pub use vertex::PbrVertex;
pub use view::{Bounds, Projection, ShadowSettings, Sun, ViewInput};

pub(crate) use instances::PreparedInstances;

use std::num::NonZeroUsize;

use sashiko_renderer_core::graph::{BufferHandle, GraphError, ImageHandle, ImageView, SampleCount};

use crate::passes::{output::SdrOutput, pbr::CAMERA_BYTES};

pub use crate::pbr_deferred::GBuffer;

pub const CASCADE_COUNT: usize = 4;

/// Size in bytes of the standard view and lighting uniform. An external
/// camera buffer must provide this many writable bytes starting at zero.
pub const VIEW_BYTES: usize = CAMERA_BYTES;

/// Source of the buffer into which a recipe uploads the prepared view.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CameraStorage {
    #[default]
    Transient,
    /// Upload the view into a caller-supplied buffer.
    External,
    /// Consume the view from a caller-supplied buffer populated earlier in
    /// this Graph. Upload the same view before any visibility dispatch.
    PreUploaded,
}

/// Render targets, view storage and optional outputs for a standard PBR recipe.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub extent: [u32; 2],
    pub clear_color: [f32; 4],
    /// Directional cascade settings. `None` selects unshadowed shading.
    pub shadows: Option<ShadowSettings>,
    /// Standard SDR output. Omit it for HDR-only or application-defined output.
    pub output: Option<SdrOutput>,
    pub samples: SampleCount,
    pub camera_storage: CameraStorage,
}

impl Config {
    /// Standard single-sample view for a display target.
    pub fn display(extent: [u32; 2]) -> Self {
        Self {
            extent,
            clear_color: [0., 0., 0., 1.],
            shadows: None,
            output: Some(SdrOutput::display(false)),
            samples: SampleCount::X1,
            camera_storage: CameraStorage::Transient,
        }
    }
}

/// Upper bounds for frame construction. The scene draw count combines opaque
/// and blended commands, and the shadow limit bounds the common caster list and each culled list. Budget
/// instanced ranges using [`Draw::command_count`].
#[derive(Clone, Copy, Debug)]
pub struct SceneLimits {
    pub frames_in_flight: NonZeroUsize,
    pub max_draws: usize,
    pub max_materials: usize,
    /// Maximum simultaneous indirect shadow capsules, after culling.
    pub max_indirect_shadow_capsules: usize,
    /// Maximum static lightmapped commands in this view.
    pub max_lightmapped_draws: usize,
    /// Maximum local lights per view. Zero reserves no light data or programs.
    pub max_local_lights: usize,
    /// Maximum shadow-casting rectangles in one view.
    pub max_shadowed_rect_lights: usize,
    pub max_shadow_casters: usize,
    /// Maximum model transforms across all draws in one view (u32 addressing).
    /// Zero reserves no table. Larger tables use bounded descriptor pages.
    pub max_transforms: usize,
}

impl SceneLimits {
    /// The active local lights exceed the prepared count. At a change boundary, call
    /// [Renderer::prepare_scene](crate::Renderer::prepare_scene) before retrying.
    pub const LOCAL_LIGHTS: sashiko_renderer_core::graph::GraphCapacityKind =
        sashiko_renderer_core::graph::GraphCapacityKind::InputRecords("local lights");
    /// The shadowed rectangles exceed the prepared atlas slots.
    pub const SHADOWED_RECT_LIGHTS: sashiko_renderer_core::graph::GraphCapacityKind =
        sashiko_renderer_core::graph::GraphCapacityKind::InputRecords("shadowed rectangle lights");
    pub(crate) fn check_lights(&self, lights: &[LocalLight]) -> Result<(), GraphError> {
        for (kind, limit, required) in [
            (Self::LOCAL_LIGHTS, self.max_local_lights, lights.len()),
            (
                Self::SHADOWED_RECT_LIGHTS,
                self.max_shadowed_rect_lights,
                lights.iter().filter_map(LocalLight::shadow_emitter).count(),
            ),
        ] {
            if required > limit {
                return Err(GraphError::capacity_error(kind, limit, Some(required)));
            }
        }
        Ok(())
    }
}

/// Graph-local linear HDR environment inputs. Irradiance and reflection are
/// 2D images. Irradiance uses the selected diffuse layout. Reflection is
/// equirectangular with mips ordered by roughness.
/// BRDF RG stores the two preintegrated split-sum terms.
#[derive(Clone, Copy, Debug)]
pub struct Environment {
    pub irradiance: ImageView,
    pub diffuse_lighting: DiffuseLighting,
    pub reflection: ImageView,
    /// Immutable probe records, or an initialized empty table when local reflections are disabled.
    pub reflection_metadata: sashiko_renderer_core::graph::BufferHandle,
    pub local_reflections: Option<LocalReflections>,
    pub brdf_lut: ImageView,
    /// Luminance in cd/m² of a unit white environment texel, before convolution.
    pub nits_per_unit: f32,
    pub yaw_radians: f32,
    pub max_prefilter_lod: u32,
}

/// A view calculated before selecting scene draws, cascade draws or history.
pub struct PreparedView {
    pub(crate) virtual_input: Option<(ViewInput, Sun, Bounds, ShadowSettings)>,
    pub(crate) shadow_limit: usize,
    pub(crate) shadow_light_tail: Option<sashiko_renderer_core::graph::ImageDesc>,
    pub(crate) shadow_view: Option<ImageView>,
    pub(crate) shadow_mode: IndirectShadowMode,
    data: view::ViewData,
    pub(crate) sort_direction: [f64; 3],
    culling: Option<ViewCulling>,
    uniform: [u8; CAMERA_BYTES],
    pub(crate) clear_override: Option<[f32; 4]>,
    environment: Environment,
    extent: [u32; 2],
    shadows: Option<ShadowSettings>,
}

impl PreparedView {
    pub(crate) fn prepare(
        view: ViewInput,
        sun: Sun,
        bounds: Option<Bounds>,
        environment: Environment,
        extent: [u32; 2],
        shadows: Option<ShadowSettings>,
    ) -> Result<Self, GraphError> {
        if environment.max_prefilter_lod >= environment.reflection.mip_level_count() {
            return Err(GraphError::invalid_input(
                "PBR reflection LOD exceeds the reflection view",
            ));
        }
        let data = view::prepare(
            view,
            sun,
            bounds,
            shadows,
            [
                environment.nits_per_unit,
                environment.yaw_radians,
                environment.max_prefilter_lod as f32,
            ],
        )?;
        let mut uniform = data.uniform_bytes();
        if let Some(local) = environment.local_reflections {
            uniform[124..128].copy_from_slice(
                &local
                    .view
                    .camera_parameter(local.blend, shadows.is_some())?
                    .to_le_bytes(),
            );
        }
        uniform[460..464].copy_from_slice(&environment.diffuse_lighting.uniform()?.to_le_bytes());
        Ok(Self {
            sort_direction: view.forward(),
            virtual_input: None,
            uniform,
            clear_override: None,
            shadow_limit: 0,
            shadow_light_tail: None,
            shadow_view: None,
            shadow_mode: view.indirect_shadows,
            culling: bounds
                .map(|bounds| ViewCulling::new(*data.matrix(), data.shadow_matrices(), bounds)),
            data,
            environment,
            extent,
            shadows,
        })
    }

    /// Override this view's HDR clear at frame construction (for example a
    /// time-varying sky). The prepared target and pipeline are reusable.
    pub fn with_clear_color(mut self, color: [f32; 4]) -> Result<Self, GraphError> {
        if !color.into_iter().all(f32::is_finite) {
            return Err(GraphError::invalid_input("invalid PBR clear color"));
        }
        self.clear_override = Some(color);
        Ok(self)
    }
    pub(crate) fn matches(&self, extent: [u32; 2], shadows: Option<ShadowSettings>) -> bool {
        self.extent == extent && self.shadows == shadows
    }

    pub(crate) fn environment(&self) -> Environment {
        self.environment
    }

    pub fn matrix(&self) -> &[[f32; 4]; 4] {
        self.data.matrix()
    }

    /// The cascade projections, if the recipe uses directional shadows.
    pub fn shadow_matrices(&self) -> Option<&[[[f32; 4]; 4]; CASCADE_COUNT]> {
        self.data.shadow_matrices()
    }

    /// Whether this world-space box may intersect the camera frustum.
    pub fn visible(&self, bounds: Bounds) -> bool {
        self.culling.as_ref().is_none_or(|c| c.visible(bounds))
    }

    /// Whether this world-space box may contribute to a cascade.
    /// Returns `None` for indices outside the prepared maps.
    pub fn cascade_overlaps(&self, cascade: usize, bounds: Bounds) -> Option<bool> {
        self.culling.as_ref()?.cascade_overlaps(cascade, bounds)
    }

    /// Key each enabled cascade by its projection and the caller's revision
    /// for caster inputs, including transforms.
    pub fn shadow_keys(
        &self,
        caster_revisions: [u64; CASCADE_COUNT],
    ) -> Option<[ShadowKey; CASCADE_COUNT]> {
        let matrices = self.shadow_matrices()?;
        Some(std::array::from_fn(|cascade| {
            ShadowKey::new(matrices[cascade], caster_revisions[cascade])
        }))
    }

    pub fn uniform_bytes(&self) -> &[u8; CAMERA_BYTES] {
        &self.uniform
    }
}

/// Choose which image a cascade supplies to the scene. All handles belong to
/// the current Graph. [`ShadowHistory`] can manage images across submissions.
#[derive(Clone, Copy, Debug)]
pub enum ShadowSource<'a> {
    Render {
        /// Omit to use a prepared transient D32 image.
        target: Option<ImageHandle>,
        casters: &'a [Draw],
    },
    /// The caller establishes that the image still matches this view's
    /// projection, caster geometry and material coverage.
    Reuse(ImageHandle),
}

/// Graph-local shadow inputs matching the prepared recipe's shadow setting.
#[derive(Clone, Copy, Debug)]
pub enum SunShadows<'a> {
    Cascades([ShadowSource<'a>; CASCADE_COUNT]),
    Virtual(crate::passes::virtual_shadow::VirtualSun),
    Disabled(UnshadowedView),
}

pub struct PbrDrawInput<'a> {
    pub(crate) forward_draws: Option<&'a crate::pipeline::ForwardDraws>,
    pub view: &'a PreparedView,
    pub lights: &'a [LocalLight],
    /// Geometry eligible to cast local light shadows, including offscreen objects.
    pub(crate) casters: &'a [Draw],
    pub(crate) rect_target: Option<rect_shadows::Targets<'a>>,
    pub materials: &'a [GraphMaterial],
    /// Draws with Opaque or Mask materials in submission order.
    pub opaque: &'a [Draw],
    /// Indexed draws with Blend materials, already ordered back to front.
    pub transparent: &'a [Draw],
    pub sun_shadows: SunShadows<'a>,
    /// Supply a graph-local target for [`CameraStorage::External`] or
    /// [`CameraStorage::PreUploaded`].
    pub camera_target: Option<BufferHandle>,
    /// Transforms referenced by the view's instance ranges.
    pub transforms: &'a [Transform],
}

impl<'a> PbrDrawInput<'a> {
    /// Attach the atlas chosen by the current Graph's history transaction.
    pub fn use_rect_shadow_history(&mut self, frame: &RectShadowFrame<'a>) {
        self.rect_target = Some(frame.targets);
    }
    /// Supply the same local lights to any of the standard PBR techniques.
    #[must_use]
    pub fn with_lights(mut self, lights: &'a [LocalLight]) -> Self {
        self.lights = lights;
        self
    }

    /// Select shadow input from the prepared view. Enabled shadows use a
    /// common caster list. Disabled shadows use the shared default depth.
    pub fn new(
        view: &'a PreparedView,
        materials: &'a [GraphMaterial],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        casters: &'a [Draw],
        defaults: DefaultViews,
    ) -> Result<Self, GraphError> {
        if view.shadow_matrices().is_some() {
            Ok(Self::with_cascades(
                view,
                materials,
                opaque,
                transparent,
                casters,
            ))
        } else {
            Ok(Self::with_sun_shadows(
                view,
                materials,
                opaque,
                transparent,
                casters,
                SunShadows::Disabled(defaults.unshadowed()?),
            ))
        }
    }

    /// Render all cascades to transient targets using the same caster list.
    /// Override `sun_shadows` for per-cascade culling or history reuse.
    pub fn with_cascades(
        view: &'a PreparedView,
        materials: &'a [GraphMaterial],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        casters: &'a [Draw],
    ) -> Self {
        Self::with_sun_shadows(
            view,
            materials,
            opaque,
            transparent,
            casters,
            SunShadows::Cascades(
                [ShadowSource::Render {
                    target: None,
                    casters,
                }; CASCADE_COUNT],
            ),
        )
    }

    /// Compose an advanced frame from per-cascade targets or reused shadow maps.
    pub fn with_sun_shadows(
        view: &'a PreparedView,
        materials: &'a [GraphMaterial],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        casters: &'a [Draw],
        sun_shadows: SunShadows<'a>,
    ) -> Self {
        Self {
            view,
            materials,
            opaque,
            transparent,
            sun_shadows,
            camera_target: None,
            lights: &[],
            casters,
            rect_target: None,
            transforms: &[],
            forward_draws: None,
        }
    }
}

/// Graph-local HDR and shadow images and the uploaded camera buffer.
#[derive(Clone, Copy, Debug)]
pub struct SceneImages {
    pub hdr: ImageHandle,
    /// Single-sample D32 at 1×. The 4× path resolves only color.
    pub depth: Option<ImageHandle>,
    pub shadows: Option<[ImageHandle; CASCADE_COUNT]>,
    pub camera: BufferHandle,
    /// Coherent Deferred planes and geometry depth when no later Forward opaque pass writes depth.
    pub gbuffer: Option<GBuffer>,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameImages {
    pub hdr: ImageHandle,
    /// Single-sample D32 at 1×. The 4× path resolves only color.
    pub depth: Option<ImageHandle>,
    pub sdr: crate::passes::output::SdrImage,
    pub shadows: Option<[ImageHandle; CASCADE_COUNT]>,
    pub camera: BufferHandle,
    /// Coherent Deferred planes and geometry depth when no later Forward opaque pass writes depth.
    pub gbuffer: Option<GBuffer>,
}

impl SceneImages {
    pub(crate) fn with_sdr(self, sdr: crate::passes::output::SdrImage) -> FrameImages {
        FrameImages {
            hdr: self.hdr,
            depth: self.depth,
            shadows: self.shadows,
            camera: self.camera,
            gbuffer: self.gbuffer,
            sdr,
        }
    }
}
