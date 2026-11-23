//! Scene-derived preparation at an explicit engine startup or change boundary.
use super::*;
use crate::{
    material::{MaterialCheckpoint, MaterialHandle, MaterialInstance, Materials},
    mesh::{MeshCheckpoint, MeshHandle, Meshes},
    passes::output::SdrOutput,
};
use renderer_core::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{GraphError, GraphPool, ImageDesc},
    runtime::{FrameId, FrameSubmission, ManagedImage, Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{
    borrow::{BorrowMut, Cow},
    collections::BTreeSet,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    time::Duration,
};

mod build;
pub use build::RendererBuild;

/// Registries remain engine-owned and are borrowed exclusively only during setup.
pub struct SceneAssets<'a> {
    pub(crate) meshes: &'a mut Meshes,
    pub(crate) materials: &'a mut Materials,
    pub(crate) objects: Cow<'a, [RenderObject]>,
    pub(crate) draws: usize,
    pub(crate) extra_meshes: Cow<'a, [MeshHandle]>,
    pub(crate) extra_materials: Cow<'a, [MaterialHandle]>,
    pub(crate) lights: &'a [LocalLight],
}
impl<'a> SceneAssets<'a> {
    pub fn new(
        meshes: &'a mut Meshes,
        materials: &'a mut Materials,
        objects: &'a [RenderObject],
    ) -> Self {
        Self {
            meshes,
            materials,
            objects: objects.into(),
            draws: objects.len(),
            extra_meshes: Cow::Borrowed(&[]),
            extra_materials: Cow::Borrowed(&[]),
            lights: &[],
        }
    }
    /// Include every possible LOD, reserving draws for the largest single LOD.
    pub fn add_model(&mut self, model: &crate::model::ModelInstance) {
        self.objects.to_mut().extend(model.asset_draws());
        self.draws += model.max_parts();
    }
    pub fn add_object(&mut self, object: RenderObject) {
        self.objects.to_mut().push(object);
        self.draws += 1;
    }
    /// Pre-register an alternate pairing for an existing draw slot.
    pub fn preload_pair(&mut self, mesh: MeshHandle, material: MaterialHandle) {
        self.objects
            .to_mut()
            .push(RenderObject::new(mesh, material));
    }
    pub fn preload_mesh(&mut self, mesh: MeshHandle) {
        self.extra_meshes.to_mut().push(mesh);
    }
    pub fn preload_material(&mut self, material: MaterialHandle) {
        self.extra_materials.to_mut().push(material);
    }
    pub fn with_lights(mut self, lights: &'a [LocalLight]) -> Self {
        self.lights = lights;
        self
    }
    /// Borrow the same working set for a retryable preparation attempt.
    pub fn reborrow(&mut self) -> SceneAssets<'_> {
        SceneAssets {
            meshes: self.meshes,
            materials: self.materials,
            objects: self.objects.as_ref().into(),
            draws: self.draws,
            extra_meshes: Cow::Borrowed(&self.extra_meshes),
            extra_materials: Cow::Borrowed(&self.extra_materials),
            lights: self.lights,
        }
    }
    fn input(&self) -> SceneInput<'_> {
        SceneInput::new(self.meshes, self.materials, &self.objects).with_lights(self.lights)
    }
}

type Register = Box<
    dyn FnOnce(
        &mut ViewRenderer,
        [u32; 2],
        NonZeroUsize,
        Option<SdrOutput>,
        &mut Preparation,
        SceneLimits,
        &[MaterialDefinition],
    ) -> PipelineResult<PipelineId>,
>;

/// Cameras sharing one pipeline instance, extent and output configuration.
pub struct CameraGroup {
    cameras: Vec<CameraView>,
    register: Register,
    pub output: Option<SdrOutput>,
}
impl CameraGroup {
    pub fn standard(camera: CameraView, technique: Technique, config: StandardConfig) -> Self {
        Self::custom(camera, move |_, limits, definitions| {
            Ok(StandardPipeline::new(
                technique,
                config,
                limits,
                definitions,
            )?)
        })
    }

    /// The factory receives the dimensions, scene-derived limits and custom material definitions.
    pub fn custom<P: ViewPipeline + 'static>(
        camera: CameraView,
        make: impl FnOnce([u32; 2], SceneLimits, &[MaterialDefinition]) -> PipelineResult<P> + 'static,
    ) -> Self {
        Self {
            cameras: vec![camera],
            output: Some(SdrOutput::display(false)),
            register: Box::new(
                move |renderer, extent, count, output, preparation, limits, definitions| {
                    Ok(renderer.prepare_pipeline(
                        make(extent, limits, definitions)?,
                        extent,
                        count,
                        output,
                        preparation,
                    )?)
                },
            ),
        }
    }
    pub fn add_view(&mut self, camera: CameraView) -> Result<(), GraphError> {
        if camera.extent != self.cameras[0].extent {
            return Err(GraphError::invalid_input(
                "camera extent differs from its group",
            ));
        }
        self.cameras.push(camera);
        Ok(())
    }
}

