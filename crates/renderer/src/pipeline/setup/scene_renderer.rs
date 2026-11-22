//! Recording and explicit configuration changes for an initialized scene renderer.
use super::*;

/// The replacement views are active. Retry release of their predecessors
/// with `SceneRenderer::collect_retired_views`.
#[derive(Debug)]
pub struct RetiredViewError(Box<dyn std::error::Error>);
impl std::fmt::Display for RetiredViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "replacement views active, previous view release pending: {}",
            self.0
        )
    }
}
impl std::error::Error for RetiredViewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Prepared views, reusable graph storage and the runtime used for their submissions.
pub struct SceneRenderer<R: BorrowMut<Runtime> = Runtime> {
    pub(crate) limits: SceneLimits,
    pub(crate) definitions: Vec<MaterialDefinition>,
    pub(super) driver: ViewRenderer,
    pub(super) pool: GraphPool,
    pub(super) runtime: Option<R>,
    pub(super) mesh_budget: u64,
    pub(super) configured: Preparation,
    pub(super) retired: Option<ViewRenderer>,
}
impl<R: BorrowMut<Runtime>> SceneRenderer<R> {
    pub(crate) fn mesh_upload_budget(&self) -> u64 {
        self.mesh_budget
    }
    /// Recreate all views on this runtime at a change boundary. The plan describes
    /// the complete replacement working set, including other engine passes using
    /// the device. Return outstanding result/export owners first. A rejected working
    /// set or failed initialization preserves the old views. If old-view release
    /// fails after publication, the error is a [`RetiredViewError`].
    pub fn replace_working_set(&mut self, plan: PreparedSetup<'_>) -> PipelineResult<()> {
        self.replace_plan(plan, |plan, runtime, overlap| {
            plan.check_runtime(runtime)?;
            Ok(runtime.prepare_working_set(overlap.working_set())?)
        })
    }
    pub(crate) fn replace_fitted_working_set(
        &mut self,
        plan: PreparedSetup<'_>,
    ) -> PipelineResult<()> {
        self.replace_plan(plan, |plan, runtime, overlap| {
            Ok(runtime.prepare_fitted_working_set(
                overlap,
                plan.upload_capacity()?,
                plan.readback,
            )?)
        })
    }

    fn replace_plan(
        &mut self,
        plan: PreparedSetup<'_>,
        prepare: impl FnOnce(&PreparedSetup<'_>, &mut Runtime, &Preparation) -> PipelineResult<()>,
    ) -> PipelineResult<()> {
        self.collect_retired_views()?;
        self.driver.check_idle()?;
        let runtime = self.runtime.as_mut().expect("live renderer").borrow_mut();
        // Keep every transient class and program the original cameras could
        // need if the new pipeline rejects an asset or fails to initialize.
        // Registered images and buffers retain their separate native leases.
        let mut old = Preparation::new(plan.preparation().frames_in_flight());
        old.add_working_set(self.configured.working_set())?;
        let mut overlap = Preparation::new(old.frames_in_flight());
        overlap.add_alternatives(&[&old, plan.preparation()])?;
        prepare(&plan, runtime, &overlap)?;
        let mut next = plan.begin_with_runtime(runtime)?.wait()?;
        self.retired = Some(std::mem::replace(
            &mut self.driver,
            std::mem::take(&mut next.driver),
        ));
        std::mem::swap(&mut self.pool, &mut next.pool);
        std::mem::swap(&mut self.configured, &mut next.configured);
        std::mem::swap(&mut self.definitions, &mut next.definitions);
        self.limits = next.limits;
        self.mesh_budget = next.mesh_budget;
        drop(next);
        self.collect_retired_views()
            .map_err(|e| Box::new(RetiredViewError(e)) as _)
    }

    pub fn collect_retired_views(&mut self) -> PipelineResult<()> {
        if let Some(retired) = &mut self.retired {
            retired.clear(self.runtime.as_mut().expect("live renderer").borrow_mut())?;
            self.retired = None;
        }
        Ok(())
    }

    pub(crate) fn prepare_native_set(
        &mut self,
        set: Preparation,
        transfer: u64,
    ) -> PipelineResult<()> {
        let limits = self.runtime().limits();
        let graph = set.replacement_limits(limits)?.graph;
        let pool = (graph != limits.graph)
            .then(|| GraphPool::new(graph, set.frames_in_flight().get()))
            .transpose()?;
        self.runtime()
            .prepare_fitted_working_set(&set, transfer, 0)?;
        if let Some(pool) = pool {
            self.pool = pool;
        }
        self.configured = set;
        Ok(())
    }
    /// Publish changed asset revisions within the prepared limits. Invoke after
    /// submitting or discarding graphs that refer to previous revisions. Accepted
    /// uploads survive errors so this maintenance operation can be retried.
    pub fn prepare_assets(
        &mut self,
        assets: SceneAssets<'_>,
    ) -> Result<(), crate::AssetPrepareError> {
        assets.meshes.collect_gpu(self.runtime())?;
        assets.materials.collect_gpu(self.runtime())?;
        self.prepare_assets_inner(assets).map_err(Into::into)
    }
    pub(crate) fn prepare_assets_inner(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        let mut meshes = Vec::new();
        let mut seen = HashSet::new();
        let mut materials = BTreeSet::new();
        for object in assets.objects.iter() {
            assets.meshes.geometry(object.mesh)?;
            assets.materials.get(object.material)?;
            if seen.insert(object.mesh) {
                meshes.push(object.mesh);
            }
            materials.insert(object.material);
        }
        for &mesh in assets.extra_meshes.iter() {
            assets.meshes.geometry(mesh)?;
            if seen.insert(mesh) {
                meshes.push(mesh);
            }
        }
        materials.extend(assets.extra_materials.iter().copied());
        let budget = self.mesh_budget;
        let (driver, pool, runtime) = self.parts_mut();
        assets.meshes.check_runtime(&meshes, runtime)?;
        let changed = MaterialCheckpoint::begin(assets.materials, &materials, runtime)?;
        let wait = |runtime: &mut Runtime, frame| -> PipelineResult<()> {
            while runtime
                .wait_for_frame(frame, Duration::from_secs(1))?
                .is_none()
            {}
            Ok(())
        };
        loop {
            let batch = assets.meshes.stream_gpu(runtime, pool, &meshes, budget)?;
            if let Some(id) = batch.submission {
                wait(runtime, id)?;
            }
            if batch.remaining_bytes == 0 {
                break;
            }
        }
        if let Some(id) = changed.upload(assets.materials, runtime)? {
            wait(runtime, id)?;
        }
        driver.prepare_all(assets.input())
    }
    pub fn begin_frame(&mut self) -> PipelineResult<SceneFrame<'_>> {
        Ok(SceneFrame {
            frame: self.driver.begin_frame(&mut self.pool)?,
            runtime: self.runtime.as_mut().expect("live renderer").borrow_mut(),
        })
    }
    pub fn camera_cut(&mut self, id: ViewId) -> PipelineResult<()> {
        self.driver.camera_cut(id)
    }

