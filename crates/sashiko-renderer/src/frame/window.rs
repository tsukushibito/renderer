//! Standard window rendering. The application owns its event loop.
use super::{Frame, FrameDestination, Renderer};
use crate::{
    passes::output::SdrOutput,
    pipeline::{CameraView, PipelineResult, SceneAssets},
};
use raw_window_handle::HasWindowHandle;
use sashiko_renderer_core::{
    PresentOutcome, PresentationMetrics, SubmissionMetrics,
    graph::ImageDesc,
    runtime::{FrameId, ManagedImage, Runtime, RuntimeConfig, WindowPresenter, WindowReadiness},
};
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};

pub struct WindowRenderer {
    presenter: WindowPresenter,
    renderer: Renderer,
    accepted: Option<FrameId>,
    profile: Option<Box<Option<SubmissionMetrics>>>,
    defer_for_asset_change: bool,
}

impl crate::pipeline::AssetTarget for WindowRenderer {
    type Device = Runtime;
    fn asset_renderer(&mut self) -> &mut crate::pipeline::SceneRenderer {
        &mut self.renderer.drawing
    }
    fn poll_presentation_for_asset_change(&mut self) -> PipelineResult<bool> {
        let ready = self.wait_idle(Duration::ZERO)?;
        self.defer_for_asset_change = !ready;
        Ok(ready)
    }
}

impl WindowRenderer {
    pub fn new(
        window: impl HasWindowHandle + 'static,
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<Self> {
        Self::new_with(window, backend, camera, assets, NonZeroUsize::MIN, |_| {
            Ok(())
        })
    }

    /// Prepare application passes and capacity for overlapping frame and asset submissions.
    pub fn new_with(
        window: impl HasWindowHandle + 'static,
        mut backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
        frames_in_flight: NonZeroUsize,
        configure: impl FnOnce(
            &mut sashiko_renderer_core::capacity::Preparation,
        ) -> Result<(), sashiko_renderer_core::graph::GraphError>,
    ) -> PipelineResult<Self> {
        let presentation = sashiko_renderer_core::PresentationConfig::default();
        let destination = FrameDestination::Window(presentation);
        let mut plan = Renderer::prepare(camera, None, assets, destination, frames_in_flight)?;
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
            profile: None,
            defer_for_asset_change: false,
        })
    }

    /// Read runtime limits and on-demand backend telemetry at observation boundaries.
    pub fn runtime_info(&mut self) -> &Runtime {
        self.renderer.drawing.runtime()
    }

    /// Check format/extent and filtering on the device used by this renderer.
    pub fn check_texture_support(
        &mut self,
        desc: ImageDesc,
        linear_filtering: bool,
    ) -> PipelineResult<()> {
        self.renderer.check_texture_support(desc, linear_filtering)
    }

    /// Add a texture reservation at an explicit capacity-change boundary.
    pub fn prepare_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        simultaneous: usize,
    ) -> PipelineResult<()> {
        self.renderer.prepare_texture(name, desc, simultaneous)
    }

    /// Upload one complete texture. Retain the returned owner until its last use is submitted.
    pub fn upload_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        self.renderer.upload_texture(name, desc, bytes)
    }

    /// Upload a contiguous mip range from level-major packed data.
    pub fn upload_texture_mips(
        &mut self,
        name: &str,
        desc: ImageDesc,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        self.renderer.upload_texture_mips(name, desc, levels, bytes)
    }

    /// Create a candidate from an existing version and the supplied new mip contents.
    pub fn promote_texture(
        &mut self,
        name: &str,
        previous: &ManagedImage,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        self.renderer.promote_texture(name, previous, levels, bytes)
    }

    /// Consume the upload completion without waiting on the GPU.
    pub fn poll_texture_upload(&mut self, id: FrameId) -> PipelineResult<bool> {
        self.renderer.poll_texture_upload(id)
    }

    /// Upload the static surface pages and the corresponding dynamic probe volume.
    pub fn load_surface_lighting(
        &mut self,
        surfaces: &crate::pbr::SurfaceLightmaps,
        reflections: Option<&crate::pbr::ReflectionProbes>,
    ) -> PipelineResult<crate::pipeline::ResidentBakedLighting> {
        self.ready_for_change()?;
        self.renderer.load_surface_lighting(surfaces, reflections)
    }

    /// Prepare baked diffuse and specular lighting on this renderer's device.
    pub fn load_probe_lighting(
        &mut self,
        assets: crate::pipeline::ProbeLighting<'_>,
    ) -> PipelineResult<crate::pipeline::ResidentBakedLighting> {
        self.ready_for_change()?;
        self.renderer.load_probe_lighting(assets)
    }

    /// Prepare the active geometry, materials and local lights at a scene change.
    pub fn prepare_scene(
        &mut self,
        assets: SceneAssets<'_>,
    ) -> Result<(), crate::AssetPrepareError> {
        self.ready_for_change()
            .map_err(crate::AssetPrepareError::from)?;
        self.renderer.prepare_scene(assets)
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
            use sashiko_renderer_core::capacity::{
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

    /// Configure timestamps at an idle change boundary. The graph and the
    /// fullscreen presentation commands have separate GPU intervals.
    pub fn set_profiling(&mut self, enabled: bool) -> PipelineResult<()> {
        if self.profile.is_some() == enabled {
            return Ok(());
        }
        self.ready_for_change()?;
        self.renderer.drawing.runtime().set_profiling(enabled)?;
        if let Err(error) = self.presenter.set_profiling(enabled) {
            self.renderer.drawing.runtime().set_profiling(!enabled)?;
            return Err(error.into());
        }
        self.profile = enabled.then(|| Box::new(None));
        Ok(())
    }

    /// Take the most recently retired graph sample. Drain after each draw
    /// attempt and after the final idle wait to retain every measured frame.
    pub fn take_graph_profile(&mut self) -> Option<SubmissionMetrics> {
        self.profile.as_deref_mut()?.take()
    }

    /// Drain completed presentation samples after draw attempts and idle waits.
    pub fn take_presentation_profile(&mut self) -> Option<PresentationMetrics> {
        self.presenter.take_profile()
    }

    /// Apply the presenter's supported VSync mode at a change boundary.
    pub fn set_vsync(&mut self, enabled: bool) -> PipelineResult<()> {
        self.ready_for_change()?;
        self.presenter.set_vsync(enabled)?;
        Ok(())
    }

    fn retire_graph(&mut self, timeout: Duration) -> PipelineResult<bool> {
        if let Some(id) = self.accepted {
            let Some(result) = self
                .renderer
                .drawing
                .runtime()
                .wait_for_frame(id, timeout)?
            else {
                return Ok(false);
            };
            if let Some(slot) = self.profile.as_deref_mut() {
                *slot = result.metrics().copied();
            }
            self.accepted = None;
        }
        Ok(true)
    }

    /// Retire the last render result and the presenter's source leases within
    /// one timeout. A queued but unpresented image still needs a draw retry.
    pub fn wait_idle(&mut self, timeout: Duration) -> PipelineResult<bool> {
        if self.presenter.has_pending_frame() {
            return Ok(false);
        }
        let start = Instant::now();
        if !self.retire_graph(timeout)? {
            return Ok(false);
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
        // Leave one opportunity to change the working set after the presented
        // frame retires. A canceled request cannot suppress a subsequent draw.
        if std::mem::take(&mut self.defer_for_asset_change) {
            return Ok(PresentOutcome::NotReady);
        }
        if !self.retire_graph(Duration::ZERO)? {
            return Ok(PresentOutcome::NotReady);
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
