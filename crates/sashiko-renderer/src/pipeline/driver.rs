//! Record cameras of different techniques and resolutions into one application Graph.
mod group;
use self::group::{Group, PreparedGroup};
use super::{
    CameraView, FrameScene, PipelineId, PipelineResult, PreparedScene, SceneInput,
    StandardPipeline, StandardViewOutput, ViewId, ViewImages, ViewPipeline,
};
use crate::passes::output::{PreparedOutput, SdrImage, SdrOutput};
use sashiko_renderer_core::{
    capacity::Preparation,
    graph::{CompiledGraph, Graph, GraphError, GraphPool, ImageDesc, ImageHandle},
    runtime::{FrameId, FrameSubmission, Runtime},
};
use std::{
    any::Any,
    collections::HashMap,
    error::Error,
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

/// Rejected work can be rebuilt. Resume publication of already accepted work
/// with [`ViewRenderer::finish_submission`].
#[derive(Debug)]
pub enum ViewSubmitError {
    Rejected(Box<dyn Error>),
    Accepted {
        frame: FrameId,
        source: Box<dyn Error>,
    },
}
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
impl Error for ViewSubmitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Rejected(e) => e.as_ref(),
            Self::Accepted { source, .. } => source.as_ref(),
        })
    }
}

struct Pipeline {
    group: Box<dyn Group>,
    requirements: Preparation,
    output: Option<PreparedOutput>,
}

/// Registered cameras share one frame Graph, GPU submission and history transaction.
/// Each prepared pipeline supplies its own extent, rendering technique and output.
#[derive(Default)]
pub struct ViewRenderer {
    poses: std::cell::RefCell<crate::mesh::FramePoses>,
    pipelines: HashMap<PipelineId, Pipeline>,
    views: HashMap<ViewId, PipelineId>,
    pending: Vec<ViewId>,
    accepted: Option<FrameSubmission>,
    host_publication: Option<FrameId>,
    moving: Option<(ViewId, PipelineId)>,
}

