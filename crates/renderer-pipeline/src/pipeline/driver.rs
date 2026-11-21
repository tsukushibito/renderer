//! Record interchangeable camera pipelines into an application Graph pool.
use super::{
    CameraView, PipelineResult, PreparedScene, SceneInput, StandardPipeline, StandardViewOutput,
    ViewId, ViewImages, ViewPipeline,
};
use crate::passes::output::{PreparedOutput, SdrImage, SdrOutput};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::runtime::{FrameId, FrameSubmission, Runtime};
use renderer::{
    capacity::Preparation,
    graph::{Graph, GraphError, GraphPool, ImageDesc, ImageHandle},
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use std::error::Error;
use std::{collections::HashMap, num::NonZeroUsize};

/// A rejected frame can be rebuilt. An accepted frame must resume publication
/// with ViewRenderer::finish_submission before recording more work.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[derive(Debug)]
pub enum ViewSubmitError {
    Rejected(Box<dyn Error>),
    Accepted {
        frame: FrameId,
        source: Box<dyn Error>,
    },
}
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl std::fmt::Display for ViewSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(e) => write!(f, "view frame rejected: {e}"),
            Self::Accepted { frame, source } => write!(
                f,
                "view frame {frame:?} accepted; publication pending: {source}"
            ),
        }
    }
}
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl Error for ViewSubmitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Rejected(e) => e.as_ref(),
            Self::Accepted { source, .. } => source.as_ref(),
        })
    }
}

struct RegisteredView<S> {
    state: S,
    recorded_graph: Option<u64>,
}

/// Per-pipeline registrations and bounded submission bookkeeping.
pub struct ViewRenderer<P: ViewPipeline> {
    pipeline: P,
    extent: [u32; 2],
    limit: usize,
    output: Option<PreparedOutput>,
    views: HashMap<ViewId, RegisteredView<P::State>>,
    pending: Vec<(ViewId, P::Update)>,
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    accepted: Option<FrameSubmission>,
    initialized: bool,
    pairs: HashMap<(crate::mesh::MeshHandle, crate::material::MaterialHandle), (u64, u64)>,
}

