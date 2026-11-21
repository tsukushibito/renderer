//! Common preparation and frame operations for the standard PBR techniques.

use renderer_core::{
    capacity::Preparation,
    graph::{Graph, GraphError, ImageHandle, SampleCount},
};

use crate::{passes::output::OutputImages, pbr_deferred, pbr_forward, pbr_forward_plus};

use crate::pbr::{
    Bounds, Config, Environment, Frame, FrameImages, PreparedView, SceneImages, SceneLimits, Sun,
    ViewInput,
};

/// Select the standard PBR technique at preparation time.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Technique {
    #[default]
    Forward,
    ForwardPlus,
    Deferred,
}

impl Technique {
    pub const fn supports_samples(self, samples: SampleCount) -> bool {
        match self {
            Self::Forward | Self::ForwardPlus => true,
            Self::Deferred => matches!(samples, SampleCount::X1),
        }
    }

    /// Whether the standard scene output includes a sampleable D32 depth image.
    pub const fn provides_scene_depth(self, samples: SampleCount) -> bool {
        self.supports_samples(samples) && matches!(samples, SampleCount::X1)
    }

    pub const fn provides_gbuffer(self) -> bool {
        matches!(self, Self::Deferred)
    }
}

/// A standard PBR recipe with a common frame API. Construct it from a
/// [`Technique`], or supply a specialized Forward recipe via its variant.
pub enum GeometryPipeline {
    Forward(pbr_forward::Prepared),
    ForwardPlus(pbr_forward_plus::Prepared),
    Deferred(pbr_deferred::Prepared),
}

impl GeometryPipeline {
    pub fn new(
        technique: Technique,
        config: Config,
        limits: SceneLimits,
    ) -> Result<Self, GraphError> {
        match technique {
            Technique::Forward => pbr_forward::prepare(config, limits).map(Self::Forward),
            Technique::ForwardPlus => {
                pbr_forward_plus::prepare(config, limits).map(Self::ForwardPlus)
            }
            Technique::Deferred => pbr_deferred::prepare(config, limits).map(Self::Deferred),
        }
    }

    pub(crate) fn common(&self) -> &crate::pbr::scene::PreparedScene {
        match self {
            Self::Forward(recipe) => &recipe.common,
            Self::ForwardPlus(recipe) => &recipe.common,
            Self::Deferred(recipe) => &recipe.common,
        }
    }

    pub const fn technique(&self) -> Technique {
        match self {
            Self::Forward(_) => Technique::Forward,
            Self::ForwardPlus(_) => Technique::ForwardPlus,
            Self::Deferred(_) => Technique::Deferred,
        }
    }

    pub fn preparation(&self) -> &Preparation {
        self.common().preparation()
    }

    pub fn prepare_view(
        &self,
        view: ViewInput,
        sun: Sun,
        bounds: impl Into<Option<Bounds>>,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        self.common().prepare_view(view, sun, bounds, environment)
    }

    /// Declare the scene and return linear HDR for further processing.
    pub fn draw_hdr(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        match self {
            Self::Forward(recipe) | Self::ForwardPlus(recipe) => recipe.add_scene(graph, frame),
            Self::Deferred(recipe) => recipe.add_scene(graph, frame),
        }
    }

    /// Declare the standard output stage after scene rendering and any
    /// application HDR passes.
    pub fn write_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        self.common().add_output(graph, hdr, exposure)
    }

    /// Declare scene drawing and the standard output stage. Discard the
    /// frame Graph if a declaration fails.
    pub fn add_scene_and_sdr_output(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        exposure: f32,
    ) -> Result<FrameImages, GraphError> {
        match self {
            Self::Forward(recipe) | Self::ForwardPlus(recipe) => {
                recipe.add_scene_and_sdr_output(graph, frame, exposure)
            }
            Self::Deferred(recipe) => recipe.add_scene_and_sdr_output(graph, frame, exposure),
        }
    }
}