impl ViewRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve a group of cameras with the same technique and dimensions.
    /// Include all simultaneously recorded groups in the same preparation.
    pub fn prepare_pipeline<P: ViewPipeline + 'static>(
        &mut self,
        pipeline: P,
        extent: [u32; 2],
        views: NonZeroUsize,
        output: Option<SdrOutput>,
        preparation: &mut Preparation,
    ) -> Result<PipelineId, GraphError> {
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
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = PipelineId(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
                .expect("view pipeline identity exhausted"),
        );
        self.pipelines.insert(
            id,
            Pipeline {
                group: Box::new(PreparedGroup::new(pipeline, extent, views.get())),
                requirements,
                output: stage,
            },
        );
        Ok(id)
    }
    /// Include a previously configured group when preparing a replacement Runtime working set.
    pub fn reserve_pipeline(
        &self,
        id: PipelineId,
        preparation: &mut Preparation,
    ) -> Result<(), GraphError> {
        preparation.add_alternatives(&[&self.pipeline(id)?.requirements])
    }
    fn pipeline(&self, id: PipelineId) -> Result<&Pipeline, GraphError> {
        self.pipelines
            .get(&id)
            .ok_or_else(|| GraphError::invalid_input("unknown view pipeline"))
    }
    fn pipeline_mut(&mut self, id: PipelineId) -> Result<&mut Pipeline, GraphError> {
        self.pipelines
            .get_mut(&id)
            .ok_or_else(|| GraphError::invalid_input("unknown view pipeline"))
    }
    pub fn pipeline_for_view(&self, view: ViewId) -> Result<PipelineId, GraphError> {
        self.views
            .get(&view)
            .copied()
            .ok_or_else(|| GraphError::invalid_input("unregistered camera"))
    }
    pub(super) fn check_idle(&self) -> PipelineResult<()> {
        if self.accepted.is_some() || self.host_publication.is_some() {
            Err("finish publishing accepted views first".into())
        } else if self.moving.is_some() {
            Err("finish reconfiguring the camera before using the view renderer".into())
        } else {
            Ok(())
        }
    }

    /// Initialize GPU assets for this group once its working set has been prepared.
    pub fn initialize_pipeline(
        &mut self,
        id: PipelineId,
        runtime: &mut Runtime,
    ) -> PipelineResult<Option<FrameId>> {
        self.check_idle()?;
        self.pipeline_mut(id)?.group.initialize(runtime)
    }
    /// Register new or changed mesh/material pairs without invalidating other pairs.
    pub fn prepare_scene(
        &mut self,
        id: PipelineId,
        changed: SceneInput<'_>,
    ) -> Result<(), GraphError> {
        self.pipeline_mut(id)?.group.prepare_scene(changed)?;
        self.poses.get_mut().reserve(changed.meshes.pose_count());
        Ok(())
    }
    pub(super) fn prepare_all(&mut self, changed: SceneInput<'_>) -> PipelineResult<()> {
        self.check_idle()?;
        for pipeline in self.pipelines.values_mut() {
            pipeline.group.stage_scene(changed)?;
        }
        for pipeline in self.pipelines.values_mut() {
            pipeline.group.publish_scene();
        }
        self.poses.get_mut().reserve(changed.meshes.pose_count());
        Ok(())
    }
    pub(super) fn remove_pairs(
        &mut self,
        pairs: &[(crate::mesh::MeshHandle, crate::material::MaterialHandle)],
    ) {
        for pipeline in self.pipelines.values_mut() {
            for &(mesh, material) in pairs {
                pipeline.group.remove_pair(mesh, material);
            }
        }
    }
    pub fn remove_scene_pair(
        &mut self,
        id: PipelineId,
        mesh: crate::mesh::MeshHandle,
        material: crate::material::MaterialHandle,
    ) -> Result<bool, GraphError> {
        Ok(self.pipeline_mut(id)?.group.remove_pair(mesh, material))
    }
    pub fn register_view(
        &mut self,
        pipeline: PipelineId,
        camera: &CameraView,
    ) -> PipelineResult<()> {
        self.check_idle()?;
        if self.views.contains_key(&camera.id) {
            return Err("camera already registered".into());
        }
        self.pipeline_mut(pipeline)?.group.register(camera)?;
        self.views.insert(camera.id, pipeline);
        self.pending.reserve(self.views.len());
        Ok(())
    }
    pub fn remove_view(&mut self, id: ViewId, runtime: &mut Runtime) -> PipelineResult<()> {
        self.check_idle()?;
        let group = self.pipeline_for_view(id)?;
        self.pipeline_mut(group)?.group.remove_view(id, runtime)?;
        self.views.remove(&id);
        Ok(())
    }
    /// Invalidate this view's temporal inputs before recording its next frame.
    pub fn camera_cut(&mut self, id: ViewId) -> PipelineResult<()> {
        self.check_idle()?;
        let group = self.pipeline_for_view(id)?;
        self.pipeline_mut(group)?.group.camera_cut(id)
    }
    /// Move a camera to a prepared extent/technique at a change boundary.
    /// Create the replacement before releasing the old history. A failed creation
    /// preserves the original registration. If release fails, retry this operation
    /// with the same camera and target to finish the move without recreating state.
    pub fn reconfigure_view(
        &mut self,
        id: PipelineId,
        camera: &CameraView,
        runtime: &mut Runtime,
    ) -> PipelineResult<()> {
        match self.moving {
            None => {
                self.check_idle()?;
                let old = self.pipeline_for_view(camera.id)?;
                if old == id {
                    return Err(
                        "prepare a different pipeline configuration before moving the view".into(),
                    );
                }
                self.pipeline_mut(id)?.group.register(camera)?;
                self.moving = Some((camera.id, id));
            }
            Some((view, target)) if (view, target) == (camera.id, id) => (),
            Some(_) => return Err("finish the previous camera reconfiguration first".into()),
        }
        self.finish_reconfiguration(runtime)
    }

    fn finish_reconfiguration(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        if let Some((view, target)) = self.moving {
            let old = self.pipeline_for_view(view)?;
            self.pipeline_mut(old)?.group.remove_view(view, runtime)?;
            *self.views.get_mut(&view).expect("registered view") = target;
            self.moving = None;
        }
        Ok(())
    }

    /// Release the pipeline after moving or removing its last view.
    pub fn remove_pipeline(&mut self, id: PipelineId, runtime: &mut Runtime) -> PipelineResult<()> {
        self.check_idle()?;
        if self.views.values().any(|value| *value == id) {
            return Err("remove or move registered cameras first".into());
        }
        self.pipeline_mut(id)?.group.release(runtime)?;
        self.pipelines.remove(&id);
        Ok(())
    }
    pub fn begin_frame<'a>(&'a mut self, pool: &mut GraphPool) -> PipelineResult<ViewFrame<'a>> {
        self.check_idle()?;
        let graph = pool.begin()?;
        Ok(self.start_recording(graph))
    }

    /// Continue recording in the host's Graph, including passes already declared there.
    pub fn begin_host_frame(&mut self, graph: Graph) -> PipelineResult<ViewFrame<'_>> {
        self.check_idle()?;
        Ok(self.start_recording(graph))
    }

    fn start_recording(&mut self, graph: Graph) -> ViewFrame<'_> {
        let id = graph.graph_id();
        self.poses.get_mut().begin(id);
        ViewFrame {
            renderer: self,
            graph: Some(graph),
            id,
            failed: false,
        }
    }
    /// Retry publication of remaining histories, using the existing GPU receipt.
    pub fn finish_submission(&mut self) -> PipelineResult<FrameSubmission> {
        let receipt = self.accepted.as_ref().ok_or("no accepted submission")?;
        Self::publish_pending(&mut self.pending, &self.views, &mut self.pipelines, receipt)?;
        Ok(self.accepted.take().expect("accepted receipt"))
    }

    /// Retry a partially published host submission, using the same receipt.
    pub fn finish_host_submission(&mut self, receipt: &FrameSubmission) -> PipelineResult<()> {
        if self.host_publication != Some(receipt.id()) {
            return Err("no pending view publication for this host frame".into());
        }
        Self::publish_pending(&mut self.pending, &self.views, &mut self.pipelines, receipt)?;
        self.host_publication = None;
        Ok(())
    }

    fn publish_pending(
        pending: &mut Vec<ViewId>,
        views: &HashMap<ViewId, PipelineId>,
        pipelines: &mut HashMap<PipelineId, Pipeline>,
        receipt: &FrameSubmission,
    ) -> PipelineResult<()> {
        while let Some(&view) = pending.last() {
            let group = views[&view];
            pipelines
                .get_mut(&group)
                .expect("registered pipeline")
                .group
                .accept(view, receipt)?;
            pending.pop();
        }
        Ok(())
    }

    fn check_submission(&self, graph: u64) -> PipelineResult<()> {
        for view in &self.pending {
            self.pipelines[&self.views[view]]
                .group
                .check_submission(*view, graph)?;
        }
        Ok(())
    }
    pub fn clear(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        self.finish_reconfiguration(runtime)?;
        self.check_idle()?;
        while let Some(id) = self.views.keys().next().copied() {
            self.remove_view(id, runtime)?;
        }
        while let Some(id) = self.pipelines.keys().next().copied() {
            self.remove_pipeline(id, runtime)?;
        }
        Ok(())
    }
}

