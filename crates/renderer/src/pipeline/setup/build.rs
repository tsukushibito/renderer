//! Bounded, resumable startup. Only the accepted frame belonging to this build is observed.
use super::*;
use std::task::Poll;

#[derive(Clone, Copy)]
enum Stage {
    Pipelines(usize),
    Meshes,
    Materials,
    Register(usize),
    Ready,
    Cancelled,
}

/// Poll once per loading tick. A pending build exclusively borrows its input registries.
/// A failed poll preserves progress so runtime backpressure can be resolved and retried.
#[must_use]
pub struct RendererBuild<'a, R: BorrowMut<Runtime>> {
    assets: SceneAssets<'a>,
    meshes: Vec<MeshHandle>,
    groups: Vec<PipelineId>,
    mesh_budget: u64,
    mesh_checkpoint: MeshCheckpoint,
    material_checkpoint: MaterialCheckpoint,
    renderer: Option<SceneRenderer<R>>,
    pending: Option<FrameId>,
    stage: Stage,
}

impl<'a, R: BorrowMut<Runtime>> RendererBuild<'a, R> {
    pub(super) fn new(plan: PreparedSetup<'a>, mut runtime: R) -> PipelineResult<Self> {
        plan.check_runtime(runtime.borrow_mut())?;
        let capacity = runtime.borrow_mut().limits().graph;
        let pool = GraphPool::new(capacity, plan.preparation.frames_in_flight().get())?;
        let material_checkpoint = MaterialCheckpoint::begin(
            plan.assets.materials,
            &plan.materials,
            runtime.borrow_mut(),
        )?;
        let mesh_checkpoint =
            MeshCheckpoint::begin(plan.assets.meshes, &plan.meshes, runtime.borrow_mut())?;
        Ok(Self {
            assets: plan.assets,
            meshes: plan.meshes,
            groups: plan.groups,
            mesh_budget: plan.mesh_budget,
            mesh_checkpoint,
            material_checkpoint,
            renderer: Some(SceneRenderer {
                limits: plan.limits,
                definitions: plan.definitions,
                transfers: super::candidate::Transfers::new(runtime.borrow_mut().limits().frames),
                configured: plan.preparation,
                retired: None,
                driver: plan.driver,
                pool,
                runtime: Some(runtime),
                mesh_budget: plan.mesh_budget,
            }),
            stage: Stage::Pipelines(0),
            pending: None,
        })
    }
    /// Access the runtime to retire unrelated work after recoverable backpressure.
    /// Leave the build's own submission for poll or wait.
    pub fn runtime(&mut self) -> &mut Runtime {
        self.renderer
            .as_mut()
            .expect("build owns renderer")
            .runtime()
    }
    pub fn pending_frame(&self) -> Option<FrameId> {
        self.pending
    }
    /// Advance one startup stage or mesh batch, without waiting for a GPU fence.
    pub fn poll(&mut self) -> PipelineResult<Poll<()>> {
        self.advance(Duration::ZERO)
    }

    fn advance(&mut self, timeout: Duration) -> PipelineResult<Poll<()>> {
        let renderer = self.renderer.as_mut().expect("build owns renderer");
        let (driver, pool, runtime) = renderer.parts_mut();
        if let Some(id) = self.pending {
            if runtime.wait_for_frame(id, timeout)?.is_none() {
                return Ok(Poll::Pending);
            }
            self.pending = None;
        }
        match self.stage {
            Stage::Pipelines(i) => {
                if let Some(&id) = self.groups.get(i) {
                    self.pending = driver.initialize_pipeline(id, runtime)?;
                    self.stage = Stage::Pipelines(i + 1);
                } else {
                    self.stage = Stage::Meshes;
                }
            }
            Stage::Meshes => {
                let progress =
                    self.assets
                        .meshes
                        .stream_gpu(runtime, pool, &self.meshes, self.mesh_budget)?;
                self.pending = progress.submission;
                if progress.remaining_bytes == 0 {
                    self.stage = Stage::Materials;
                }
            }
            Stage::Materials => {
                self.pending = self
                    .material_checkpoint
                    .upload(self.assets.materials, runtime)?;
                self.stage = Stage::Register(0);
            }
            Stage::Register(i) => {
                if let Some(&id) = self.groups.get(i) {
                    driver.prepare_scene(id, self.assets.input())?;
                    self.stage = Stage::Register(i + 1);
                } else {
                    self.stage = Stage::Ready;
                    return Ok(Poll::Ready(()));
                }
            }
            Stage::Ready => return Ok(Poll::Ready(())),
            Stage::Cancelled => return Err("renderer build was cancelled".into()),
        }
        Ok(Poll::Pending)
    }

    /// Wait for this build's accepted submissions and release the scene borrow.
    pub fn wait(mut self) -> PipelineResult<SceneRenderer<R>> {
        while self.advance(Duration::from_secs(1))?.is_pending() {}
        self.finish()
    }

    /// Publish the staged asset versions and release the scene borrow after poll returns Ready.
    pub fn finish(mut self) -> PipelineResult<SceneRenderer<R>> {
        if !matches!(self.stage, Stage::Ready) {
            return Err("renderer build is not ready".into());
        }
        let renderer = self.renderer.as_mut().expect("build owns renderer");
        self.mesh_checkpoint
            .finish(self.assets.meshes, true, renderer.runtime())?;
        Ok(self.renderer.take().expect("ready renderer"))
    }

    /// Restore the engine's original versions and release setup registrations. An
    /// accepted upload retains its GPU leases. The host must consume the returned
    /// frame's completion. Retry cleanup on failure. Dropping an unfinished build
    /// performs cleanup and waits for its outstanding completion.
    pub fn cancel(&mut self) -> PipelineResult<Option<FrameId>> {
        self.stage = Stage::Cancelled;
        let renderer = self.renderer.as_mut().expect("build owns renderer");
        self.material_checkpoint
            .rollback(self.assets.materials, renderer.runtime())?;
        self.mesh_checkpoint
            .finish(self.assets.meshes, false, renderer.runtime())?;
        renderer.clear()?;
        Ok(self.pending.take())
    }
}
impl<R: BorrowMut<Runtime>> Drop for RendererBuild<'_, R> {
    fn drop(&mut self) {
        if self.renderer.is_none() {
            return;
        }
        let pending = self.pending;
        let _ = self.cancel();
        if let Some(id) = pending {
            while let Ok(None) = self.runtime().wait_for_frame(id, Duration::from_secs(1)) {}
        }
    }
}