/// Cold configuration shared by blocking and incremental startup.
pub struct RendererSetup {
    groups: Vec<CameraGroup>,
    pub frames_in_flight: NonZeroUsize,
    /// Capacity for additional independent draws, materials, casters and model transforms.
    pub additional_objects: usize,
    /// Extra local lights with no dedicated shadow atlas reservation.
    pub additional_lights: usize,
    /// Extra lights with dedicated shadow atlas space. Also reserves their light records.
    pub additional_shadowed_rect_lights: usize,
    /// Maximum shared geometry bytes per startup submission, rounded down to four bytes.
    pub mesh_upload_bytes: u64,
}
impl RendererSetup {
    /// One standard Forward camera with scene-linear HDR and optional display output.
    pub fn standard(camera: CameraView) -> Self {
        Self::new(CameraGroup::standard(
            camera,
            Technique::Forward,
            StandardConfig::new(camera.extent),
        ))
    }
    pub fn new(group: CameraGroup) -> Self {
        Self {
            groups: vec![group],
            frames_in_flight: NonZeroUsize::MIN,
            additional_objects: 0,
            additional_lights: 0,
            additional_shadowed_rect_lights: 0,
            mesh_upload_bytes: 1024 * 1024,
        }
    }
    pub fn add_group(&mut self, group: CameraGroup) {
        self.groups.push(group);
    }

    pub fn prepare(self, assets: SceneAssets<'_>) -> PipelineResult<PreparedSetup<'_>> {
        if self.mesh_upload_bytes < 4 {
            return Err("mesh upload budget must fit a four-byte element".into());
        }
        let mut meshes = Vec::new();
        let mut seen = HashSet::new();
        let mut materials = BTreeSet::new();
        let mut seen_definitions = HashSet::new();
        let mut definitions = Vec::new();
        let mut pbr_objects = 0;
        let mut casters = 0;
        let mut uniforms = 0usize;
        let mut mesh_bytes = 0u64;
        for object in assets.objects.iter() {
            let geometry = assets.meshes.geometry(object.mesh)?;
            if object
                .index_range
                .is_some_and(|r| r.indices().end > geometry.index_count())
            {
                return Err("submesh exceeds the staged index buffer".into());
            }
            let instance = assets.materials.get(object.material)?;
            if matches!(instance, MaterialInstance::Pbr(_)) {
                pbr_objects += 1;
            }
            if object.casts_shadow {
                casters += 1;
            }
        }
        for mesh in assets
            .objects
            .iter()
            .map(|o| o.mesh)
            .chain(assets.extra_meshes.iter().copied())
        {
            let geometry = assets.meshes.geometry(mesh)?;
            if seen.insert(mesh) {
                meshes.push(mesh);
                mesh_bytes = mesh_bytes
                    .checked_add(geometry.byte_len())
                    .ok_or("geometry size overflow")?;
            }
        }
        for material in assets
            .objects
            .iter()
            .map(|o| o.material)
            .chain(assets.extra_materials.iter().copied())
        {
            let instance = assets.materials.get(material)?;
            if materials.insert(material) {
                match instance {
                    MaterialInstance::Pbr(p) if !p.is_resident() => uniforms += 1,
                    MaterialInstance::Custom(m)
                        if seen_definitions.insert(m.definition().clone()) =>
                    {
                        definitions.push(m.definition().clone());
                    }
                    _ => {}
                }
            }
        }
        let room = |count: usize| {
            count
                .checked_add(self.additional_objects)
                .ok_or("scene capacity overflow")
        };
        let limits = SceneLimits {
            max_lightmapped_draws: assets
                .objects
                .iter()
                .filter(|o| o.placement().is_some())
                .count()
                .min(assets.draws),
            frames_in_flight: self.frames_in_flight,
            max_draws: room(assets.draws)?,
            max_materials: room(materials.len())?,
            max_transforms: room(pbr_objects.min(assets.draws))?,
            max_shadow_casters: room(casters.min(assets.draws))?,
            max_shadowed_rect_lights: assets
                .lights
                .iter()
                .filter_map(LocalLight::shadow_emitter)
                .count()
                .checked_add(self.additional_shadowed_rect_lights)
                .ok_or("shadow light capacity overflow")?,
            max_local_lights: assets
                .lights
                .len()
                .checked_add(self.additional_lights)
                .and_then(|n| n.checked_add(self.additional_shadowed_rect_lights))
                .ok_or("light capacity overflow")?,
        };
        let mut preparation = Preparation::new(self.frames_in_flight);
        let mut driver = ViewRenderer::new();
        let mut groups = Vec::new();
        for group in self.groups {
            let id = (group.register)(
                &mut driver,
                group.cameras[0].extent,
                NonZeroUsize::new(group.cameras.len()).expect("group has a camera"),
                group.output,
                &mut preparation,
                limits,
                &definitions,
            )?;
            for camera in &group.cameras {
                driver.register_view(id, camera)?;
            }
            groups.push(id);
        }
        assets
            .meshes
            .reserve_selection(&mut preparation, meshes.iter().copied())?;
        assets
            .meshes
            .reserve_poses(&mut preparation, &assets.objects)?;
        preparation.add_requirements(
            GraphRequirements {
                resources: uniforms,
                passes: uniforms,
                accesses: uniforms,
                upload_bytes: uniforms
                    .checked_mul(crate::passes::pbr::MATERIAL_BYTES)
                    .ok_or("material upload overflow")?,
                name_bytes: uniforms
                    .checked_mul(2 * "material factors".len())
                    .ok_or("material name capacity overflow")?,
                ..Default::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Buffer {
                    desc: pbr::MaterialUniform::DESC,
                    count: uniforms,
                }],
                ..Default::default()
            },
        )?;
        Ok(PreparedSetup {
            assets,
            meshes,
            materials,
            driver,
            groups,
            preparation,
            limits,
            definitions,
            mesh_budget: self.mesh_upload_bytes,
            transfer: mesh_bytes.min(self.mesh_upload_bytes),
            readback: 0,
        })
    }

