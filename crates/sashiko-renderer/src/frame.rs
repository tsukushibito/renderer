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
    runtime::{FrameId, FrameSubmission, ManagedImage, Runtime, RuntimeConfig},
};
use std::{num::NonZeroUsize, time::Duration};

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
    drawing: SceneRenderer,
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
        jobs: impl Into<sashiko_renderer_core::jobs::RendererJobs>,
    ) -> PipelineResult<()> {
        self.drawing.prepare_jobs(jobs)
    }
    fn prepare<'a>(
        camera: CameraView,
        scene_extent: Option<[u32; 2]>,
        assets: SceneAssets<'a>,
        destination: FrameDestination,
        frames_in_flight: NonZeroUsize,
    ) -> PipelineResult<PreparedSetup<'a>> {
        let scene = CameraView {
            extent: scene_extent.unwrap_or(camera.extent),
            ..camera
        };
        let mut group =
            CameraGroup::standard(scene, Technique::Forward, StandardConfig::new(scene.extent));
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
        let destination = FrameDestination::Capture;
        let mut plan = Self::prepare(camera, None, assets, destination, frames_in_flight)?;
        let host = Self::prepare_host(&mut plan, configure)?;
        let working_set = Self::base_working_set(&plan)?;
        let drawing = plan.build(backend)?;
        Ok(Self {
            drawing,
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

    /// Check format/extent and filtering on the device used by this renderer.
    pub fn check_texture_support(
        &mut self,
        desc: ImageDesc,
        linear_filtering: bool,
    ) -> PipelineResult<()> {
        self.drawing.check_texture_support(desc, linear_filtering)
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

    /// Upload one complete texture. Retain the returned owner until its last use is submitted.
    pub fn upload_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        self.drawing.upload_texture(name, desc, bytes)
    }

    /// Upload a contiguous mip range from level-major packed data.
    pub fn upload_texture_mips(
        &mut self,
        name: &str,
        desc: ImageDesc,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        self.drawing.upload_texture_mips(name, desc, levels, bytes)
    }

    /// Create a candidate from an existing version and the supplied new mip contents.
    pub fn promote_texture(
        &mut self,
        name: &str,
        previous: &ManagedImage,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> PipelineResult<(FrameId, ManagedImage)> {
        self.drawing.promote_texture(name, previous, levels, bytes)
    }

    /// Consume the upload completion without waiting on the GPU.
    pub fn poll_texture_upload(&mut self, id: FrameId) -> PipelineResult<bool> {
        self.drawing.poll_texture_upload(id)
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
        let recording = self.drawing.begin_frame()?;
        Ok(Frame {
            recording,
            camera: CameraView {
                extent: self.scene_extent.unwrap_or(self.camera.extent),
                ..self.camera
            },
            output_extent: self.camera.extent,
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
    output_extent: [u32; 2],
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
        output::tone_map_neutral(self.graph(), TONE_MAP, view, target, exposure)?;
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
        let output = self.output(SdrOutput::capture(false))?;
        let pixels = self.graph().readback_image(READBACK, output.raw_image())?;
        let accepted: FrameSubmission = self.recording.submit()?;
        Ok(Capture {
            frame: accepted.id(),
            pixels,
            extent: self.output_extent,
        })
    }
}
