//! Standard window rendering. The application owns its event loop.
use super::{Frame, Renderer};
use crate::{
    passes::output::SdrOutput,
    pipeline::{CameraView, PipelineResult, SceneAssets},
};
use raw_window_handle::HasWindowHandle;
use renderer_core::{
    PresentOutcome,
    runtime::{FrameId, Runtime, RuntimeConfig, WindowPresenter, WindowReadiness},
};
use std::time::Duration;

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
        let mut plan = Renderer::prepare(camera, assets, SdrOutput::display(false))?;
        configure(plan.preparation_mut())?;
        plan.fit_backend(&mut backend)?;
        let (runtime, presenter) = Runtime::for_window(
            backend,
            plan.runtime_limits(Default::default())?,
            plan.preparation().working_set(),
            window,
            Default::default(),
        )?;
        let working_set = Renderer::base_working_set(&plan)?;
        let drawing = plan.begin_with_runtime(runtime)?.wait()?;
        Ok(Self {
            presenter,
            renderer: Renderer {
                drawing,
                camera,
                working_set,
            },
            accepted: None,
        })
    }

    pub fn prepare_assets(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        self.renderer.prepare_assets(assets)
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