/// A single Graph recording cameras from any registered pipeline.
pub struct ViewFrame<'a> {
    renderer: &'a mut ViewRenderer,
    graph: Option<Graph>,
    id: u64,
    failed: bool,
}
impl<'a> ViewFrame<'a> {
    pub fn draw_scene(
        &mut self,
        camera: &CameraView,
        input: SceneInput<'_>,
    ) -> Result<ViewImages, GraphError> {
        let bound = self.bind_for_view(camera.id, input)?;
        self.draw_view(bound, camera)
    }
    /// Bind the scene once for all cameras in this camera's pipeline group.
    pub fn bind_for_view<'s>(
        &self,
        view: ViewId,
        scene: SceneInput<'s>,
    ) -> Result<FrameScene<'s>, GraphError> {
        self.bind_scene(self.renderer.pipeline_for_view(view)?, scene)
    }
    pub fn graph(&mut self) -> &mut Graph {
        self.graph.as_mut().expect("building frame")
    }
    /// Record an auxiliary geometry pass using the same posed streams as camera draws.
    pub fn with_geometry<T>(
        &mut self,
        scene: SceneInput<'_>,
        record: impl FnOnce(&mut Graph, SceneInput<'_>) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        let graph = self.graph.as_mut().expect("building frame");
        if self.failed || graph.graph_id() != self.id {
            return Err(GraphError::invalid_input(
                "abandon failed or replaced view frame",
            ));
        }
        self.failed = true;
        // Reborrow the caller's scene for this recording's shorter lifetime.
        let input = SceneInput {
            poses: Some(&self.renderer.poses),
            ..scene
        };
        let output = record(graph, input)?;
        self.failed = false;
        Ok(output)
    }
    /// Validate revisions once for all cameras of this pipeline in this frame.
    pub fn bind_scene<'s>(
        &self,
        id: PipelineId,
        scene: SceneInput<'s>,
    ) -> Result<FrameScene<'s>, GraphError> {
        let group = self.renderer.pipeline(id)?.group.as_ref();
        if let Some(standard) =
            (group as &dyn Any).downcast_ref::<PreparedGroup<StandardPipeline>>()
        {
            standard.check_lights(scene.lights)?;
        }
        group.pairs().bind(scene, self.id, id)
    }
    fn record_with(
        &mut self,
        scene: FrameScene<'_>,
        camera: &CameraView,
        record: impl FnOnce(
            &mut dyn Group,
            &mut Graph,
            &CameraView,
            SceneInput<'_>,
        ) -> Result<ViewImages, GraphError>,
    ) -> Result<ViewImages, GraphError> {
        if self.failed {
            return Err(GraphError::invalid_input("abandon failed view frame"));
        }
        self.failed = true;
        let graph = self.graph.as_mut().expect("building frame");
        if graph.graph_id() != self.id {
            return Err(GraphError::invalid_input("abandon replaced view graph"));
        }
        let id = self.renderer.pipeline_for_view(camera.id)?;
        let mut input = scene.resolve(self.id, id)?;
        input.poses = Some(&self.renderer.poses);
        let images = record(
            self.renderer
                .pipelines
                .get_mut(&id)
                .expect("registered camera")
                .group
                .as_mut(),
            graph,
            camera,
            input,
        )?;
        self.renderer.pending.push(camera.id);
        self.failed = false;
        Ok(images)
    }
    pub fn draw_view(
        &mut self,
        scene: FrameScene<'_>,
        camera: &CameraView,
    ) -> Result<ViewImages, GraphError> {
        self.record_with(scene, camera, |group, graph, camera, input| {
            group.record(graph, camera, input)
        })
    }
    /// Insert a consumer before the standard Deferred lighting pass.
    pub fn draw_view_with_gbuffer(
        &mut self,
        scene: FrameScene<'_>,
        camera: &CameraView,
        consume: impl FnOnce(&mut Graph, crate::pbr::GBuffer) -> Result<(), GraphError>,
    ) -> Result<ViewImages, GraphError> {
        self.record_with(scene, camera, |group, graph, camera, input| {
            let group = (group as &mut dyn Any)
                .downcast_mut::<PreparedGroup<StandardPipeline>>()
                .ok_or_else(|| {
                    GraphError::invalid_input(
                        "G-buffer access requires the standard Deferred pipeline",
                    )
                })?;
            group.with_gbuffer(graph, camera, input, consume)
        })
    }
    pub fn standard_output(&self, view: ViewId) -> Option<&StandardViewOutput> {
        let id = self.renderer.pipeline_for_view(view).ok()?;
        let group = self.renderer.pipeline(id).ok()?.group.as_ref() as &dyn Any;
        group
            .downcast_ref::<PreparedGroup<StandardPipeline>>()?
            .output(view)
    }
    pub fn write_output(
        &mut self,
        view: ViewId,
        color: ImageHandle,
        exposure: f32,
        target: Option<ImageHandle>,
    ) -> Result<SdrImage, GraphError> {
        let id = self.renderer.pipeline_for_view(view)?;
        let output = self
            .renderer
            .pipeline(id)?
            .output
            .ok_or_else(|| GraphError::invalid_input("SDR output was not prepared"))?;
        if self.failed {
            return Err(GraphError::invalid_input("abandon failed view frame"));
        }
        self.failed = true;
        let graph = self.graph();
        let source = graph.image_view(color, 0, 0)?;
        let result = output.append_to(
            graph,
            source,
            PreparedOutput::validate_exposure(exposure)?,
            target,
        )?;
        self.failed = false;
        Ok(result.sdr)
    }
    /// Compile for inspection or submission. The plan retains its provisional histories.
    pub fn compile(mut self) -> Result<CompiledViewFrame<'a>, GraphError> {
        let graph = self.graph.take().expect("building frame");
        if self.failed || self.id != graph.graph_id() {
            return Err(GraphError::invalid_input(
                "abandon failed or replaced view graph",
            ));
        }
        Ok(CompiledViewFrame {
            plan: graph.compile()?,
            frame: self,
        })
    }
    /// Return the host's mutable Graph and a token for its provisional view histories.
    /// Append host UI, compile and submit as usual, then publish with that receipt.
    /// Dropping the token before submission abandons the provisional changes.
    pub fn finish_recording(mut self) -> PipelineResult<(Graph, RecordedViews<'a>)> {
        let graph = self.graph.take().expect("building frame");
        if self.failed || self.id != graph.graph_id() {
            return Err(GraphError::invalid_input("abandon failed or replaced view graph").into());
        }
        self.renderer.check_submission(self.id)?;
        Ok((
            graph,
            RecordedViews {
                frame: self,
                published: None,
            },
        ))
    }
    pub fn submit(self, runtime: &mut Runtime) -> Result<FrameSubmission, ViewSubmitError> {
        self.compile()
            .map_err(|e| ViewSubmitError::Rejected(e.into()))?
            .submit(runtime)
    }
}
impl Drop for ViewFrame<'_> {
    fn drop(&mut self) {
        if self.renderer.accepted.is_none() && self.renderer.host_publication.is_none() {
            for view in self.renderer.pending.drain(..) {
                let id = self.renderer.views[&view];
                self.renderer
                    .pipelines
                    .get_mut(&id)
                    .expect("registered pipeline")
                    .group
                    .cancel(view);
            }
        }
    }
}

