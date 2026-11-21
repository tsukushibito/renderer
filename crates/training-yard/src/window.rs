//! Standalone presentation prototype. The application owns its event loop.
use raw_window_handle::HasWindowHandle;
use renderer::pipeline::{
    CameraView, PipelineResult, RendererSetup, SceneAssets, SceneInput, SceneRenderer,
};
use renderer_core::{
    PresentOutcome,
    runtime::{FrameId, Runtime, RuntimeConfig, WindowPresenter, WindowReadiness},
};
use std::time::Duration;

pub struct WindowView {
    presenter: WindowPresenter,
    renderer: SceneRenderer,
    camera: CameraView,
    accepted: Option<FrameId>,
}

impl WindowView {
    pub fn new(
        window: impl HasWindowHandle + 'static,
        mut backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<Self> {
        let plan = RendererSetup::standard(camera).prepare(assets)?;
        plan.fit_backend(&mut backend)?;
        let (runtime, presenter) = Runtime::for_window(
            backend,
            plan.runtime_limits(Default::default())?,
            plan.preparation().working_set(),
            window,
            Default::default(),
        )?;
        let renderer = plan.begin_with_runtime(runtime)?.wait()?;
        Ok(Self {
            presenter,
            renderer,
            camera,
            accepted: None,
        })
    }

    /// A pending present retries the accepted image. It does not resubmit the scene.
    pub fn draw(&mut self, input: SceneInput<'_>) -> PipelineResult<PresentOutcome> {
        self.presenter.poll()?;
        if self.presenter.has_pending_frame() {
            return Ok(self.presenter.present_frame(self.renderer.runtime())?);
        }
        if let Some(id) = self.accepted {
            if self
                .renderer
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
            WindowReadiness::Ready { extent, .. } if [extent.0, extent.1] != self.camera.extent => {
                return Ok(PresentOutcome::RecreateRequired);
            }
            WindowReadiness::Ready { .. } => {}
        }
        let mut frame = self.renderer.begin_frame()?;
        let color = frame.draw_scene(&self.camera, input)?.color;
        let output = frame.write_output(self.camera.id, color, 1., None)?;
        output
            .display()
            .expect("display output configured")
            .export_for_display(frame.graph())?;
        let mut receipt = frame.submit()?;
        self.accepted = Some(receipt.id());
        self.presenter.queue_frame(&mut receipt)?;
        Ok(self.presenter.present_frame(self.renderer.runtime())?)
    }
}
