//! Common preparation and frame operations for the standard PBR techniques.

use renderer::{
    capacity::Preparation,
    graph::{Graph, GraphError, ImageHandle, SampleCount},
};

use crate::{passes::output::OutputImages, pbr_deferred, pbr_forward, pbr_forward_plus};

use super::{
    Bounds, Config, Environment, Frame, FrameImages, PointLight, PreparedView, SceneImages,
    SceneLimits, Sun, ViewInput,
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

    pub const fn supports_point_lights(self) -> bool {
        matches!(self, Self::ForwardPlus)
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
pub enum Prepared {
    Forward(pbr_forward::Prepared),
    ForwardPlus(pbr_forward_plus::Prepared),
    Deferred(pbr_deferred::Prepared),
}

impl Prepared {
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

    pub const fn technique(&self) -> Technique {
        match self {
            Self::Forward(_) => Technique::Forward,
            Self::ForwardPlus(_) => Technique::ForwardPlus,
            Self::Deferred(_) => Technique::Deferred,
        }
    }

    pub fn preparation(&self) -> &Preparation {
        match self {
            Self::Forward(recipe) => recipe.preparation(),
            Self::ForwardPlus(recipe) => recipe.preparation(),
            Self::Deferred(recipe) => recipe.preparation(),
        }
    }

    pub fn prepare_view(
        &self,
        view: ViewInput,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        match self {
            Self::Forward(recipe) => recipe.prepare_view(view, sun, bounds, environment),
            Self::ForwardPlus(recipe) => recipe.prepare_view(view, sun, bounds, environment),
            Self::Deferred(recipe) => recipe.prepare_view(view, sun, bounds, environment),
        }
    }

    /// Declare the scene and return linear HDR for further processing. Point
    /// lights require Forward+ and are checked before declaring any work.
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        lights: &[PointLight],
    ) -> Result<SceneImages, GraphError> {
        self.check_lights(lights)?;
        match self {
            Self::Forward(recipe) => recipe.add_scene(graph, frame),
            Self::ForwardPlus(recipe) => recipe.add_scene(graph, frame, lights),
            Self::Deferred(recipe) => recipe.add_scene(graph, frame),
        }
    }

    /// Declare the standard output stage after scene rendering and any
    /// application HDR passes.
    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        match self {
            Self::Forward(recipe) => recipe.add_output(graph, hdr, exposure),
            Self::ForwardPlus(recipe) => recipe.add_output(graph, hdr, exposure),
            Self::Deferred(recipe) => recipe.add_output(graph, hdr, exposure),
        }
    }

    /// Declare scene drawing and the standard output stage. Discard the
    /// frame Graph if a declaration fails.
    pub fn add_passes(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        lights: &[PointLight],
    ) -> Result<FrameImages, GraphError> {
        self.check_lights(lights)?;
        match self {
            Self::Forward(recipe) => recipe.add_passes(graph, frame),
            Self::ForwardPlus(recipe) => recipe.add_passes(graph, frame, lights),
            Self::Deferred(recipe) => recipe.add_passes(graph, frame),
        }
    }

    fn check_lights(&self, lights: &[PointLight]) -> Result<(), GraphError> {
        if !lights.is_empty() && !self.technique().supports_point_lights() {
            return Err(GraphError::invalid_input(
                "PBR point lights require Forward+",
            ));
        }
        Ok(())
    }
}
