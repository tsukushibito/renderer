//! Standard scene and Canvas composition into a single output.
use crate::{
    canvas::{self, Layer},
    passes::output::{self, SdrOutput},
    pipeline::{
        CameraGroup, CameraView, PipelineResult, PreparedSetup, RendererSetup, SceneAssets,
        SceneFrame, SceneInput, SceneRenderer, StandardConfig, Technique,
    },
};
use sashiko_renderer_core::{
    ImageFormat,
    capacity::Preparation,
    graph::{Graph, GraphError, ImageDesc, ImageHandle, LoadOp, PassHandle, SampleCount},
    runtime::{FrameId, FrameSubmission, Runtime, RuntimeConfig},
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

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
const SCENE_LINEAR: &str = "scene display linear";

#[derive(Clone, Copy)]
enum FrameDestination {
    Capture,
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    Window(sashiko_renderer_core::PresentationConfig),
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
    config: StandardConfig,
    drawing: SceneRenderer,
    captures: Box<[CaptureSlot]>,
    camera: CameraView,
    scene_extent: Option<[u32; 2]>,
    working_set: Preparation,
    host: Preparation,
    destination: FrameDestination,
}

impl crate::pipeline::AssetTarget for Renderer {
    type Device = Runtime;
    fn asset_renderer(&mut self) -> &mut SceneRenderer {
        &mut self.drawing
    }
}

impl Renderer {
    /// Prepare shared scene, Graph and command-recording jobs on the host team.
    pub fn prepare_jobs(
        &mut self,
        jobs: impl Into<sashiko_renderer_core::jobs::SharedJobs>,
    ) -> PipelineResult<()> {
        self.drawing.prepare_jobs(jobs)
    }
    fn prepare<'a>(
        camera: CameraView,
        mut config: StandardConfig,
        scene_extent: Option<[u32; 2]>,
        assets: SceneAssets<'a>,
        destination: FrameDestination,
        frames_in_flight: NonZeroUsize,
    ) -> PipelineResult<PreparedSetup<'a>> {
        let scene = CameraView {
            extent: scene_extent.unwrap_or(camera.extent),
            ..camera
        };
        config.extent = scene.extent;
        let mut group = CameraGroup::standard(scene, Technique::Forward, config);
        group.output = None;
        let mut setup = RendererSetup::new(group);
        setup.frames_in_flight = frames_in_flight;
        // Small scenes commonly load the first models after showing their UI.
        // Reserve instance operands here; geometry sizes are known at model load.
        setup.additional_objects = 16usize.saturating_sub(assets.draws);
        let mut plan = setup.prepare(assets)?;
        let [w, h] = camera.extent;
        let target = ImageDesc::rgba8(w, h);
        let prep = plan.preparation_mut();
        prep.reserve_sampler(canvas::IMAGE_SAMPLER);
        prep.reserve_sampler(output::TONE_MAP_SAMPLER);
        prep.reserve_sampler(destination.encoding().sampler());
        prep.reserve_transient_image(LINEAR, ImageDesc::rgba16_float(w, h))?;
        if scene.extent != camera.extent {
            let [sw, sh] = scene.extent;
            prep.reserve_transient_image(SCENE_LINEAR, ImageDesc::rgba16_float(sw, sh))?;
            prep.reserve_raster_pass("canvas", SampleCount::X1, |pass| {
                pass.attachment()?;
                pass.draws(&canvas::programs(ImageFormat::Rgba16Float)?[0], 1)
            })?;
        }
        prep.reserve_raster_pass(TONE_MAP, SampleCount::X1, |pass| {
            pass.attachment()?;
            pass.draws(&output::tone_map_neutral_pipeline().program, 1)?;
            if config.film_output {
                pass.draws(&output::film_program(), 1)?;
            }
            Ok(())
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
            FrameDestination::Capture => {
                let largest_capture = if config.film_output {
                    ImageDesc::rgba16_float(w, h)
                } else {
                    target
                };
                plan.reserve_readback(READBACK, largest_capture)?;
            }
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
        Self::headless_with(backend, camera, assets, NonZeroUsize::MIN, |_| Ok(()))
    }

    /// Prepare host passes and capacity for overlapping frame and asset submissions.
    pub fn headless_with(
        backend: RuntimeConfig,
        camera: CameraView,
        assets: SceneAssets<'_>,
        frames_in_flight: NonZeroUsize,
        configure: impl FnOnce(&mut Preparation) -> Result<(), GraphError>,
    ) -> PipelineResult<Self> {
        Self::headless_configured(
            backend,
            camera,
            StandardConfig::new(camera.extent),
            assets,
            frames_in_flight,
            configure,
        )
    }

    pub fn headless_configured(
        backend: RuntimeConfig,
        camera: CameraView,
        config: StandardConfig,
        assets: SceneAssets<'_>,
        frames_in_flight: NonZeroUsize,
        configure: impl FnOnce(&mut Preparation) -> Result<(), GraphError>,
    ) -> PipelineResult<Self> {
        let destination = FrameDestination::Capture;
        let mut plan = Self::prepare(camera, config, None, assets, destination, frames_in_flight)?;
        let host = Self::prepare_host(&mut plan, configure)?;
        let working_set = Self::base_working_set(&plan)?;
        let drawing = plan.build(backend)?;
        Ok(Self {
            config,
            drawing,
            captures: (0..frames_in_flight.get())
                .map(|_| CaptureSlot::default())
                .collect(),
            camera,
            scene_extent: None,
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
        self.configure_view(camera, self.scene_extent, assets)
    }

    /// Set the 3D render resolution at a change boundary. Overlays and captures
    /// use the full output size. Pass `None` to follow output resizes again.
    pub fn set_scene_extent(
        &mut self,
        extent: Option<[u32; 2]>,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<()> {
        self.configure_view(self.camera, extent, assets)
    }

    fn configure_view(
        &mut self,
        camera: CameraView,
        scene_extent: Option<[u32; 2]>,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<()> {
        if scene_extent.is_some_and(|extent| extent.contains(&0)) {
            return Err("3D render extent must be nonzero".into());
        }
        let mut plan = Self::prepare(
            camera,
            self.config,
            scene_extent,
            assets,
            self.destination,
            self.host.frames_in_flight(),
        )?;
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
            self.scene_extent = scene_extent;
            self.working_set = working_set;
        }
        replaced
    }

    fn base_working_set(plan: &PreparedSetup<'_>) -> PipelineResult<Preparation> {
        let mut base = Preparation::new(plan.preparation().frames_in_flight());
        base.add_working_set(plan.preparation().working_set())?;
        Ok(base)
    }

    /// Read runtime limits and backend telemetry at observation boundaries.
    pub fn runtime_info(&mut self) -> &Runtime {
        self.drawing.runtime()
    }

    /// Add a texture reservation at an explicit capacity-change boundary.
    pub fn prepare_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        simultaneous: usize,
    ) -> PipelineResult<()> {
        let extend = |old: &Preparation| -> PipelineResult<Preparation> {
            let mut next = Preparation::new(old.frames_in_flight());
            next.add_requirements(old.graph_requirements(), old.working_set())?;
            crate::pipeline::reserve_texture_upload(&mut next, name, desc, simultaneous)?;
            Ok(next)
        };
        let host = extend(&self.host)?;
        let working_set = extend(&self.working_set)?;
        self.drawing.prepare_texture(name, desc, simultaneous)?;
        self.host = host;
        self.working_set = working_set;
        Ok(())
    }

    /// Upload the static surface pages and the corresponding dynamic probe volume.
    pub fn load_surface_lighting(
        &mut self,
        surfaces: &crate::pbr::SurfaceLightmaps,
        reflections: Option<&crate::pbr::ReflectionProbes>,
    ) -> PipelineResult<crate::pipeline::ResidentBakedLighting> {
        self.drawing.load_surface_lighting(surfaces, reflections)
    }

    /// Prepare baked diffuse and specular lighting on this renderer's device.
    pub fn load_probe_lighting(
        &mut self,
        assets: crate::pipeline::ProbeLighting<'_>,
    ) -> PipelineResult<crate::pipeline::ResidentBakedLighting> {
        self.drawing.load_probe_lighting(assets)
    }

    /// Prepare geometry, materials, and local light capacity at a scene change. Supply the active
    /// model selection. Subsequent pose changes use only the scene input.
    pub fn prepare_scene(
        &mut self,
        assets: SceneAssets<'_>,
    ) -> Result<(), crate::AssetPrepareError> {
        self.prepare_scene_inner(assets).map_err(Into::into)
    }
    fn prepare_scene_inner(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        use crate::material::MaterialInstance;
        use std::collections::HashSet;
        let mut meshes = HashSet::new();
        let mut materials = HashSet::new();
        let mut uniforms = 0;
        let mut transforms = 0;
        let mut casters = 0;
        let mut new_program = false;
        for object in assets.objects.iter() {
            meshes.insert(object.mesh);
            casters += usize::from(object.casts_shadow);
            transforms += usize::from(matches!(
                assets.materials.get(object.material)?,
                MaterialInstance::Pbr(_)
            ));
        }
        meshes.extend(assets.extra_meshes.iter().copied());
        for material in assets
            .objects
            .iter()
            .map(|o| o.material)
            .chain(assets.extra_materials.iter().copied())
        {
            if !materials.insert(material) {
                continue;
            }
            match assets.materials.get(material)? {
                MaterialInstance::Pbr(_) => uniforms += 1,
                MaterialInstance::Custom(m) => {
                    new_program |= !self.drawing.definitions.contains(m.definition())
                }
                _ => {}
            }
        }
        // Asset loading can introduce new techniques or more independent objects
        // than the initial scene. Negotiate the change before publishing its assets.
        let limits = self.drawing.limits;
        if new_program
            || assets.shadow_count()? > limits.max_indirect_shadow_capsules
            || assets.draws > limits.max_draws
            || assets
                .objects
                .iter()
                .filter(|o| o.placement().is_some())
                .count()
                .min(assets.draws)
                > limits.max_lightmapped_draws
            || materials.len() > limits.max_materials
            || transforms.min(assets.draws) > limits.max_transforms
            || casters.min(assets.draws) > limits.max_shadow_casters
            || assets.lights.len() > limits.max_local_lights
            || assets
                .lights
                .iter()
                .filter_map(crate::pbr::LocalLight::shadow_emitter)
                .count()
                > limits.max_shadowed_rect_lights
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
        assets.meshes.reserve_poses(&mut active, &assets.objects)?;
        active.reserve_buffer(crate::pbr::MaterialUniform::DESC, uniforms)?;
        assets.meshes.collect_gpu(self.drawing.runtime())?;
        assets.materials.collect_gpu(self.drawing.runtime())?;
        let material_bytes = uniforms
            .checked_mul(crate::passes::pbr::MATERIAL_BYTES)
            .ok_or("material upload overflow")?;
        let transfer = upload.max(material_bytes as u64);
        active.add_graph_requirements(sashiko_renderer_core::capacity::GraphRequirements {
            upload_bytes: usize::try_from(transfer)?,
            passes: uniforms,
            resources: uniforms,
            accesses: uniforms,
            ..Default::default()
        })?;
        self.drawing
            .replace_resource_reservations(active, transfer)?;
        self.drawing.prepare_assets_inner(assets)
    }

    pub fn begin_frame(&mut self) -> PipelineResult<Frame<'_>> {
        self.poll_abandoned_captures()?;
        let recording = self.drawing.begin_frame()?;
        Ok(Frame {
            recording,
            captures: &mut self.captures,
            camera: CameraView {
                extent: self.scene_extent.unwrap_or(self.camera.extent),
                ..self.camera
            },
            output_extent: self.camera.extent,
            film_capture: self.config.film_output
                && matches!(self.destination, FrameDestination::Capture),
            linear: None,
        })
    }

    pub fn into_runtime(self) -> PipelineResult<Runtime> {
        self.drawing.into_runtime()
    }

    /// Retire completed captures whose tickets were dropped. Live tickets and
    /// other submissions remain available to their owners. Never waits for the GPU.
    /// Returns the number of abandoned captures still pending on the GPU.
    pub fn poll_abandoned_captures(&mut self) -> PipelineResult<usize> {
        let mut pending = 0;
        for slot in &mut self.captures {
            if let Some(id) = slot.frame
                && slot.abandoned.load(Ordering::Acquire)
            {
                if self
                    .drawing
                    .runtime()
                    .wait_for_frame(id, Duration::ZERO)?
                    .is_some()
                {
                    slot.frame = None;
                    slot.abandoned.store(false, Ordering::Relaxed);
                } else {
                    pending += 1;
                }
            }
        }
        Ok(pending)
    }

    pub fn read_capture(
        &mut self,
        capture: &Capture,
        bytes: &mut [u8],
        timeout: Duration,
    ) -> PipelineResult<bool> {
        let Some(slot) = self.captures.get_mut(capture.slot) else {
            return Err("unknown capture ticket".into());
        };
        if slot.frame != Some(capture.frame) || !Arc::ptr_eq(&slot.abandoned, &capture.abandoned) {
            return Err("capture belongs to another renderer or has already been read".into());
        }
        let [w, h] = capture.extent;
        if usize::try_from(u64::from(w) * u64::from(h))
            .ok()
            .and_then(|n| n.checked_mul(capture.texel_bytes))
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
        // The runtime has transferred this completion, even if reading it fails.
        capture.consumed.store(true, Ordering::Relaxed);
        slot.frame = None;
        result.readback_into(capture.pixels, bytes)?;
        Ok(true)
    }
}

#[derive(Default)]
struct CaptureSlot {
    frame: Option<FrameId>,
    // Allocated once per slot. A ticket can signal abandonment from any thread.
    abandoned: Arc<AtomicBool>,
}

/// Dropping an unread ticket schedules its result for nonblocking collection.
#[must_use]
pub struct Capture {
    frame: FrameId,
    pixels: PassHandle,
    extent: [u32; 2],
    texel_bytes: usize,
    abandoned: Arc<AtomicBool>,
    consumed: AtomicBool,
    slot: usize,
}
impl Capture {
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        if !self.consumed.load(Ordering::Relaxed) {
            self.abandoned.store(true, Ordering::Release);
        }
    }
}

pub struct Frame<'a> {
    recording: SceneFrame<'a>,
    captures: &'a mut [CaptureSlot],
    camera: CameraView,
    output_extent: [u32; 2],
    film_capture: bool,
    linear: Option<ImageHandle>,
}