impl<P: ViewPipeline> ViewRenderer<P> {
    fn has_accepted(&self) -> bool {
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        {
            self.accepted.is_some()
        }
        #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
        {
            false
        }
    }
    /// Add this renderer's complete working set before creating the Runtime.
    pub fn prepare(
        pipeline: P,
        extent: [u32; 2],
        views: NonZeroUsize,
        output: Option<SdrOutput>,
        preparation: &mut Preparation,
    ) -> Result<Self, GraphError> {
        if extent.contains(&0) {
            return Err(GraphError::invalid_input("empty camera extent"));
        }
        let mut requirements = Preparation::new(preparation.frames_in_flight());
        pipeline.reserve(&mut requirements, extent, views)?;
        let mut stage = None;
        if let Some(settings) = output {
            for _ in 0..views.get() {
                stage = Some(PreparedOutput::prepare(
                    &mut requirements,
                    extent,
                    settings,
                )?);
            }
        }
        preparation.add_alternatives(&[&requirements])?;
        Ok(Self {
            pipeline,
            extent,
            limit: views.get(),
            output: stage,
            views: HashMap::with_capacity(views.get()),
            pending: Vec::with_capacity(views.get()),
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            accepted: None,
            initialized: !cfg!(any(feature = "vulkan", all(windows, feature = "d3d12"))),
            pairs: HashMap::new(),
        })
    }
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn initialize(&mut self, runtime: &mut Runtime) -> PipelineResult<Option<FrameId>> {
        if self.initialized {
            return Err("view renderer already initialized".into());
        }
        let upload = self.pipeline.initialize(runtime)?;
        self.initialized = true;
        Ok(upload)
    }
    /// Register all mesh/material pairs usable in subsequent frames. Call after
    /// uploading changed assets; transforms, visibility and order can vary freely.
    pub fn prepare_scene(&mut self, scene: SceneInput<'_>) -> Result<(), GraphError> {
        let mut pairs = HashMap::with_capacity(scene.objects.len());
        for obj in scene.objects {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                pairs.entry((obj.mesh, obj.material))
            {
                self.pipeline.check_object(scene, obj)?;
                entry.insert((
                    scene.meshes.revision(obj.mesh)?,
                    scene.materials.revision(obj.material)?,
                ));
            }
        }
        self.pairs = pairs;
        Ok(())
    }
    pub fn register_view(&mut self, camera: &CameraView) -> PipelineResult<()> {
        if camera.extent != self.extent
            || self.views.contains_key(&camera.id)
            || self.views.len() == self.limit
        {
            return Err("camera extent, identity or reserved view capacity mismatch".into());
        }
        self.views.insert(
            camera.id,
            RegisteredView {
                state: self.pipeline.create_view(camera)?,
                recorded_graph: None,
            },
        );
        Ok(())
    }
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn remove_view(&mut self, id: ViewId, runtime: &mut Runtime) -> PipelineResult<()> {
        if self.has_accepted() {
            return Err("finish publishing accepted views first".into());
        }
        let state = self.views.get_mut(&id).ok_or("unknown view")?;
        self.pipeline.release_view(&mut state.state, runtime)?;
        self.views.remove(&id);
        Ok(())
    }
    pub fn begin_frame<'a>(&'a mut self, pool: &mut GraphPool) -> PipelineResult<ViewFrame<'a, P>> {
        if !self.initialized || self.has_accepted() {
            return Err(
                "initialize renderer and finish accepted updates before beginning a frame".into(),
            );
        }
        let graph = pool.begin()?;
        let id = graph.graph_id();
        Ok(ViewFrame {
            renderer: self,
            graph: Some(graph),
            id,
            failed: false,
        })
    }
    /// Continue publication after an accepted submission. Successfully published
    /// views have already been removed. Never resubmits GPU work.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn finish_submission(&mut self) -> PipelineResult<FrameSubmission> {
        let receipt = self.accepted.as_ref().ok_or("no accepted submission")?;
        while let Some((id, update)) = self.pending.last_mut() {
            self.pipeline.accept(
                &mut self
                    .views
                    .get_mut(id)
                    .expect("registered pending view")
                    .state,
                update,
                receipt,
            )?;
            self.pending.pop();
        }
        Ok(self.accepted.take().expect("accepted receipt"))
    }
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        if self.has_accepted() {
            return Err("finish publishing accepted views first".into());
        }
        while let Some(id) = self.views.keys().next().copied() {
            self.remove_view(id, runtime)?;
        }
        self.pipeline.release(runtime)?;
        self.initialized = false;
        Ok(())
    }
}

