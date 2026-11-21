//! Public contract for independently implemented camera pipelines.
use super::{Bounds, RenderObject, SceneInput, Sun, ViewInput};
use renderer::runtime::{FrameId, FrameSubmission, Runtime};
use renderer::{
    capacity::Preparation,
    graph::{Graph, GraphError, ImageHandle},
};
use std::{error::Error, num::NonZeroUsize};

pub type PipelineResult<T> = Result<T, Box<dyn Error>>;

/// Stable application identity. Register at camera creation and remove at destruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewId(pub u64);

#[derive(Clone, Copy)]
pub struct CameraView {
    pub id: ViewId,
    pub extent: [u32; 2],
    pub camera: ViewInput,
    pub bounds: Bounds,
    pub sun: Sun,
}

/// Graph-local scene-linear Rec.709, single-sample RGBA16F and optional D32.
#[derive(Clone, Copy)]
pub struct ViewImages {
    pub color: ImageHandle,
    pub depth: Option<ImageHandle>,
}

/// Pairings and revisions checked for this pipeline type by the frame driver.
pub struct PreparedScene<'a, P: ?Sized> {
    pub(super) input: SceneInput<'a>,
    pub(super) pipeline: std::marker::PhantomData<fn(&P)>,
}
impl<'a, P: ?Sized> PreparedScene<'a, P> {
    pub fn input(&self) -> SceneInput<'a> {
        self.input
    }
}

/// Implement in the application crate. State is allocated at view registration.
/// Updates own any provisional leases and cancel on drop before acceptance.
pub trait ViewPipeline {
    type State;
    type Update;

    fn reserve(
        &self,
        preparation: &mut Preparation,
        extent: [u32; 2],
        views: NonZeroUsize,
    ) -> Result<(), GraphError>;
    fn initialize(&mut self, _runtime: &mut Runtime) -> PipelineResult<Option<FrameId>> {
        Ok(None)
    }
    /// Validate the mesh/material pairing only. Object transforms and tint may vary per frame.
    fn check_object(&self, scene: SceneInput<'_>, object: &RenderObject) -> Result<(), GraphError>;
    fn create_view(&self, camera: &CameraView) -> PipelineResult<Self::State>;
    fn record(
        &mut self,
        graph: &mut Graph,
        state: &mut Self::State,
        camera: &CameraView,
        scene: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, Self::Update), GraphError>;
    /// Verify any graph-bound transaction prior to handing work to the Runtime.
    fn check_submission(
        &self,
        _state: &Self::State,
        _update: &Self::Update,
        _graph_id: u64,
    ) -> PipelineResult<()> {
        Ok(())
    }
    /// Publish provisional state after GPU acceptance. Stateless pipelines use the default.
    /// On error retain everything needed to retry with the same receipt.
    fn accept(
        &mut self,
        _state: &mut Self::State,
        _update: &mut Self::Update,
        _submission: &FrameSubmission,
    ) -> PipelineResult<()> {
        Ok(())
    }
    fn release_view(
        &mut self,
        _state: &mut Self::State,
        _runtime: &mut Runtime,
    ) -> PipelineResult<()> {
        Ok(())
    }
    fn release(&mut self, _runtime: &mut Runtime) -> PipelineResult<()> {
        Ok(())
    }
}