impl Frame<'_> {
    /// Camera and actual 3D resolution used for view-dependent extraction and LOD.
    pub fn scene_view(&self) -> &CameraView {
        &self.camera
    }
    pub fn graph(&mut self) -> &mut Graph {
        self.recording.graph()
    }

    fn new_target(&mut self) -> Result<ImageHandle, GraphError> {
        let [w, h] = self.output_extent;
        self.graph().image(LINEAR, ImageDesc::rgba16_float(w, h))
    }

    /// Draw scene-linear geometry and apply the camera exposure before UI compositing.
    pub fn draw_scene(&mut self, input: SceneInput<'_>, exposure: f32) -> Result<(), GraphError> {
        self.draw_with_output(input, |graph, view, target| {
            output::tone_map_neutral(graph, TONE_MAP, view, target, exposure)
        })
    }

    /// Composite geometry rendered over transparent black with the source sky
    /// and apply the physical camera's linear response before display encoding.
    pub fn draw_film(
        &mut self,
        input: SceneInput<'_>,
        film: output::FilmOutput<'_>,
    ) -> Result<(), GraphError> {
        self.draw_with_output(input, |graph, view, target| {
            output::film_composite(graph, TONE_MAP, view, target, film)
        })
    }

    fn draw_with_output(
        &mut self,
        input: SceneInput<'_>,
        compose: impl FnOnce(
            &mut Graph,
            sashiko_renderer_core::graph::ImageView,
            ImageHandle,
        ) -> Result<PassHandle, GraphError>,
    ) -> Result<(), GraphError> {
        if self.linear.is_some() {
            return Err(GraphError::invalid_input(
                "draw the main scene before its overlays",
            ));
        }
        let hdr = self.recording.draw_scene(&self.camera, input)?.color;
        let view = self.graph().image_view(hdr, 0, 0)?;
        let scaled = self.camera.extent != self.output_extent;
        let target = if scaled {
            let [w, h] = self.camera.extent;
            self.graph()
                .image(SCENE_LINEAR, ImageDesc::rgba16_float(w, h))?
        } else {
            self.new_target()?
        };
        compose(self.graph(), view, target)?;
        if scaled {
            let image = self.graph().image_view(target, 0, 0)?;
            let [w, h] = self.output_extent;
            return self.draw_overlay(&[Layer {
                rect: sashiko_renderer_core::graph::RasterRect::new(0, 0, w, h),
                clip: None,
                paint: canvas::Paint::Image(image),
            }]);
        }
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
        let [w, h] = self.output_extent;
        let target = self.graph().image(OUTPUT, ImageDesc::rgba8(w, h))?;
        output::write_sdr(self.graph(), TRANSFER, source, target, encoding)
    }

    /// Submit this frame and obtain its sRGB readback ticket without waiting.
    pub fn capture(mut self) -> PipelineResult<Capture> {
        let image = self.output(SdrOutput::capture(false))?.raw_image();
        self.submit_capture(image, 4)
    }

    /// Capture the composed linear film as tightly packed RGBA16F, before
    /// display clipping and encoding. Prepared by headless physical-film views.
    pub fn capture_linear(self) -> PipelineResult<Capture> {
        if !self.film_capture {
            return Err("linear film capture requires a prepared headless film output".into());
        }
        let image = self.linear.ok_or("draw the film before capturing it")?;
        self.submit_capture(image, 8)
    }

    fn submit_capture(mut self, image: ImageHandle, texel_bytes: usize) -> PipelineResult<Capture> {
        let slot = self
            .captures
            .iter()
            .position(|slot| slot.frame.is_none())
            .ok_or("outstanding captures exhaust the prepared frame count")?;
        let pixels = self.graph().readback_image(READBACK, image)?;
        let accepted: FrameSubmission = self.recording.submit()?;
        self.captures[slot].frame = Some(accepted.id());
        Ok(Capture {
            abandoned: Arc::clone(&self.captures[slot].abandoned),
            consumed: AtomicBool::new(false),
            slot,
            frame: accepted.id(),
            pixels,
            extent: self.output_extent,
            texel_bytes,
        })
    }
}