pub struct ViewFrame<'a, P: ViewPipeline> {
    renderer: &'a mut ViewRenderer<P>,
    graph: Option<Graph>,
    id: u64,
    failed: bool,
}
impl<P: ViewPipeline> ViewFrame<'_, P> {
    /// Inspect a CPU-only Graph. Provisional view updates are discarded.
    #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
    pub fn compile(mut self) -> Result<renderer::graph::CompiledGraph, GraphError> {
        let graph = self.graph.take().expect("building frame");
        if self.failed || self.id != graph.graph_id() {
            return Err(GraphError::invalid_input(
                "abandon failed or replaced view graph",
            ));
        }
        graph.compile()
    }
    pub fn graph(&mut self) -> &mut Graph {
        self.graph.as_mut().expect("building frame")
    }
    pub fn draw_view(
        &mut self,
        scene: SceneInput<'_>,
        camera: &CameraView,
    ) -> Result<ViewImages, GraphError> {
        self.record_with(scene, camera, P::record)
    }
    fn record_with(
        &mut self,
        scene: SceneInput<'_>,
        camera: &CameraView,
        record: impl FnOnce(
            &mut P,
            &mut Graph,
            &mut P::State,
            &CameraView,
            PreparedScene<'_, P>,
        ) -> Result<(ViewImages, P::Update), GraphError>,
    ) -> Result<ViewImages, GraphError> {
        if self.failed {
            return Err(GraphError::invalid_input("abandon failed view frame"));
        }
        self.failed = true;
        let r = &mut self.renderer;
        let graph = self.graph.as_mut().expect("building frame");
        if graph.graph_id() != self.id || camera.extent != r.extent {
            return Err(GraphError::invalid_input(
                "view already recorded or incompatible with frame",
            ));
        }
        let state = r
            .views
            .get_mut(&camera.id)
            .ok_or_else(|| GraphError::invalid_input("unregistered camera"))?;
        if state.recorded_graph == Some(self.id) {
            return Err(GraphError::invalid_input(
                "camera already recorded in this graph",
            ));
        }
        state.recorded_graph = Some(self.id);
        for object in scene.objects {
            let revisions = (
                scene.meshes.revision(object.mesh)?,
                scene.materials.revision(object.material)?,
            );
            if r.pairs.get(&(object.mesh, object.material)) != Some(&revisions) {
                return Err(GraphError::invalid_input(
                    "prepare scene after changing an asset or mesh/material pairing",
                ));
            }
        }
        let (images, update) = record(
            &mut r.pipeline,
            graph,
            &mut state.state,
            camera,
            PreparedScene {
                input: scene,
                pipeline: std::marker::PhantomData,
            },
        )?;
        let [w, h] = camera.extent;
        if graph.image_desc(images.color)? != ImageDesc::rgba16_float(w, h)
            || images
                .depth
                .map(|d| graph.image_desc(d))
                .transpose()?
                .is_some_and(|d| d != ImageDesc::depth32(w, h))
        {
            return Err(GraphError::invalid_input(
                "incompatible camera color or depth output",
            ));
        }
        r.pending.push((camera.id, update));
        self.failed = false;
        Ok(images)
    }
    pub fn write_output(
        &mut self,
        color: ImageHandle,
        exposure: f32,
        target: Option<ImageHandle>,
    ) -> Result<SdrImage, GraphError> {
        let stage = self
            .renderer
            .output
            .ok_or_else(|| GraphError::invalid_input("SDR output was not prepared"))?;
        if self.failed {
            return Err(GraphError::invalid_input("abandon failed view frame"));
        }
        self.failed = true;
        let graph = self.graph();
        let source = graph.image_view(color, 0, 0)?;
        let output = stage.append_to(
            graph,
            source,
            PreparedOutput::validate_exposure(exposure)?,
            target,
        )?;
        self.failed = false;
        Ok(output.sdr)
    }
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn submit(mut self, runtime: &mut Runtime) -> Result<FrameSubmission, ViewSubmitError> {
        let graph = self.graph.take().expect("building frame");
        if self.failed || self.id != graph.graph_id() {
            return Err(ViewSubmitError::Rejected(
                "abandon failed or replaced view graph".into(),
            ));
        }
        for (id, update) in &self.renderer.pending {
            self.renderer
                .pipeline
                .check_submission(&self.renderer.views[id].state, update, self.id)
                .map_err(ViewSubmitError::Rejected)?;
        }
        let plan = graph
            .compile()
            .map_err(|e| ViewSubmitError::Rejected(e.into()))?;
        let receipt = runtime
            .submit(&plan)
            .map_err(|e| ViewSubmitError::Rejected(e.into()))?;
        let frame = receipt.id();
        self.renderer.accepted = Some(receipt);
        self.renderer
            .finish_submission()
            .map_err(|source| ViewSubmitError::Accepted { frame, source })
    }
}
impl<P: ViewPipeline> Drop for ViewFrame<'_, P> {
    fn drop(&mut self) {
        if !self.renderer.has_accepted() {
            self.renderer.pending.clear();
        }
    }
}

impl ViewFrame<'_, StandardPipeline> {
    /// Append a consumer at the coherent Deferred geometry stage.
    pub fn draw_view_with_gbuffer(
        &mut self,
        scene: SceneInput<'_>,
        camera: &CameraView,
        consume: impl FnOnce(&mut Graph, crate::pbr::GBuffer) -> Result<(), GraphError>,
    ) -> Result<ViewImages, GraphError> {
        self.record_with(scene, camera, |p, g, state, camera, scene| {
            p.record_camera(g, state, camera, scene.input(), Some(consume))
        })
    }

    /// Standard technique outputs available to subsequent passes in this Graph.
    pub fn standard_output(&self, id: ViewId) -> Option<&StandardViewOutput> {
        self.renderer
            .pending
            .iter()
            .find(|(view, _)| *view == id)
            .map(|(_, update)| update)
    }
}
