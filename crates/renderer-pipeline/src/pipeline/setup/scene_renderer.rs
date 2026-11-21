//! Recording and explicit configuration changes for an initialized scene renderer.
use super::*;

/// Prepared views, reusable graph storage and the runtime used for their submissions.
pub struct SceneRenderer<R: BorrowMut<Runtime> = Runtime> {
    pub(super) driver: ViewRenderer,
    pub(super) pool: GraphPool,
    pub(super) runtime: Option<R>,
    pub(super) mesh_budget: u64,
}
impl<R: BorrowMut<Runtime>> SceneRenderer<R> {
    /// Recreate all views on this runtime at a change boundary. The plan describes
    /// the complete replacement working set, including other engine passes using
    /// the device. Return outstanding result/export owners first. A rejected working
    /// set preserves the old renderer. If initialization then fails, the runtime and
    /// engine assets remain accessible; retry with a corrected plan before drawing.
    pub fn replace_working_set(&mut self, plan: PreparedSetup<'_>) -> PipelineResult<()> {
        let (driver, pool, runtime) = self.parts_mut();
        driver.check_idle()?;
        plan.check_runtime(runtime)?;
        runtime.prepare_working_set(plan.preparation().working_set())?;
        driver.clear(runtime)?;
        let mut next = plan.begin_with_runtime(runtime)?.wait()?;
        std::mem::swap(driver, &mut next.driver);
        std::mem::swap(pool, &mut next.pool);
        let budget = next.mesh_budget;
        drop(next);
        self.mesh_budget = budget;
        Ok(())
    }
    /// Publish changed asset revisions within the prepared limits. Invoke after
    /// submitting or discarding graphs that refer to previous revisions. Accepted
    /// uploads survive errors so this maintenance operation can be retried.
    pub fn prepare_assets(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        let mut meshes = Vec::new();
        let mut seen = HashSet::new();
        let mut materials = BTreeSet::new();
        for object in assets.objects {
            assets.meshes.geometry(object.mesh)?;
            assets.materials.get(object.material)?;
            if seen.insert(object.mesh) {
                meshes.push(object.mesh);
            }
            materials.insert(object.material);
        }
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
    pub fn draw_scene(
        &mut self,
        camera: &CameraView,
        input: SceneInput<'_>,
    ) -> Result<ViewImages, GraphError> {
        let bound = self.bind_for_view(camera.id, input)?;
        self.frame.draw_view(bound, camera)
    }
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
