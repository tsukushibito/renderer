//! Material, draw and view inputs shared by the standard PBR recipes.

mod culling;
mod defaults;
pub(crate) mod draw;
mod history;
pub(crate) mod instances;
pub(crate) mod raster;
mod vertex;
mod view;

pub use culling::{CascadeClip, Frustum, ViewCulling};
#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
pub use defaults::DefaultResourceError;
pub use defaults::{DefaultResources, DefaultViews};
pub use draw::{Draw, IndexedDraw, Material};
#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
pub use history::ShadowHistoryError;
pub use history::{ShadowFrame, ShadowHistory, ShadowKey, ShadowUpdates};
pub use instances::{MAX_TRANSFORMS, Transform};
pub use vertex::PbrVertex;
pub use view::{AlphaMode, Bounds, Camera, MaterialFactors, ShadowSettings, Sun};

pub(crate) use instances::PreparedInstances;

use std::num::NonZeroUsize;

use renderer::graph::{BufferHandle, GraphError, ImageHandle, ImageView};

use crate::passes::pbr::CAMERA_BYTES;

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

/// Upper bounds for frame construction. The scene draw count combines opaque
/// and blended submissions, and the shadow draw limit is per cascade.
#[derive(Clone, Copy, Debug)]
pub struct SceneLimits {
    pub frames_in_flight: NonZeroUsize,
    pub max_draws: usize,
    pub max_materials: usize,
    pub max_casters_per_cascade: usize,
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
    culling: ViewCulling,
    uniform: [u8; CAMERA_BYTES],
    environment: Environment,
    extent: [u32; 2],
    shadows: ShadowSettings,
}

impl PreparedView {
    pub(crate) fn prepare(
        camera: Camera,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
        extent: [u32; 2],
        shadows: ShadowSettings,
    ) -> Result<Self, GraphError> {
        if environment.max_prefilter_lod >= environment.reflection.mip_level_count() {
            return Err(GraphError::invalid_input(
                "PBR reflection LOD exceeds the reflection view",
            ));
        }
        let data = view::prepare(
            camera,
            sun,
            bounds,
            extent,
            shadows,
            [
                environment.strength,
                environment.yaw_radians,
                environment.max_prefilter_lod as f32,
            ],
        )?;
        Ok(Self {
            uniform: data.uniform_bytes(),
            culling: ViewCulling::new(*data.matrix(), data.shadow_matrices(), bounds),
            data,
            environment,
            extent,
            shadows,
        })
    }

    pub(crate) fn matches(&self, extent: [u32; 2], shadows: ShadowSettings) -> bool {
        self.extent == extent && self.shadows == shadows
    }

    pub(crate) fn environment(&self) -> Environment {
        self.environment
    }

    pub fn matrix(&self) -> &[[f32; 4]; 4] {
        self.data.matrix()
    }

    pub fn shadow_matrices(&self) -> &[[[f32; 4]; 4]; CASCADE_COUNT] {
        self.data.shadow_matrices()
    }

    /// Whether this world-space box may intersect the camera frustum.
    pub fn visible(&self, bounds: Bounds) -> bool {
        self.culling.visible(bounds)
    }

    /// Whether this world-space box may contribute to a cascade.
    /// Returns `None` for indices outside the four cascades.
    pub fn cascade_overlaps(&self, cascade: usize, bounds: Bounds) -> Option<bool> {
        self.culling.cascade_overlaps(cascade, bounds)
    }

    /// Key each cascade by its projection, whether shadows are enabled, and
    /// the caller's revision for its caster inputs, including transforms.
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

pub struct Frame<'a> {
    pub view: &'a PreparedView,
    pub materials: &'a [Material],
    /// Opaque and alpha-masked draws in submission order.
    pub opaque: &'a [Draw],
    /// Indexed alpha-blended draws, already ordered back to front.
    pub transparent: &'a [Draw],
    pub shadows: [ShadowSource<'a>; CASCADE_COUNT],
    /// Supply a graph-local target for [`CameraStorage::External`] or
    /// [`CameraStorage::PreUploaded`].
    pub camera_target: Option<BufferHandle>,
    /// Transforms referenced by the view's instance ranges.
    pub transforms: &'a [Transform],
    /// Positive linear exposure passed to the Neutral tone mapper.
    pub exposure: f32,
}

impl<'a> Frame<'a> {
    /// Use a common caster list for all cascades, transient targets and unit
    /// exposure. Change `shadows` to supply per-cascade culling or reuse.
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
            transforms: &[],
            exposure: 1.0,
        }
    }
}

/// Graph-local HDR and shadow images and the uploaded camera buffer.
#[derive(Clone, Copy, Debug)]
pub struct SceneImages {
    pub hdr: ImageHandle,
    /// Single-sample D32 at 1×. The 4× path resolves only color.
    pub depth: Option<ImageHandle>,
    pub shadows: [ImageHandle; CASCADE_COUNT],
    pub camera: BufferHandle,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameImages {
    pub hdr: ImageHandle,
    /// Single-sample D32 at 1×. The 4× path resolves only color.
    pub depth: Option<ImageHandle>,
    pub sdr: crate::passes::output::SdrImage,
    pub shadows: [ImageHandle; CASCADE_COUNT],
    pub camera: BufferHandle,
}