    pub fn begin_build(
        self,
        backend: RuntimeConfig,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<RendererBuild<'_, Runtime>> {
        self.prepare(assets)?.begin_build(backend)
    }
    pub fn build(
        self,
        backend: RuntimeConfig,
        assets: SceneAssets<'_>,
    ) -> PipelineResult<SceneRenderer> {
        self.begin_build(backend, assets)?.wait()
    }
}

/// CPU preparation. Compose engine passes and assets here before creating or updating a runtime.
pub struct PreparedSetup<'a> {
    assets: SceneAssets<'a>,
    meshes: Vec<MeshHandle>,
    materials: BTreeSet<MaterialHandle>,
    driver: ViewRenderer,
    groups: Vec<PipelineId>,
    preparation: Preparation,
    limits: SceneLimits,
    definitions: Vec<MaterialDefinition>,
    mesh_budget: u64,
    transfer: u64,
    readback: u64,
}

impl<'a> PreparedSetup<'a> {
    pub fn scene_limits(&self) -> SceneLimits {
        self.limits
    }
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }
    pub fn preparation_mut(&mut self) -> &mut Preparation {
        &mut self.preparation
    }
    /// Reserve a second vertex/index pair of this mesh's sizes for an asset
    /// update while a previous version is still drawable.
    pub fn reserve_mesh_replacement(&mut self, mesh: MeshHandle) -> Result<(), GraphError> {
        self.assets
            .meshes
            .reserve_replacement(mesh, &mut self.preparation)
    }

    /// One persistent image and a standalone full-texture upload.
    pub fn reserve_texture(&mut self, name: &str, desc: ImageDesc) -> PipelineResult<()> {
        if desc.array_layers != 1 || desc.format == renderer_core::ImageFormat::Depth32Float {
            return Err("texture upload requires a single-layer color image".into());
        }
        let footprint = transfer_footprint(desc)?;
        let bytes = usize::try_from(desc.byte_len().ok_or("invalid texture")?)?;
        self.preparation.add_requirements(
            GraphRequirements {
                resources: 1,
                passes: 1,
                accesses: 1,
                upload_bytes: bytes,
                name_bytes: name.len().checked_mul(2).ok_or("texture name overflow")?,
                ..Default::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Image { desc, count: 1 }],
                ..Default::default()
            },
        )?;
        self.transfer = self.transfer.max(footprint);
        Ok(())
    }
    /// One full-image readback per frame. Call once per simultaneously read image.
    pub fn reserve_readback(&mut self, name: &str, desc: ImageDesc) -> PipelineResult<()> {
        if desc.mip_levels != 1 || desc.array_layers != 1 {
            return Err("image readback requires one mip and one layer".into());
        }
        let total = self
            .readback
            .checked_add(transfer_footprint(desc)?)
            .ok_or("readback size overflow")?;
        self.preparation.add_graph_requirements(GraphRequirements {
            accesses: 1,
            passes: 1,
            name_bytes: name.len(),
            ..Default::default()
        })?;
        self.readback = total;
        Ok(())
    }
    pub fn runtime_limits(&self, base: RuntimeLimits) -> Result<RuntimeLimits, GraphError> {
        self.preparation.replacement_limits(base)
    }
    /// Fit engine-supplied backend settings before constructing a headless, windowed or adopted runtime.
    pub fn fit_backend(&self, backend: &mut RuntimeConfig) -> PipelineResult<()> {
        backend.fit_preparation(&self.preparation, self.upload_capacity()?, self.readback)?;
        Ok(())
    }
    /// Install this complete working set and grow recording storage on an existing
    /// device. Include the host's live passes in the plan before calling this.
    pub fn prepare_runtime(&self, runtime: &mut Runtime) -> PipelineResult<()> {
        Ok(runtime.prepare_fitted_working_set(
            &self.preparation,
            self.upload_capacity()?,
            self.readback,
        )?)
    }
    fn upload_capacity(&self) -> PipelineResult<u64> {
        let req = self.preparation.graph_requirements();
        // Allow placement padding between native image transfers in the same submission.
        let padding = (req.passes as u64)
            .checked_mul(512)
            .ok_or("native transfer size overflow")?;
        let bytes = (req.upload_bytes as u64)
            .checked_add(padding)
            .ok_or("native transfer size overflow")?;
        Ok(self.transfer.max(bytes))
    }
    pub fn begin_build(
        self,
        mut backend: RuntimeConfig,
    ) -> PipelineResult<RendererBuild<'a, Runtime>> {
        self.fit_backend(&mut backend)?;
        let runtime = Runtime::new(
            backend,
            self.runtime_limits(RuntimeLimits::default())?,
            self.preparation.working_set(),
        )?;
        self.begin_with_runtime(runtime)
    }

    pub fn build(self, backend: RuntimeConfig) -> PipelineResult<SceneRenderer> {
        self.begin_build(backend)?.wait()
    }
    /// The host first includes this preparation in its complete working set.
    /// Accepts either owned Runtime or an exclusive borrow. Never replaces the host working set.
    pub fn begin_with_runtime<R: BorrowMut<Runtime>>(
        self,
        runtime: R,
    ) -> PipelineResult<RendererBuild<'a, R>> {
        RendererBuild::new(self, runtime)
    }
    fn check_runtime(&self, runtime: &Runtime) -> PipelineResult<()> {
        use renderer_core::capacity::{
            CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind,
        };
        let limits = runtime.limits();
        let required = self.runtime_limits(limits)?;
        runtime.check_transfer_and_binding_limits(
            &self.preparation,
            self.upload_capacity()?,
            self.readback,
        )?;
        let insufficient = |kind, limit, required| {
            RuntimeCapacityError::new(kind, CapacityRecovery::Reconfigure, limit, required)
        };
        if limits.graph != required.graph {
            return Err(insufficient(RuntimeCapacityKind::GraphMetadata, None, None).into());
        }
        // Extra metadata reserved for a future replacement is optional on a shared runtime.
        for (kind, current, requested) in [
            (
                RuntimeCapacityKind::ExecutionSlots,
                limits.frames,
                self.limits.frames_in_flight.get(),
            ),
            (
                RuntimeCapacityKind::ResultSlots,
                limits.results,
                self.limits.frames_in_flight.get(),
            ),
            (
                RuntimeCapacityKind::Resources,
                limits.resource_capacity,
                self.preparation.resource_count()?,
            ),
        ] {
            if current < requested {
                return Err(
                    insufficient(kind, Some(current as u64), Some(requested as u64)).into(),
                );
            }
        }
        Ok(())
    }
}

fn transfer_footprint(desc: ImageDesc) -> PipelineResult<u64> {
    if desc.sample_count != renderer_core::graph::SampleCount::X1 || desc.byte_len().is_none() {
        return Err("transfer requires a valid single-sample image".into());
    }
    let mut total = 0u64;
    for mip in 0..desc.mip_levels {
        let [width, height] = desc.mip_extent(mip).ok_or("invalid mip")?;
        let pitch = desc
            .format
            .packed_row_bytes(width)
            .checked_next_multiple_of(256)
            .ok_or("row pitch overflow")?;
        let slice = pitch
            .checked_mul(u64::from(desc.format.packed_rows(height)))
            .and_then(|n| n.checked_next_multiple_of(512))
            .and_then(|n| n.checked_mul(u64::from(desc.array_layers)))
            .ok_or("image footprint overflow")?;
        total = total.checked_add(slice).ok_or("image footprint overflow")?;
    }
    Ok(total)
}

mod scene_renderer;
pub use self::scene_renderer::{CompiledSceneFrame, RetiredViewError, SceneFrame, SceneRenderer};
