//! Standard single-camera headless consumer prototype.
use renderer::{
    passes::output::SdrOutput,
    pipeline::{
        CameraGroup, CameraView, PipelineResult, RendererSetup, SceneAssets, SceneInput,
        SceneRenderer, StandardConfig, Technique,
    },
};
use renderer_core::{
    graph::{ImageDesc, PassHandle},
    runtime::{FrameId, Runtime, RuntimeConfig},
};
use std::time::Duration;

pub struct CaptureView {
    renderer: SceneRenderer,
    camera: CameraView,
}

pub struct PendingCapture {
    frame: FrameId,
    pixels: PassHandle,
}

impl CaptureView {
    pub fn new(
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<Self> {
        let mut group = CameraGroup::standard(
            camera,
            Technique::Forward,
            StandardConfig::new(camera.extent),
        );
        group.output = Some(SdrOutput::capture(false));
        let mut plan = RendererSetup::new(group).prepare(assets)?;
        plan.reserve_readback(
            "capture",
            ImageDesc::rgba8(camera.extent[0], camera.extent[1]),
        )?;
        Ok(Self {
            renderer: plan.build(backend)?,
            camera,
        })
    }

    /// Submit one frame. The scene can be changed as soon as recording returns.
    pub fn render(&mut self, input: SceneInput<'_>) -> PipelineResult<PendingCapture> {
        let mut frame = self.renderer.begin_frame()?;
        let color = frame.draw_scene(&self.camera, input)?.color;
        let output = frame.write_output(self.camera.id, color, 1., None)?;
        let pixels = output
            .capture()
            .expect("capture encoding configured")
            .readback(frame.graph(), "capture")?;
        Ok(PendingCapture {
            frame: frame.submit()?.id(),
            pixels,
        })
    }

    /// Wait only for this frame. On timeout the token remains usable.
    pub fn read(
        &mut self,
        pending: &PendingCapture,
        bytes: &mut [u8],
        timeout: Duration,
    ) -> PipelineResult<bool> {
        let [w, h] = self.camera.extent;
        if usize::try_from(u64::from(w) * u64::from(h))
            .ok()
            .and_then(|n| n.checked_mul(4))
            != Some(bytes.len())
        {
            return Err("capture buffer has the wrong extent".into());
        }
        let Some(mut frame) = self
            .renderer
            .runtime()
            .wait_for_frame(pending.frame, timeout)?
        else {
            return Ok(false);
        };
        frame.readback_into(pending.pixels, bytes)?;
        Ok(true)
    }

    pub fn into_runtime(self) -> PipelineResult<Runtime> {
        self.renderer.into_runtime()
    }
}