    /// Record on a host-allocated Graph while the host keeps access to the Runtime
    /// for scheduling and observing its own submissions.
    pub fn begin_host_frame(
        &mut self,
        graph: renderer_core::graph::Graph,
    ) -> PipelineResult<(ViewFrame<'_>, &mut Runtime)> {
        let frame = self.driver.begin_host_frame(graph)?;
        Ok((
            frame,
            self.runtime.as_mut().expect("live renderer").borrow_mut(),
        ))
    }
    pub fn finish_host_submission(&mut self, receipt: &FrameSubmission) -> PipelineResult<()> {
        self.driver.finish_host_submission(receipt)
    }
    pub fn runtime(&mut self) -> &mut Runtime {
        self.runtime.as_mut().expect("live renderer").borrow_mut()
    }
    pub fn finish_submission(&mut self) -> PipelineResult<FrameSubmission> {
        self.driver.finish_submission()
    }
    pub fn upload_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        Ok(self
            .runtime
            .as_mut()
            .expect("live renderer")
            .borrow_mut()
            .upload_texture(&mut self.pool, name, desc, bytes)?)
    }
    /// Access the underlying components together at an explicit engine change boundary.
    pub fn parts_mut(&mut self) -> (&mut ViewRenderer, &mut GraphPool, &mut Runtime) {
        (
            &mut self.driver,
            &mut self.pool,
            self.runtime.as_mut().expect("live renderer").borrow_mut(),
        )
    }
    pub fn clear(&mut self) -> PipelineResult<()> {
        self.collect_retired_views()?;
        self.driver
            .clear(self.runtime.as_mut().expect("live renderer").borrow_mut())
    }
    /// Release view-owned resources before transferring the runtime back to the host.
    pub fn into_runtime(mut self) -> PipelineResult<R> {
        self.clear()?;
        Ok(self.runtime.take().expect("live renderer"))
    }
}

impl<R: BorrowMut<Runtime>> Drop for SceneRenderer<R> {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.as_mut() {
            if let Some(retired) = &mut self.retired {
                let _ = retired.clear(runtime.borrow_mut());
            }
            let _ = self.driver.clear(runtime.borrow_mut());
        }
    }
}

/// Standard and custom views and engine passes share a graph and submission receipt.
pub struct SceneFrame<'a> {
    frame: ViewFrame<'a>,
    runtime: &'a mut Runtime,
}
impl<'a> Deref for SceneFrame<'a> {
    type Target = ViewFrame<'a>;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
impl DerefMut for SceneFrame<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frame
    }
}
impl<'a> SceneFrame<'a> {
    pub fn compile(self) -> Result<CompiledSceneFrame<'a>, GraphError> {
        Ok(CompiledSceneFrame {
            frame: self.frame.compile()?,
            runtime: self.runtime,
        })
    }
    pub fn submit(self) -> Result<FrameSubmission, ViewSubmitError> {
        self.frame.submit(self.runtime)
    }
}
pub struct CompiledSceneFrame<'a> {
    frame: CompiledViewFrame<'a>,
    runtime: &'a mut Runtime,
}
impl<'a> Deref for CompiledSceneFrame<'a> {
    type Target = CompiledViewFrame<'a>;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
impl CompiledSceneFrame<'_> {
    pub fn submit(self) -> Result<FrameSubmission, ViewSubmitError> {
        self.frame.submit(self.runtime)
    }
}
