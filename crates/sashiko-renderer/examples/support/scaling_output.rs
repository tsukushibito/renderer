//! Presentation is optional; both outputs record the same standard camera and SDR pass.
use super::{Options, PipelineResult, SceneRenderer, backend};
use sashiko_renderer::pipeline::PreparedSetup;
use sashiko_renderer_core::runtime::{FrameSubmission, Runtime};
use serde_json::Value;

#[cfg(all(windows, feature = "presentation"))]
#[path = "scaling_window.rs"]
mod window;

pub(super) enum Output {
    Headless,
    #[cfg(all(windows, feature = "presentation"))]
    Window(Box<window::Display>),
}

impl Output {
    pub(super) fn build(
        plan: PreparedSetup<'_>,
        opts: &Options,
    ) -> PipelineResult<(SceneRenderer, Self)> {
        let api = backend(&opts.api)?;
        if opts.present {
            #[cfg(all(windows, feature = "presentation"))]
            {
                let (renderer, window) = window::Display::build(plan, api, opts)?;
                return Ok((renderer, Self::Window(Box::new(window))));
            }
            #[cfg(not(all(windows, feature = "presentation")))]
            return Err("--present requires the Windows presentation feature".into());
        }
        Ok((plan.build(api)?, Self::Headless))
    }

    pub(super) fn begin_measurement(&mut self) {
        #[cfg(all(windows, feature = "presentation"))]
        if let Self::Window(window) = self {
            window.begin_measurement();
        }
    }
    pub(super) fn prepare_frame(&mut self) -> PipelineResult<()> {
        #[cfg(all(windows, feature = "presentation"))]
        if let Self::Window(window) = self {
            window.prepare_frame()?;
        }
        Ok(())
    }
    pub(super) fn present(
        &mut self,
        runtime: &mut Runtime,
        receipt: &mut FrameSubmission,
    ) -> PipelineResult<()> {
        #[cfg(all(windows, feature = "presentation"))]
        if let Self::Window(window) = self {
            window.present(runtime, receipt)?;
        }
        #[cfg(not(all(windows, feature = "presentation")))]
        let _ = (runtime, receipt);
        Ok(())
    }
    pub(super) fn drain(&mut self) -> PipelineResult<()> {
        #[cfg(all(windows, feature = "presentation"))]
        if let Self::Window(window) = self {
            window.drain()?;
        }
        Ok(())
    }
    pub(super) fn finish(self) -> PipelineResult<Value> {
        match self {
            Self::Headless => Ok(Value::Null),
            #[cfg(all(windows, feature = "presentation"))]
            Self::Window(mut window) => window.finish(),
        }
    }
}
