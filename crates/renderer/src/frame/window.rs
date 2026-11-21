//! Standard window rendering. The application owns its event loop.
use super::{Frame, FrameDestination, Renderer};
use crate::{
    passes::output::SdrOutput,
    pipeline::{CameraView, PipelineResult, SceneAssets},
};
use raw_window_handle::HasWindowHandle;
use renderer_core::{
    PresentOutcome,
    runtime::{FrameId, Runtime, RuntimeConfig, WindowPresenter, WindowReadiness},
};
use std::time::{Duration, Instant};

pub struct WindowRenderer {
    presenter: WindowPresenter,
    renderer: Renderer,
    accepted: Option<FrameId>,
}

impl WindowRenderer {
    pub fn new(
        window: impl HasWindowHandle + 'static,
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<Self> {
        Self::new_with(window, backend, camera, assets, |_| Ok(()))
    }

    /// Prepare application passes for the same Graph as the scene and screen overlay.
    pub fn new_with(
        window: impl HasWindowHandle + 'static,
        mut backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
        configure: impl FnOnce(
            &mut renderer_core::capacity::Preparation,
        ) -> Result<(), renderer_core::graph::GraphError>,
    ) -> PipelineResult<Self> {
        let presentation = renderer_core::PresentationConfig::default();
        let destination = FrameDestination::Window(presentation);
        let mut plan = Renderer::prepare(camera, None, assets, destination)?;
        let host = Renderer::prepare_host(&mut plan, configure)?;
        plan.fit_backend(&mut backend)?;
        let (runtime, presenter) = Runtime::for_window(
            backend,
            plan.runtime_limits(Default::default())?,
            plan.preparation().working_set(),
            window,
            presentation,
        )?;
        let working_set = Renderer::base_working_set(&plan)?;
        let drawing = plan.begin_with_runtime(runtime)?.wait()?;
        Ok(Self {
            presenter,
            renderer: Renderer {
                drawing,
                camera,
                scene_extent: None,
                working_set,
                destination,
                host,
            },
            accepted: None,
        })
    }

    pub fn prepare_assets(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        self.ready_for_change()?;
        self.renderer.prepare_assets(assets)
    }

    /// Set the 3D resolution independently of the window and its HUD.
    pub fn set_scene_extent(
        &mut self,
        extent: Option<[u32; 2]>,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<()> {
        self.ready_for_change()?;
        self.renderer.set_scene_extent(extent, assets)
    }

    fn ready_for_change(&mut self) -> PipelineResult<()> {
        if self.presenter.has_pending_frame() {
            use renderer_core::capacity::{
                CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind,
            };
            return Err(RuntimeCapacityError::new(
                RuntimeCapacityKind::PresentationSlots,
                CapacityRecovery::Poll,
                Some(1),
                Some(2),
            )
            .into());
        }
        self.wait_for_change()
    }

    fn wait_for_change(&mut self) -> PipelineResult<()> {
        while !self.wait_idle(Duration::from_secs(1))? {}
        Ok(())
    }

    /// Retire the last render result and the presenter's source leases within
    /// one timeout. A queued but unpresented image still needs a draw retry.
    pub fn wait_idle(&mut self, timeout: Duration) -> PipelineResult<bool> {
        if self.presenter.has_pending_frame() {
            return Ok(false);
        }
        let start = Instant::now();
        if let Some(id) = self.accepted {
            if self
                .renderer
                .drawing
                .runtime()
                .wait_for_frame(id, timeout)?
                .is_none()
            {
                return Ok(false);
            }
            self.accepted = None;
        }
        Ok(self
            .presenter
            .wait_idle(timeout.saturating_sub(start.elapsed()))?)
    }

    pub fn set_camera(&mut self, camera: crate::pbr::ViewInput) {
        self.renderer.set_camera(camera);
    }
    pub fn cut_camera(&mut self, camera: crate::pbr::ViewInput) -> PipelineResult<()> {
        self.renderer.cut_camera(camera)
    }

    /// Apply a window size event at a change boundary. Zero size suspends presentation.
    pub fn resize(&mut self, extent: [u32; 2], assets: SceneAssets<'_>) -> PipelineResult<()> {
        self.presenter.discard_frame();
        if !extent.contains(&0) && self.renderer.camera.extent != extent {
            self.wait_for_change()?;
            let camera = CameraView {
                extent,
                ..self.renderer.camera
            };
            let changed = self.renderer.reconfigure(camera, assets);
            if self.renderer.camera.extent == extent {
                self.presenter.resize(extent[0], extent[1]);
            }
            return changed;
        }
        self.presenter.resize(extent[0], extent[1]);
        Ok(())
    }

    /// Close presentation and return the device for releasing host-owned assets.
    pub fn into_runtime(mut self) -> PipelineResult<Runtime> {
        while !self.presenter.close(Duration::from_secs(1))? {}
        if let Some(id) = self.accepted {
            while self
                .renderer
                .drawing
                .runtime()
                .wait_for_frame(id, Duration::from_secs(1))?
                .is_none()
            {}
        }
        self.renderer.into_runtime()
    }

    /// A pending present retries the accepted image. It does not resubmit the scene.
    pub fn draw(
        &mut self,
        draw: impl FnOnce(&mut Frame<'_>) -> PipelineResult<()>,
    ) -> PipelineResult<PresentOutcome> {
        self.presenter.poll()?;
        if self.presenter.has_pending_frame() {
            return Ok(self
                .presenter
                .present_frame(self.renderer.drawing.runtime())?);
        }
        if let Some(id) = self.accepted {
            if self
                .renderer
                .drawing
                .runtime()
                .wait_for_frame(id, Duration::ZERO)?
                .is_none()
            {
                return Ok(PresentOutcome::NotReady);
            }
            self.accepted = None;
        }
        match self.presenter.prepare()? {
            WindowReadiness::Pending => return Ok(PresentOutcome::NotReady),
            WindowReadiness::Suspended => return Ok(PresentOutcome::Suspended),
            WindowReadiness::Ready { extent, .. }
                if [extent.0, extent.1] != self.renderer.camera.extent =>
            {
                return Ok(PresentOutcome::RecreateRequired);
            }
            WindowReadiness::Ready { .. } => {}
        }
        let mut receipt = {
            let mut frame = self.renderer.begin_frame()?;
            draw(&mut frame)?;
            let image = frame.output(SdrOutput::display(false))?;
            frame.graph().export_frame_image(image.raw_image())?;
            frame.recording.submit()?
        };
        self.accepted = Some(receipt.id());
        self.presenter.queue_frame(&mut receipt)?;
        Ok(self
            .presenter
            .present_frame(self.renderer.drawing.runtime())?)
    }
}
