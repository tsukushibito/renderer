//! Standard scene and Canvas composition into a single output.
use crate::{
    canvas::{self, Layer},
    passes::output::{self, SdrOutput},
    pipeline::{
        CameraGroup, CameraView, PipelineResult, PreparedSetup, RendererSetup, SceneAssets,
        SceneFrame, SceneInput, SceneRenderer, StandardConfig, Technique,
    },
};
use renderer_core::{
    ImageFormat,
    graph::{Graph, GraphError, ImageDesc, ImageHandle, LoadOp, PassHandle, SampleCount},
    runtime::{FrameId, FrameSubmission, Runtime, RuntimeConfig},
};
use std::time::Duration;

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod window;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
pub use window::WindowRenderer;

const LINEAR: &str = "frame linear";
const OUTPUT: &str = "frame output";
const TRANSFER: &str = "frame output transfer";
const READBACK: &str = "frame pixels";
const TONE_MAP: &str = "frame tone map";

/// Standard drawing context for a camera. A frame borrows it until submission or cancellation.
pub struct Renderer {
    drawing: SceneRenderer,
    camera: CameraView,
}

impl Renderer {
    fn prepare<'a>(
        camera: CameraView,
        assets: SceneAssets<'a>,
        output: SdrOutput,
    ) -> PipelineResult<PreparedSetup<'a>> {
        let mut group = CameraGroup::standard(
            camera,
            Technique::Forward,
            StandardConfig::new(camera.extent),
        );
        group.output = None;
        let mut plan = RendererSetup::new(group).prepare(assets)?;
        let [w, h] = camera.extent;
        let target = ImageDesc::rgba8(w, h);
        let prep = plan.preparation_mut();
        prep.reserve_transient_image(LINEAR, ImageDesc::rgba16_float(w, h))?;
        prep.reserve_raster_pass(TONE_MAP, SampleCount::X1, |pass| {
            pass.attachment()?;
            pass.draws(&output::tone_map_neutral_pipeline().program, 1)
        })?;
        prep.reserve_raster_pass("canvas", SampleCount::X1, |pass| {
            pass.attachment()?;
            let [image, fill] = canvas::programs(ImageFormat::Rgba16Float)?;
            pass.draws(&image, 256)?;
            pass.draws(&fill, 256)
        })?;
        prep.reserve_raster_pass(TRANSFER, SampleCount::X1, |pass| {
            pass.transient_attachment(OUTPUT, target)?;
            pass.draws(&output.pipeline().program, 1)
        })?;
        if output == SdrOutput::capture(false) {
            plan.reserve_readback(READBACK, target)?;
        }
        Ok(plan)
    }

    pub fn headless(
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<Self> {
        let drawing = Self::prepare(camera, assets, SdrOutput::capture(false))?.build(backend)?;
        Ok(Self { drawing, camera })
    }

    pub fn begin_frame(&mut self) -> PipelineResult<Frame<'_>> {
        let recording = self.drawing.begin_frame()?;
        Ok(Frame {
            recording,
            camera: self.camera,
            linear: None,
        })
    }

    pub fn into_runtime(self) -> PipelineResult<Runtime> {
        self.drawing.into_runtime()
    }

    pub fn read_capture(
        &mut self,
        capture: &Capture,
        bytes: &mut [u8],
        timeout: Duration,
    ) -> PipelineResult<bool> {
        let [w, h] = capture.extent;
        if usize::try_from(u64::from(w) * u64::from(h))
            .ok()
            .and_then(|n| n.checked_mul(4))
            != Some(bytes.len())
        {
            return Err("capture buffer has the wrong extent".into());
        }
        let Some(mut result) = self
            .drawing
            .runtime()
            .wait_for_frame(capture.frame, timeout)?
        else {
            return Ok(false);
        };
        result.readback_into(capture.pixels, bytes)?;
        Ok(true)
    }
}

pub struct Capture {
    frame: FrameId,
    pixels: PassHandle,
    extent: [u32; 2],
}
impl Capture {
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
}

pub struct Frame<'a> {
    recording: SceneFrame<'a>,
    camera: CameraView,
    linear: Option<ImageHandle>,
}

impl Frame<'_> {
    pub fn graph(&mut self) -> &mut Graph {
        self.recording.graph()
    }

    fn new_target(&mut self) -> Result<ImageHandle, GraphError> {
        let [w, h] = self.camera.extent;
        self.graph().image(LINEAR, ImageDesc::rgba16_float(w, h))
    }

    /// Draw scene-linear geometry and apply the camera exposure before UI compositing.
    pub fn draw_scene(&mut self, input: SceneInput<'_>, exposure: f32) -> Result<(), GraphError> {
        if self.linear.is_some() {
            return Err(GraphError::invalid_input(
                "draw the main scene before its overlays",
            ));
        }
        let hdr = self.recording.draw_scene(&self.camera, input)?.color;
        let view = self.graph().image_view(hdr, 0, 0)?;
        let target = self.new_target()?;
        output::tone_map_neutral(self.graph(), TONE_MAP, view, target, exposure)?;
        self.linear = Some(target);
        Ok(())
    }

    /// Draw premultiplied layers at output resolution. A UI-only frame starts clear.
    pub fn draw_overlay(&mut self, layers: &[Layer]) -> Result<(), GraphError> {
        let (target, load) = match self.linear {
            Some(target) => (target, LoadOp::Load),
            None => (self.new_target()?, LoadOp::Clear([0., 0., 0., 1.])),
        };
        canvas::record(self.graph(), target, load, layers)?;
        self.linear = Some(target);
        Ok(())
    }

    fn output(&mut self, encoding: SdrOutput) -> Result<output::SdrImage, GraphError> {
        if self.linear.is_none() {
            self.draw_overlay(&[])?;
        }
        let linear = self.linear.expect("initialized frame");
        let source = self.graph().image_view(linear, 0, 0)?;
        let [w, h] = self.camera.extent;
        let target = self.graph().image(OUTPUT, ImageDesc::rgba8(w, h))?;
        output::write_sdr(self.graph(), TRANSFER, source, target, encoding)
    }

    /// Submit this frame and obtain its sRGB readback ticket without waiting.
    pub fn capture(mut self) -> PipelineResult<Capture> {
        let output = self.output(SdrOutput::capture(false))?;
        let pixels = self.graph().readback_image(READBACK, output.raw_image())?;
        let accepted: FrameSubmission = self.recording.submit()?;
        Ok(Capture {
            frame: accepted.id(),
            pixels,
            extent: self.camera.extent,
        })
    }
}
