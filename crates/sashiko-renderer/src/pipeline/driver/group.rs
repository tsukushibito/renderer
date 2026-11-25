//! A prepared technique owns its state, scene contracts and reusable update slots.
use super::*;
use crate::pipeline::scene_pairs::ScenePairs;
use sashiko_renderer_core::runtime::{FrameId, FrameSubmission, Runtime};
use std::any::Any;

pub(super) trait Group: Any {
    fn initialize(&mut self, runtime: &mut Runtime) -> PipelineResult<Option<FrameId>>;
    fn release(&mut self, runtime: &mut Runtime) -> PipelineResult<()>;
    fn prepare_scene(&mut self, scene: SceneInput<'_>) -> Result<(), GraphError>;
    fn stage_scene(&mut self, scene: SceneInput<'_>) -> Result<(), GraphError>;
    fn publish_scene(&mut self);
    fn remove_pair(
        &mut self,
        mesh: crate::mesh::MeshHandle,
        material: crate::material::MaterialHandle,
    ) -> bool;
    fn pairs(&self) -> &ScenePairs;
    fn register(&mut self, camera: &CameraView) -> PipelineResult<()>;
    fn camera_cut(&mut self, id: ViewId) -> PipelineResult<()>;
    fn check_room(&self, camera: &CameraView) -> PipelineResult<()>;
    fn remove_view(&mut self, id: ViewId, runtime: &mut Runtime) -> PipelineResult<()>;
    fn record(
        &mut self,
        graph: &mut Graph,
        camera: &CameraView,
        scene: SceneInput<'_>,
    ) -> Result<ViewImages, GraphError>;
    fn check_submission(&self, id: ViewId, graph: u64) -> PipelineResult<()>;
    fn accept(&mut self, id: ViewId, receipt: &FrameSubmission) -> PipelineResult<()>;
    fn cancel(&mut self, id: ViewId);
}

pub(super) struct PreparedGroup<P: ViewPipeline> {
    pipeline: P,
    extent: [u32; 2],
    limit: usize,
    initialized: bool,
    pairs: ScenePairs,
    views: HashMap<ViewId, ViewSlot<P>>,
}

struct ViewSlot<P: ViewPipeline> {
    state: P::State,
    update: Option<P::Update>,
}

impl PreparedGroup<StandardPipeline> {
    pub(super) fn check_lights(&self, lights: &[crate::pbr::LocalLight]) -> Result<(), GraphError> {
        self.pipeline.limits.check_lights(lights)
    }
}

impl<P: ViewPipeline> PreparedGroup<P> {
    pub(super) fn new(pipeline: P, extent: [u32; 2], limit: usize) -> Self {
        Self {
            pipeline,
            extent,
            limit,
            initialized: false,
            pairs: ScenePairs::default(),
            views: HashMap::with_capacity(limit),
        }
    }

    fn record_with(
        &mut self,
        graph: &mut Graph,
        camera: &CameraView,
        input: SceneInput<'_>,
        record: impl FnOnce(
            &mut P,
            &mut Graph,
            &mut P::State,
            &CameraView,
            PreparedScene<'_, P>,
        ) -> Result<(ViewImages, P::Update), GraphError>,
    ) -> Result<ViewImages, GraphError> {
        if camera.extent != self.extent {
            return Err(GraphError::invalid_input(
                "reconfigure view before drawing at a different extent",
            ));
        }
        let slot = self
            .views
            .get_mut(&camera.id)
            .expect("view is registered to this pipeline");
        if slot.update.is_some() {
            return Err(GraphError::invalid_input(
                "camera already recorded in this graph",
            ));
        }
        let scene = PreparedScene {
            input,
            pipeline: std::marker::PhantomData,
        };
        let (images, update) = record(&mut self.pipeline, graph, &mut slot.state, camera, scene)?;
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
        slot.update = Some(update);
        Ok(images)
    }
}

impl PreparedGroup<StandardPipeline> {
    pub(super) fn with_gbuffer(
        &mut self,
        graph: &mut Graph,
        camera: &CameraView,
        scene: SceneInput<'_>,
        consume: impl FnOnce(&mut Graph, crate::pbr::GBuffer) -> Result<(), GraphError>,
    ) -> Result<ViewImages, GraphError> {
        self.record_with(graph, camera, scene, |p, g, state, camera, scene| {
            p.record_camera(g, state, camera, scene.input(), Some(consume))
        })
    }
    pub(super) fn output(&self, id: ViewId) -> Option<&StandardViewOutput> {
        self.views.get(&id)?.update.as_ref()
    }
}

impl<P: ViewPipeline + 'static> Group for PreparedGroup<P> {
    fn initialize(&mut self, runtime: &mut Runtime) -> PipelineResult<Option<FrameId>> {
        if self.initialized {
            return Err("view pipeline already initialized".into());
        }
        let submitted = self.pipeline.initialize(runtime)?;
        self.initialized = true;
        Ok(submitted)
    }
    fn release(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        self.pipeline.release(runtime)?;
        self.initialized = false;
        Ok(())
    }
    fn prepare_scene(&mut self, scene: SceneInput<'_>) -> Result<(), GraphError> {
        self.pairs.register(&self.pipeline, scene)
    }
    fn stage_scene(&mut self, scene: SceneInput<'_>) -> Result<(), GraphError> {
        self.pairs.stage(&self.pipeline, scene)
    }
    fn publish_scene(&mut self) {
        self.pairs.publish();
    }
    fn remove_pair(
        &mut self,
        mesh: crate::mesh::MeshHandle,
        material: crate::material::MaterialHandle,
    ) -> bool {
        self.pairs.remove(mesh, material)
    }
    fn pairs(&self) -> &ScenePairs {
        &self.pairs
    }
    fn check_room(&self, camera: &CameraView) -> PipelineResult<()> {
        if camera.extent != self.extent {
            return Err("camera extent differs from prepared pipeline".into());
        }
        if self.views.len() >= self.limit {
            return Err("prepared camera capacity exhausted".into());
        }
        Ok(())
    }
    fn register(&mut self, camera: &CameraView) -> PipelineResult<()> {
        self.check_room(camera)?;
        self.views.insert(
            camera.id,
            ViewSlot {
                state: self.pipeline.create_view(camera)?,
                update: None,
            },
        );
        Ok(())
    }
    fn remove_view(&mut self, id: ViewId, runtime: &mut Runtime) -> PipelineResult<()> {
        let slot = self.views.get_mut(&id).expect("registered view");
        self.pipeline.release_view(&mut slot.state, runtime)?;
        self.views.remove(&id);
        Ok(())
    }
    fn camera_cut(&mut self, id: ViewId) -> PipelineResult<()> {
        let slot = self.views.get_mut(&id).expect("registered view");
        self.pipeline.camera_cut(&mut slot.state)
    }
    fn record(
        &mut self,
        graph: &mut Graph,
        camera: &CameraView,
        scene: SceneInput<'_>,
    ) -> Result<ViewImages, GraphError> {
        self.record_with(graph, camera, scene, P::record)
    }
    fn check_submission(&self, id: ViewId, graph: u64) -> PipelineResult<()> {
        if !self.initialized {
            return Err("initialize the view pipeline before submitting".into());
        }
        let slot = &self.views[&id];
        self.pipeline.check_submission(
            &slot.state,
            slot.update.as_ref().expect("pending update"),
            graph,
        )
    }
    fn accept(&mut self, id: ViewId, receipt: &FrameSubmission) -> PipelineResult<()> {
        let slot = self.views.get_mut(&id).expect("registered view");
        self.pipeline.accept(
            &mut slot.state,
            slot.update.as_mut().expect("pending update"),
            receipt,
        )?;
        slot.update = None;
        Ok(())
    }
    fn cancel(&mut self, id: ViewId) {
        self.views.get_mut(&id).expect("registered view").update = None;
    }
}