/// Associates view-history updates with a specific Graph. The host retains its
/// submission receipt, exported images and readbacks.
#[must_use]
pub struct RecordedViews<'a> {
    frame: ViewFrame<'a>,
    published: Option<FrameId>,
}

impl RecordedViews<'_> {
    pub fn publish(&mut self, receipt: &FrameSubmission) -> PipelineResult<()> {
        if receipt.graph_id() != self.frame.id
            || self.published.is_some_and(|id| id != receipt.id())
        {
            return Err("receipt differs from the recorded view graph or submission".into());
        }
        let renderer = &mut self.frame.renderer;
        if self.published.is_none() {
            renderer.host_publication = Some(receipt.id());
            self.published = Some(receipt.id());
        }
        if renderer.host_publication.is_some() {
            renderer.finish_host_submission(receipt)?;
        }
        Ok(())
    }
}

/// CPU-inspectable plan, with one transaction for all recorded view histories.
/// Dropping the plan without submission discards the provisional updates.
pub struct CompiledViewFrame<'a> {
    plan: CompiledGraph,
    frame: ViewFrame<'a>,
}
impl CompiledViewFrame<'_> {
    pub fn pass_count(&self) -> usize {
        self.plan.pass_count()
    }
    pub fn ordered_pass_names(&self) -> impl Iterator<Item = &str> {
        self.plan.ordered_pass_names()
    }
    #[cfg(feature = "graph-diagnostics")]
    pub fn dump(&self) -> &str {
        self.plan.dump()
    }
    #[cfg(feature = "graph-diagnostics")]
    pub fn culled_pass_names(&self) -> impl Iterator<Item = &str> {
        self.plan.culled_pass_names()
    }
    pub fn submit(self, runtime: &mut Runtime) -> Result<FrameSubmission, ViewSubmitError> {
        let renderer = &mut *self.frame.renderer;
        renderer
            .check_submission(self.plan.graph_id())
            .map_err(ViewSubmitError::Rejected)?;
        let receipt = runtime
            .submit(&self.plan)
            .map_err(|e| ViewSubmitError::Rejected(e.into()))?;
        let frame = receipt.id();
        renderer.accepted = Some(receipt);
        renderer
            .finish_submission()
            .map_err(|source| ViewSubmitError::Accepted { frame, source })
    }
}
