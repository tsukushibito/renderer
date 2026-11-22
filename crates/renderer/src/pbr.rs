//! Material, draw and view inputs shared by the standard PBR recipes.

mod culling;
mod defaults;
pub(crate) mod draw;
mod history;
pub(crate) mod instances;
mod material;
pub(crate) mod raster;
pub(crate) mod scene;
mod unshadowed;
mod vertex;
mod view;

pub use crate::material::AlphaMode;
pub use crate::pipeline::Technique;
pub use culling::{CascadeClip, Frustum, ViewCulling};
pub use defaults::DefaultResourceError;
pub use defaults::{DefaultResources, DefaultViews, Defaults, PreparedDefaults};
pub use draw::{Draw, GraphMaterial, IndexedDraw, MaterialTextures};
pub use history::ShadowHistoryError;
pub use history::{ShadowFrame, ShadowHistory, ShadowKey, ShadowUpdates};
pub use instances::Transform;
pub use material::{MaterialFactors, MaterialUniform, ResidentMaterialUniform};
pub use unshadowed::UnshadowedView;
pub use vertex::PbrVertex;
pub use view::{Bounds, Projection, ShadowSettings, Sun, ViewInput};

pub(crate) use instances::PreparedInstances;

use std::num::NonZeroUsize;

use renderer_core::graph::{BufferHandle, GraphError, ImageHandle, ImageView, SampleCount};

use crate::passes::{output::SdrOutput, pbr::CAMERA_BYTES};

pub use crate::{pbr_deferred::GBuffer, pbr_forward_plus::PointLight};

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
/// and blended commands, and the shadow draw limit is per cascade. Budget
/// instanced ranges using [`Draw::command_count`].
#[derive(Clone, Copy, Debug)]
pub struct SceneLimits {
    pub frames_in_flight: NonZeroUsize,
    pub max_draws: usize,
    pub max_materials: usize,
    /// Maximum point lights per view. Zero reserves no light data or programs.
    pub max_point_lights: usize,
    pub max_casters_per_cascade: usize,
    /// Maximum model transforms across all draws in one view (u32 addressing).
    /// Zero reserves no table. Larger tables use bounded descriptor pages.
    pub max_transforms: usize,
}

/// Graph-local linear HDR environment inputs. Irradiance and reflection are
/// 2D equirectangular images. Reflection mips are ordered by roughness.
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

/// A view calculated before selecting scene draws, cascade draws or history.
pub struct PreparedView {
    data: view::ViewData,
    pub(crate) sort_direction: [f64; 3],
    culling: Option<ViewCulling>,
    uniform: [u8; CAMERA_BYTES],
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
                environment.strength,
                environment.yaw_radians,
                environment.max_prefilter_lod as f32,
            ],
        )?;
        Ok(Self {
            sort_direction: view.forward(),
            uniform: data.uniform_bytes(),
            culling: bounds
                .map(|bounds| ViewCulling::new(*data.matrix(), data.shadow_matrices(), bounds)),
            data,
            environment,
            extent,
            shadows,
        })
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
        self.culling.is_none_or(|c| c.visible(bounds))
    }

    /// Whether this world-space box may contribute to a cascade.
    /// Returns `None` for indices outside the prepared maps.
    pub fn cascade_overlaps(&self, cascade: usize, bounds: Bounds) -> Option<bool> {
        self.culling?.cascade_overlaps(cascade, bounds)
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
pub enum FrameShadows<'a> {
    Cascades([ShadowSource<'a>; CASCADE_COUNT]),
    Disabled(UnshadowedView),
}

pub struct PbrDrawInput<'a> {
    pub(crate) forward_draws: Option<&'a crate::pipeline::ForwardDraws>,
    pub view: &'a PreparedView,
    pub lights: &'a [PointLight],
    pub materials: &'a [GraphMaterial],
    /// Draws with Opaque or Mask materials in submission order.
    pub opaque: &'a [Draw],
    /// Indexed draws with Blend materials, already ordered back to front.
    pub transparent: &'a [Draw],
    pub shadows: FrameShadows<'a>,
    /// Supply a graph-local target for [`CameraStorage::External`] or
    /// [`CameraStorage::PreUploaded`].
    pub camera_target: Option<BufferHandle>,
    /// Transforms referenced by the view's instance ranges.
    pub transforms: &'a [Transform],
}

impl<'a> PbrDrawInput<'a> {
    /// Supply the same point lights to any of the standard PBR techniques.
    #[must_use]
    pub fn with_lights(mut self, lights: &'a [PointLight]) -> Self {
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
            Ok(Self::unshadowed(
                view,
                materials,
                opaque,
                transparent,
                defaults.unshadowed()?,
            ))
        }
    }

    /// Render all cascades to transient targets using the same caster list.
    /// Override `shadows` for per-cascade culling or history reuse.
    pub fn with_cascades(
        view: &'a PreparedView,
        materials: &'a [GraphMaterial],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        casters: &'a [Draw],
    ) -> Self {
        Self::with_shadows(
            view,
            materials,
            opaque,
            transparent,
            FrameShadows::Cascades(
                [ShadowSource::Render {
                    target: None,
                    casters,
                }; CASCADE_COUNT],
            ),
        )
    }

    /// Compose an advanced frame from per-cascade targets or reused shadow maps.
    pub fn with_shadows(
        view: &'a PreparedView,
        materials: &'a [GraphMaterial],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        shadows: FrameShadows<'a>,
    ) -> Self {
        Self {
            view,
            materials,
            opaque,
            transparent,
            shadows,
            camera_target: None,
            lights: &[],
            transforms: &[],
            forward_draws: None,
        }
    }

    /// Construct a frame for a recipe prepared without shadow settings.
    pub fn unshadowed(
        view: &'a PreparedView,
        materials: &'a [GraphMaterial],
        opaque: &'a [Draw],
        transparent: &'a [Draw],
        placeholder: UnshadowedView,
    ) -> Self {
        Self {
            shadows: FrameShadows::Disabled(placeholder),
            ..Self::with_cascades(view, materials, opaque, transparent, &[])
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
