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
    capacity::Preparation,
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

#[derive(Clone, Copy)]
enum FrameDestination {
    Capture,
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    Window(renderer_core::PresentationConfig),
}

impl FrameDestination {
    fn encoding(self) -> SdrOutput {
        match self {
            Self::Capture => SdrOutput::capture(false),
            #[cfg(all(
                windows,
                feature = "presentation",
                any(feature = "vulkan", feature = "d3d12")
            ))]
            Self::Window(_) => SdrOutput::display(false),
        }
    }
}

/// Standard drawing context for a camera. A frame borrows it until submission or cancellation.
pub struct Renderer {
    drawing: SceneRenderer,
    camera: CameraView,
    working_set: Preparation,
    host: Preparation,
    destination: FrameDestination,
}

impl Renderer {
    fn prepare<'a>(
        camera: CameraView,
        assets: SceneAssets<'a>,
        destination: FrameDestination,
    ) -> PipelineResult<PreparedSetup<'a>> {
        let mut group = CameraGroup::standard(
            camera,
            Technique::Forward,
            StandardConfig::new(camera.extent),
        );
        group.output = None;
        let mut setup = RendererSetup::new(group);
        // Small scenes commonly load the first models after showing their UI.
        // Reserve instance operands here; geometry sizes are known at model load.
        setup.additional_objects = 16usize.saturating_sub(assets.objects.len());
        let mut plan = setup.prepare(assets)?;
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
            pass.draws(&destination.encoding().pipeline().program, 1)
        })?;
        match destination {
            FrameDestination::Capture => plan.reserve_readback(READBACK, target)?,
            // Each outstanding present may still lease its source after the render
            // submission completes. The next Graph needs its own writable output.
            #[cfg(all(
                windows,
                feature = "presentation",
                any(feature = "vulkan", feature = "d3d12")
            ))]
            FrameDestination::Window(config) => prep.reserve_image(target, config.max_frames)?,
        }
        Ok(plan)
    }

    pub fn headless(
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<Self> {
        Self::headless_with(backend, camera, assets, |_| Ok(()))
    }

    /// Include host passes and resources that will be used on this frame's Graph.
    pub fn headless_with(
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
        configure: impl FnOnce(&mut Preparation) -> Result<(), GraphError>,
    ) -> PipelineResult<Self> {
        let destination = FrameDestination::Capture;
        let mut plan = Self::prepare(camera, assets, destination)?;
        let host = Self::prepare_host(&mut plan, configure)?;
        let working_set = Self::base_working_set(&plan)?;
        let drawing = plan.build(backend)?;
        Ok(Self {
            drawing,
            camera,
            working_set,
            host,
            destination,
        })
    }

    fn prepare_host(
        plan: &mut PreparedSetup<'_>,
        configure: impl FnOnce(&mut Preparation) -> Result<(), GraphError>,
    ) -> PipelineResult<Preparation> {
        let mut host = Preparation::new(plan.preparation().frames_in_flight());
        configure(&mut host)?;
        plan.preparation_mut()
            .add_requirements(host.graph_requirements(), host.working_set())?;
        Ok(host)
    }

    /// Update projection or viewpoint at the prepared resolution.
    pub fn set_camera(&mut self, view: crate::pbr::ViewInput) {
        self.camera.camera = view;
    }
    /// Set a discontinuous camera transform and discard prior temporal reuse.
    pub fn cut_camera(&mut self, view: crate::pbr::ViewInput) -> PipelineResult<()> {
        self.drawing.camera_cut(self.camera.id)?;
        self.set_camera(view);
        Ok(())
    }

    /// Reconfigure an existing camera at a resolution or scene change boundary.
    /// Return outstanding captures first. Registered geometry stays on the device.
    pub fn reconfigure(
        &mut self,
        camera: CameraView,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<()> {
        let mut plan = Self::prepare(camera, assets, self.destination)?;
        plan.preparation_mut()
            .add_requirements(self.host.graph_requirements(), self.host.working_set())?;
        let working_set = Self::base_working_set(&plan)?;
        let replaced = self.drawing.replace_fitted_working_set(plan);
        if replaced.is_ok()
            || replaced
                .as_ref()
                .is_err_and(|error| error.is::<crate::pipeline::RetiredViewError>())
        {
            self.camera = camera;
            self.working_set = working_set;
        }
        replaced
    }

    fn base_working_set(plan: &PreparedSetup<'_>) -> PipelineResult<Preparation> {
        let mut base = Preparation::new(plan.preparation().frames_in_flight());
        base.add_working_set(plan.preparation().working_set())?;
        Ok(base)
    }

    /// Prepare geometry and materials at a load boundary. Supply the active
    /// model selection. Subsequent pose changes use only the scene input.
    pub fn prepare_assets(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        use crate::material::MaterialInstance;
        use std::collections::HashSet;
        let mut meshes = HashSet::new();
        let mut materials = HashSet::new();
        let mut uniforms = 0;
        let mut transforms = 0;
        let mut casters = 0;
        let mut new_program = false;
        for object in assets.objects {
            meshes.insert(object.mesh);
            let first = materials.insert(object.material);
            casters += usize::from(object.casts_shadow);
            match assets.materials.get(object.material)? {
                MaterialInstance::Pbr(_) => {
                    uniforms += usize::from(first);
                    transforms += 1;
                }
                MaterialInstance::Custom(m) if first => {
                    new_program |= !self.drawing.definitions.contains(m.definition());
                }
                _ => {}
            }
        }
        // Asset loading can introduce new techniques or more independent objects
        // than the initial scene. Negotiate the change before publishing its assets.
        let limits = self.drawing.limits;
        if new_program
            || assets.objects.len() > limits.max_draws
            || materials.len() > limits.max_materials
            || transforms > limits.max_transforms
            || casters > limits.max_casters_per_cascade
            || assets.lights.len() > limits.max_point_lights
        {
            return self.reconfigure(self.camera, assets);
        }
        let mut active = Preparation::new(self.working_set.frames_in_flight());
        active.add_working_set(self.working_set.working_set())?;
        let upload = meshes
            .iter()
            .try_fold(0u64, |bytes, &mesh| -> Result<_, GraphError> {
                Ok(bytes.saturating_add(assets.meshes.geometry(mesh)?.byte_len()))
            })?
            .min(self.drawing.mesh_upload_budget());
        assets
            .meshes
            .reserve_selection(&mut active, meshes.into_iter())?;
        assets.meshes.reserve_poses(&mut active, assets.objects)?;
        active.reserve_buffer(crate::pbr::MaterialUniform::DESC, uniforms)?;
        assets.meshes.collect_gpu(self.drawing.runtime())?;
        assets.materials.collect_gpu(self.drawing.runtime())?;
        let material_bytes = uniforms
            .checked_mul(crate::passes::pbr::MATERIAL_BYTES)
            .ok_or("material upload overflow")?;
        let transfer = upload.max(material_bytes as u64);
        active.add_graph_requirements(renderer_core::capacity::GraphRequirements {
            upload_bytes: usize::try_from(transfer)?,
            passes: uniforms,
            resources: uniforms,
            accesses: uniforms,
            ..Default::default()
        })?;
        self.drawing.prepare_native_set(active, transfer)?;
        self.drawing.prepare_assets(assets)
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
