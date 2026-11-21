//! Public Graph composition and explicit working-set budgets for this consumer.

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::camera::Frustum;
use super::{
    Bloom, Error, Options, PipelineMode, SubmissionMode, asset::Scene, camera::Camera, ibl,
    instancing, lighting, program, recipes, shadow,
};
use renderer::{
    ImageFormat,
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{
        Binding, BindingResource, BufferHandle, Graph, GraphCapacity, GraphError, GraphPool,
        ImageDesc, ImageHandle, ImageView, ImportBuffer, ImportImage, IndexedIndirectArgs,
        PassHandle, RasterBindings, RasterConstants, RasterDraw,
    },
    program::{MAX_RASTER_BINDINGS, ManagedRasterProgram, SamplerDescriptor},
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    graph::BufferDesc,
    runtime::{ExecutionError, FrameSubmission, Runtime},
};
use renderer_pipeline::{
    pbr::{
        self, MaterialUniform, ResidentMaterialUniform, ShadowFrame, ShadowHistory, ShadowKey,
        ShadowUpdates,
    },
    pbr_forward, pbr_forward_plus,
};
use std::num::NonZeroUsize;
use std::sync::Arc;

#[path = "pipeline/indirect.rs"]
pub(super) mod indirect;
mod standard_forward;

const CASCADES: [&str; shadow::CASCADE_COUNT] =
    ["cascade_0", "cascade_1", "cascade_2", "cascade_3"];

pub(super) struct PreparedSet {
    main: recipes::Prepared,
    aa_off: Option<recipes::Prepared>,
    bloom_bypass: Option<recipes::Prepared>,
    reservation: Preparation,
    instance_draws: usize,
}

impl PreparedSet {
    fn new(
        options: &Options,
        scene: &Scene,
        layout: Option<&indirect::Layout>,
        background: [f32; 3],
    ) -> Result<Self, Error> {
        options.validate_lighting_extent()?;
        let prepare = |options: &Options| {
            recipes::Prepared::new(
                options,
                scene,
                layout,
                [background[0], background[1], background[2], 1.0],
            )
        };
        let main = prepare(options)?;
        let aa_off = options
            .captures_aa_off()
            .then(|| {
                let mut diagnostic = options.clone();
                diagnostic.aa = false;
                prepare(&diagnostic)
            })
            .transpose()?;
        let bloom_bypass = options
            .capture_bloom_bypass
            .then(|| {
                let mut diagnostic = options.clone();
                diagnostic.bloom = Bloom::Off;
                prepare(&diagnostic)
            })
            .transpose()?;
        // Diagnostic captures begin after the measured frames drain.
        let mut reservation = Preparation::new(main.preparation().frames_in_flight());
        let alternatives = [Some(&main), aa_off.as_ref(), bloom_bypass.as_ref()]
            .into_iter()
            .flatten()
            .map(recipes::Prepared::preparation)
            .collect::<Vec<_>>();
        reservation.add_alternatives(&alternatives)?;
        Ok(Self {
            main,
            aa_off,
            bloom_bypass,
            reservation,
            instance_draws: options.instances.draws(),
        })
    }

    pub fn working_set(&self) -> WorkingSet<'_> {
        self.reservation.working_set()
    }

    fn select(&mut self, options: &Options) -> &mut recipes::Prepared {
        if options.bloom == Bloom::Off
            && let Some(prepared) = &mut self.bloom_bypass
        {
            return prepared;
        }
        if !options.aa
            && let Some(prepared) = &mut self.aa_off
        {
            return prepared;
        }
        &mut self.main
    }
}

pub(super) struct Budget {
    pub graph: GraphCapacity,
    pub prepared: Option<PreparedSet>,
    #[cfg(feature = "vulkan")]
    pub vulkan_shader_bindings: u32,
    pub buffers: Vec<(u64, usize)>,
    pub images: Vec<(ImageDesc, usize)>,
    pub buffer_count: usize,
    pub physical_resources: usize,
    pub max_buffer_bytes: u64,
    pub upload_bytes: usize,
    pub readback_bytes: u64,
    pub ranges: usize,
    pipeline: PipelineMode,
    defaults: Option<pbr::PreparedDefaults>,
    fixture: bool,
    packed: Option<Arc<indirect::Layout>>,
    indirect: bool,
}

pub(super) fn image_desc(image: &super::asset::TextureImage) -> ImageDesc {
    ImageDesc {
        format: if image.srgb {
            ImageFormat::Rgba8Srgb
        } else {
            ImageFormat::Rgba8Unorm
        },
        mip_levels: image.mips.len() as u32,
        ..ImageDesc::rgba8(image.width, image.height)
    }
}

fn environment_desc(image: &ibl::Image) -> ImageDesc {
    ImageDesc {
        mip_levels: image.mips.len() as u32,
        ..ImageDesc::rgba16_float(image.width, image.height)
    }
}

fn add_instances<T: PartialEq>(classes: &mut Vec<(T, usize)>, desc: T, count: usize) {
    if let Some((_, available)) = classes.iter_mut().find(|(item, _)| *item == desc) {
        *available += count;
    } else {
        classes.push((desc, count));
    }
}

impl Budget {
    fn frame_buffers(prepared: &PreparedSet) -> impl Iterator<Item = (u64, usize)> + '_ {
        prepared
            .working_set()
            .resources
            .iter()
            .filter_map(|class| match *class {
                ResourceClass::Buffer { desc, count } => Some((desc.size, count)),
                ResourceClass::Image { .. } => None,
            })
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn resized_buffers(&self, prepared: &PreparedSet) -> Vec<(u64, usize)> {
        Self::frame_buffers(prepared).collect()
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn prepare_frame(
        &self,
        options: &Options,
        scene: &Scene,
        background: [f32; 3],
    ) -> Result<PreparedSet, Error> {
        PreparedSet::new(options, scene, self.packed.as_deref(), background)
    }

    pub fn packed_counts(&self) -> Option<(usize, usize)> {
        self.packed
            .as_ref()
            .map(|layout| (layout.group_count(), layout.draw_count()))
    }

    #[cfg(feature = "vulkan")]
    pub fn vulkan_staging_bytes(&self, inflight: usize) -> u64 {
        // Vulkan's required limit for nonCoherentAtomSize is 256 bytes.
        const ALIGNMENT: u64 = 256;
        let templates = self
            .packed
            .as_ref()
            .filter(|_| self.indirect)
            // Each template upload is already a multiple of one aligned block.
            .map_or(0, |layout| layout.template_upload_bytes() as u64);
        // Camera, Deferred reconstruction and local light uploads use at most
        // three independently aligned reservations in each in-flight Graph.
        let frame = templates + self.graph.upload_bytes as u64 + 3 * (ALIGNMENT - 1);
        (self.upload_bytes as u64)
            .max(frame * inflight as u64)
            .max(self.readback_bytes)
            .next_multiple_of(ALIGNMENT)
            + 8192
    }

    #[cfg(feature = "vulkan")]
    pub fn vulkan_staging_ranges(&self, inflight: usize) -> usize {
        let indirect_groups = self
            .packed
            .as_ref()
            .filter(|_| self.indirect)
            .map_or(0, |layout| layout.group_count());
        // Each indirect group contributes one template upload per in-flight Graph.
        64 + inflight * (4 + indirect_groups)
    }

    pub fn frame_images(options: &Options, prepared: &PreparedSet) -> Vec<(ImageDesc, usize)> {
        let mut classes = prepared
            .working_set()
            .resources
            .iter()
            .filter_map(|class| match *class {
                ResourceClass::Image { desc, count } => Some((desc, count)),
                ResourceClass::Buffer { .. } => None,
            })
            .collect::<Vec<_>>();
        if options.windowed {
            // Images accepted by the Presenter can outlive the submitted Graph.
            add_instances(
                &mut classes,
                ImageDesc::rgba8(options.width, options.height),
                options.inflight,
            );
        }
        classes
    }

    pub fn new(
        scene: &Scene,
        environment: &ibl::Environment,
        options: &Options,
        background: [f32; 3],
    ) -> Result<Self, Error> {
        let tiled_lights = usize::from(options.pipeline == PipelineMode::ForwardPlus);
        let packed = if options.submission == SubmissionMode::Direct {
            None
        } else {
            indirect::Layout::new(scene)?.map(Arc::new)
        };
        let indirect = options.submission == SubmissionMode::Indirect;
        let prepared = PreparedSet::new(options, scene, packed.as_deref(), background)?;
        let defaults = prepared.main.defaults();
        let packed_buffers = packed.as_ref().map_or(0, |_| 2 + usize::from(indirect));
        let buffer_count = scene
            .primitives
            .len()
            .checked_mul(2)
            .and_then(|v| v.checked_add(scene.materials.len() + 1 + packed_buffers))
            .ok_or("buffer count overflow")?;
        let mut buffers = Vec::<(u64, usize)>::new();
        let mut add = |size: u64| {
            if let Some((_, count)) = buffers.iter_mut().find(|(old, _)| *old == size) {
                *count += 1
            } else {
                buffers.push((size, 1))
            }
        };
        for primitive in &scene.primitives {
            add(primitive.vertices.len() as u64);
            add(primitive.indices.len() as u64);
        }
        for _ in &scene.materials {
            add(program::MATERIAL_BYTES as u64);
        }
        add(program::CAMERA_BYTES as u64);
        if let Some(layout) = &packed {
            add(layout.vertex_bytes as u64);
            add(layout.index_bytes() as u64);
            if indirect {
                add(layout.aabb_bytes() as u64);
            }
        }
        let mut frame_buffers = 0;
        for (size, count) in Self::frame_buffers(&prepared) {
            for _ in 0..count {
                add(size);
            }
            frame_buffers += count;
        }
        if options.resource_checks {
            // Explicit change-boundary overlap: the old material can still be
            // referenced by a submission while its new version is uploaded.
            add(program::MATERIAL_BYTES as u64);
        }
        let max_buffer_bytes = buffers
            .iter()
            .map(|(size, _)| *size)
            .max()
            .ok_or("empty buffer set")?;
        // Both streams are published by one Graph, so both upload reservations
        // coexist until completion.
        let geometry_upload_bytes = scene.primitives.iter().try_fold(0, |maximum, primitive| {
            primitive
                .vertices
                .len()
                .checked_add(primitive.indices.len())
                .map(|bytes| maximum.max(bytes))
                .ok_or("geometry upload byte overflow")
        })?;
        let geometry_upload_bytes = if let Some(layout) = &packed {
            geometry_upload_bytes.max(
                layout
                    .vertex_bytes
                    .checked_add(layout.index_bytes())
                    .ok_or("packed upload byte overflow")?,
            )
        } else {
            geometry_upload_bytes
        };
        let mut images = Vec::<(ImageDesc, usize)>::new();
        let mut add_image = |desc: ImageDesc, count: usize| {
            if let Some((_, total)) = images.iter_mut().find(|(old, _)| *old == desc) {
                *total += count
            } else {
                images.push((desc, count))
            }
        };
        for image in &scene.images {
            add_image(image_desc(image), 1);
        }
        for image in environment.images() {
            add_image(environment_desc(image), 1);
        }
        let mut check_images = 0;
        if options.resource_checks {
            let (_, image) = super::lifecycle::selection(scene)?;
            let desc = image_desc(&scene.images[image]);
            add_image(desc, 1);
            check_images += 1;
            // Whole-image transfer APIs intentionally expose one mip. Probe
            // each selected sampled mip into a separately prepared target.
            for mip in 0..desc.mip_levels {
                add_image(
                    ImageDesc::rgba8((desc.width >> mip).max(1), (desc.height >> mip).max(1)),
                    1,
                );
                check_images += 1;
            }
        }
        let capture_bloom = options.bloom != Bloom::Off;
        let mut frame_instances = 0;
        for (desc, count) in Self::frame_images(options, &prepared) {
            add_image(desc, count);
            frame_instances += count;
        }
        let requirements = prepared.reservation.graph_requirements();
        let resources = buffer_count
            + scene.images.len()
            + 3 // IBL
            + shadow::CASCADE_COUNT // cached imports
            + requirements.resources;
        let physical_resources = buffer_count
            + scene.images.len()
            + 3
            + frame_instances
            + frame_buffers
            + check_images
            + usize::from(options.resource_checks);
        let accesses = requirements.accesses + 256;
        let mut preparation = Preparation::new(
            NonZeroUsize::new(options.inflight).ok_or("Bistro needs a frame slot")?,
        );
        preparation.add_graph_requirements(GraphRequirements {
            resources,
            passes: requirements.passes + 20,
            accesses,
            bindings: requirements.bindings + 16,
            attachments: requirements.attachments + 4,
            name_bytes: resources * 32 + 2048 + requirements.name_bytes,
            ..requirements
        })?;
        let graph = preparation.graph_capacity(GraphCapacity {
            resources: 0,
            passes: 0,
            accesses: 0,
            draws: 0,
            raster_constant_words: Some(0),
            bindings: 0,
            attachments: 0,
            dependencies: 32,
            initial_ranges: resources * 2,
            name_bytes: 0,
            upload_bytes: 0,
            edges: accesses * 4,
            segments: accesses * 2,
            readers: accesses * 4,
        })?;
        #[cfg(feature = "vulkan")]
        let vulkan_shader_bindings = {
            // Vulkan budgets each Compute dispatch in addition to Raster draw bindings.
            let dispatches = requirements.compute_dispatches;
            // Asset replacement, diagnostic captures and tile initialization use
            // separately submitted Graphs sharing the runtime limits.
            u32::try_from((graph.draws + dispatches).max(1))?
        };
        // Reserve the conservative D3D12 row/placement-aligned copy footprint,
        // not merely packed asset bytes. Vulkan can use the same bounded budget.
        let upload_bytes = scene
            .images
            .iter()
            .map(|image| {
                let mut end = 0usize;
                for mip in 0..image.mips.len() {
                    let width = (image.width as usize >> mip).max(1);
                    let height = (image.height as usize >> mip).max(1);
                    end = end.next_multiple_of(512) + (width * 4).next_multiple_of(256) * height;
                }
                end.max(image.byte_length())
            })
            .max()
            .unwrap_or(0)
            .max(geometry_upload_bytes)
            .max(
                environment
                    .images()
                    .into_iter()
                    .map(|image| {
                        let mut end = 0usize;
                        for mip in 0..image.mips.len() {
                            let width = (image.width as usize >> mip).max(1);
                            let height = (image.height as usize >> mip).max(1);
                            end = end.next_multiple_of(512)
                                + (width * 8).next_multiple_of(256) * height;
                        }
                        end.max(image.byte_length())
                    })
                    .max()
                    .unwrap_or(0),
            )
            .max(if tiled_lights == 0 {
                max_buffer_bytes as usize
            } else {
                // The tile-list class is large, but its contents are written by
                // compute. Geometry streams above still size cold CPU uploads.
                pbr_forward_plus::LIGHT_BUFFER_BYTES as usize
            })
            // Direct frames also share the staging capacity across uploads.
            .max(graph.upload_bytes)
            .max(packed.as_ref().filter(|_| indirect).map_or(0, |layout| {
                let frame_bytes = layout.template_upload_bytes() + graph.upload_bytes;
                layout.aabb_bytes().max(frame_bytes)
            }))
            .max(4096)
            .checked_add(4096)
            .ok_or("upload byte overflow")?;
        let hdr_captures = if capture_bloom { 2 } else { 1 };
        let mut frame_readback_bytes = ((u64::from(options.width) * 8).next_multiple_of(256)
            * hdr_captures
            + (u64::from(options.width) * 4).next_multiple_of(256) * 2)
            * u64::from(options.height)
            + 4096;
        if options.capture_shadows {
            let resolution = u64::from(shadow::Parameters::default().resolution);
            frame_readback_bytes += (resolution * 4).next_multiple_of(256) * resolution * 4 + 4096;
        }
        let readback_bytes = if options.resource_checks {
            let (_, image) = super::lifecycle::selection(scene)?;
            let image = &scene.images[image];
            frame_readback_bytes
                .checked_mul(options.inflight as u64)
                .ok_or("overlapping readback budget overflow")?
                .max(
                    (u64::from(image.width) * 4).next_multiple_of(256) * u64::from(image.height)
                        + 4096,
                )
        } else {
            frame_readback_bytes
        };
        Ok(Self {
            graph,
            prepared: Some(prepared),
            #[cfg(feature = "vulkan")]
            vulkan_shader_bindings,
            buffers,
            images,
            buffer_count: buffer_count + frame_buffers,
            physical_resources,
            max_buffer_bytes,
            upload_bytes,
            readback_bytes,
            ranges: resources * (options.inflight + 3) + 32,
            pipeline: options.pipeline,
            defaults,
            fixture: options.fixture,
            packed,
            indirect,
        })
    }
    pub fn upload_pool(&self) -> Result<GraphPool, Error> {
        Ok(GraphPool::new(
            GraphCapacity {
                resources: 1,
                passes: 1,
                accesses: 32,
                draws: 0,
                raster_constant_words: Some(0),
                bindings: 0,
                attachments: 0,
                dependencies: 0,
                initial_ranges: 32,
                name_bytes: 256,
                upload_bytes: self.upload_bytes,
                edges: 64,
                segments: 64,
                readers: 64,
            },
            1,
        )?)
    }
}

pub(super) struct Resident {
    pub vertices: Vec<ImportBuffer>,
    pub indices: Vec<ImportBuffer>,
    pub materials: Vec<ResidentMaterialUniform>,
    surfaces: standard_forward::SurfaceTable,
    pub images: Vec<ImportImage>,
    pub environment: [ImportImage; 3],
    pub environment_mips: [u32; 3],
    pub environment_parameters: [f32; 3],
    scene_bounds: shadow::Bounds,
    standard_bounds: pbr::Bounds,
    primitive_bounds: Vec<shadow::Bounds>,
    standard_primitive_bounds: Vec<pbr::Bounds>,
    pub shadow_parameters: shadow::Parameters,
    pub camera: ImportBuffer,
    local_lights: Option<lighting::Preset>,
    defaults: Option<pbr::DefaultResources>,
    packed: Option<indirect::Resident>,
    // Only small general Uniform uploads retain byte provenance. Geometry uses
    // typed immutable bounds and releases all upload CPU streams before frames.
    pub upload_owners: Vec<GraphPool>,
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn upload_geometry(
    backend: &mut Runtime,
    pool: &mut GraphPool,
    stride: u32,
    vertices: &[u8],
    indices: &[u32],
) -> Result<(ImportBuffer, ImportBuffer), Error> {
    let geometry = renderer::geometry::GeometryData::new(stride, vertices, indices)?;
    let mut graph = pool.begin()?;
    let vertex = graph.buffer(
        "immutable vertex stream",
        BufferDesc {
            size: geometry.vertex_byte_len(),
        },
    )?;
    let index = graph.buffer(
        "immutable triangle stream",
        BufferDesc {
            size: geometry.index_byte_len(),
        },
    )?;
    geometry.upload_vertices(&mut graph, "publish immutable vertices", vertex)?;
    geometry.upload_indices(&mut graph, "publish bounded indices", index)?;
    graph.export_buffer(vertex)?;
    graph.export_buffer(index)?;
    let plan = graph.compile()?;
    let pending = backend.submit(&plan)?;
    let imports = (
        pending.export_buffer(vertex)?,
        pending.export_buffer(index)?,
    );
    drop(super::backend::wait_for(backend, pending.id())?);
    drop((pending, plan, geometry));
    // Native canonical provenance carries scalar index bounds, not CPU bytes.
    // Release this slot's final asset Arc at the explicit load boundary.
    pool.release_idle();
    Ok(imports)
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn upload_buffer(
    backend: &mut Runtime,
    owners: &mut Vec<GraphPool>,
    bytes: &[u8],
) -> Result<ImportBuffer, Error> {
    upload_asset(
        backend,
        owners,
        bytes.len(),
        |graph| {
            let buffer = graph.buffer(
                "immutable asset buffer",
                BufferDesc {
                    size: bytes.len() as u64,
                },
            )?;
            graph.upload_buffer("asset import boundary", buffer, 0, bytes)?;
            graph.export_buffer(buffer)?;
            Ok(buffer)
        },
        |buffer, submission| submission.export_buffer(buffer),
    )
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn upload_material(
    backend: &mut Runtime,
    owners: &mut Vec<GraphPool>,
    bytes: &[u8; program::MATERIAL_BYTES],
) -> Result<ResidentMaterialUniform, Error> {
    upload_asset(
        backend,
        owners,
        bytes.len(),
        |graph| {
            let material = MaterialUniform::upload_encoded(graph, "material factors", bytes)?;
            material.export(graph)?;
            Ok(material)
        },
        MaterialUniform::accept,
    )
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn upload_asset<D, T>(
    backend: &mut Runtime,
    owners: &mut Vec<GraphPool>,
    byte_count: usize,
    declare: impl FnOnce(&mut Graph) -> Result<D, GraphError>,
    accept: impl FnOnce(D, &FrameSubmission) -> Result<T, ExecutionError>,
) -> Result<T, Error> {
    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 1,
            passes: 1,
            accesses: 1,
            draws: 0,
            raster_constant_words: Some(0),
            bindings: 0,
            attachments: 0,
            dependencies: 0,
            initial_ranges: 1,
            name_bytes: 64,
            upload_bytes: byte_count,
            edges: 2,
            segments: 2,
            readers: 2,
        },
        1,
    )?;
    let mut graph = pool.begin()?;
    let resource = declare(&mut graph)?;
    let plan = graph.compile()?;
    let pending = backend.submit(&plan)?;
    let imported = accept(resource, &pending)?;
    drop(super::backend::wait_for(backend, pending.id())?);
    drop((pending, plan));
    owners.push(pool);
    Ok(imported)
}

impl Resident {
    pub fn instance_placements(
        &self,
        scene: &Scene,
        options: &Options,
        camera: Camera,
        near_far: [f32; 2],
    ) -> Result<Option<InstancePlacements>, Error> {
        options
            .instances
            .enabled()
            .then(|| -> Result<_, Error> {
                let fixture = instancing::Placements::new(
                    scene,
                    &self.primitive_bounds,
                    camera,
                    options.width as f32 / options.height as f32,
                    near_far[0],
                    near_far[1],
                )?;
                let mut transforms = [pbr::Transform::IDENTITY; instancing::COUNT];
                let source = self.primitive_bounds[fixture.primitive];
                let mut caster_bounds = self.scene_bounds;
                for (index, transform) in transforms.iter_mut().enumerate() {
                    let rows = fixture.affine_rows(index);
                    *transform = pbr::Transform::new(rows)?;
                    caster_bounds = caster_bounds.union(shadow::Bounds::new(
                        std::array::from_fn(|axis| {
                            rows[axis][axis] * source.min[axis] + rows[axis][3]
                        }),
                        std::array::from_fn(|axis| {
                            rows[axis][axis] * source.max[axis] + rows[axis][3]
                        }),
                    )?);
                }
                Ok(InstancePlacements {
                    fixture,
                    transforms,
                    shadow_bounds: pbr::Bounds::new(caster_bounds.min, caster_bounds.max)?,
                })
            })
            .transpose()
    }

    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    pub fn reprepare(
        mut self,
        backend: &mut Runtime,
        prepare: impl FnOnce(&mut Runtime) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        let take_keys = |snapshots: &mut Vec<ImportBuffer>| {
            std::mem::take(snapshots)
                .into_iter()
                .map(|snapshot| snapshot.key())
                .collect::<Vec<_>>()
        };
        let vertices = take_keys(&mut self.vertices);
        let indices = take_keys(&mut self.indices);
        self.surfaces.release_snapshots();
        for material in &mut self.materials {
            material.release_snapshot();
        }
        let camera = self.camera.key();
        let packed = self.packed.take().map(indirect::Resident::release);
        drop(self.camera);
        prepare(backend)?;
        let refresh = |keys: Vec<_>| -> Result<Vec<ImportBuffer>, Error> {
            keys.into_iter()
                .map(|key| Ok(backend.refresh_buffer(key)?))
                .collect()
        };
        self.vertices = refresh(vertices)?;
        self.indices = refresh(indices)?;
        for material in &mut self.materials {
            material.refresh_snapshot(backend)?;
        }
        self.camera = backend.refresh_buffer(camera)?;
        self.refresh_surfaces()?;
        self.packed = packed.map(|packed| packed.refresh(backend)).transpose()?;
        Ok(self)
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn upload(
        backend: &mut Runtime,
        scene: &Scene,
        environment: &ibl::Environment,
        budget: &Budget,
        camera: &[u8; 128],
    ) -> Result<Self, Error> {
        let mut pool = budget.upload_pool()?;
        let mut geometry_pool = GraphPool::new(
            GraphCapacity {
                resources: 2,
                passes: 2,
                accesses: 2,
                draws: 0,
                raster_constant_words: Some(0),
                bindings: 0,
                attachments: 0,
                dependencies: 0,
                initial_ranges: 2,
                name_bytes: 128,
                upload_bytes: 0,
                edges: 4,
                segments: 4,
                readers: 4,
            },
            1,
        )?;
        let mut upload_owners = Vec::with_capacity(scene.materials.len() + 1);
        let mut vertices = Vec::with_capacity(scene.primitives.len());
        let mut indices = Vec::with_capacity(scene.primitives.len());
        let mut primitive_bounds = Vec::with_capacity(scene.primitives.len());
        for (index, primitive) in scene.primitives.iter().enumerate() {
            let primitive_vertices = &scene.vertices[primitive.vertices.clone()];
            let mut minimum = [f32::INFINITY; 3];
            let mut maximum = [f32::NEG_INFINITY; 3];
            for vertex in primitive_vertices
                .as_chunks::<{ program::VERTEX_BYTES }>()
                .0
            {
                for axis in 0..3 {
                    let value =
                        f32::from_le_bytes(vertex[axis * 4..axis * 4 + 4].try_into().unwrap());
                    minimum[axis] = minimum[axis].min(value);
                    maximum[axis] = maximum[axis].max(value);
                }
            }
            primitive_bounds.push(shadow::Bounds::new(minimum, maximum)?);
            let encoded = &scene.indices[primitive.indices.clone()];
            let decoded = encoded
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u32::from_le_bytes(*word))
                .collect::<Vec<_>>();
            let (vertex, index_buffer) = upload_geometry(
                backend,
                &mut geometry_pool,
                program::VERTEX_BYTES as u32,
                primitive_vertices,
                &decoded,
            )?;
            vertices.push(vertex);
            indices.push(index_buffer);
            if (index + 1).is_multiple_of(256) {
                eprintln!(
                    "Bistro geometry upload {}/{}",
                    index + 1,
                    scene.primitives.len()
                );
            }
        }
        let mut materials = Vec::with_capacity(scene.materials.len());
        let packed = budget
            .packed
            .as_ref()
            .map(|layout| {
                indirect::Resident::upload(
                    backend,
                    &mut geometry_pool,
                    &mut upload_owners,
                    scene,
                    layout,
                    &primitive_bounds,
                    budget.indirect,
                )
            })
            .transpose()?;
        for material in &scene.materials {
            materials.push(upload_material(
                backend,
                &mut upload_owners,
                &material.uniform,
            )?);
        }
        let mut initial_uniform = [0; program::CAMERA_BYTES];
        initial_uniform[..128].copy_from_slice(camera);
        let camera = upload_buffer(backend, &mut upload_owners, &initial_uniform)?;
        let local_lights = budget
            .pipeline
            .uses_local_lights()
            .then(|| lighting::preset(budget.fixture));
        let defaults = if let Some(defaults) = budget.defaults {
            let (resources, startup) = defaults.initialize(backend)?;
            drop(super::backend::wait_for(backend, startup)?);
            Some(resources)
        } else {
            None
        };
        let mut images = Vec::with_capacity(scene.images.len());
        for (index, image) in scene.images.iter().enumerate() {
            // This is the immutable publication boundary. Read/hash one chain,
            // upload all its subresources, then release CPU pixels before ROI.
            let pixels = image.load_bytes()?;
            let (frame, texture) =
                backend.upload_texture(&mut pool, "material texture", image_desc(image), &pixels)?;
            drop(super::backend::wait_for(backend, frame)?);
            images.push(texture.into_registered());
            if (index + 1).is_multiple_of(32) {
                eprintln!("Bistro image upload {}/{}", index + 1, scene.images.len());
            }
        }
        let mut upload_environment = |image: &ibl::Image| -> Result<ImportImage, Error> {
            let (frame, texture) = backend.upload_texture(
                &mut pool,
                "environment texture",
                environment_desc(image),
                &image.bytes,
            )?;
            drop(super::backend::wait_for(backend, frame)?);
            Ok(texture.into_registered())
        };
        let environment_images = [
            upload_environment(&environment.diffuse)?,
            upload_environment(&environment.specular)?,
            upload_environment(&environment.brdf)?,
        ];
        let scene_bounds = primitive_bounds
            .iter()
            .copied()
            .reduce(shadow::Bounds::union)
            // An empty scene is a valid clear-only control, with no casters.
            .unwrap_or(shadow::Bounds {
                min: [-1.0; 3],
                max: [1.0; 3],
            });
        let standard_primitive_bounds = primitive_bounds
            .iter()
            .map(|bounds| pbr::Bounds::new(bounds.min, bounds.max))
            .collect::<Result<Vec<_>, _>>()?;
        let surfaces = standard_forward::SurfaceTable::new(scene, &materials, &images)?;
        Ok(Self {
            vertices,
            indices,
            materials,
            surfaces,
            images,
            environment: environment_images,
            environment_mips: [
                environment.diffuse.mips.len() as u32,
                environment.specular.mips.len() as u32,
                environment.brdf.mips.len() as u32,
            ],
            environment_parameters: [
                environment.strength,
                environment.yaw_degrees.to_radians(),
                environment.max_prefilter_lod as f32,
            ],
            scene_bounds,
            standard_bounds: pbr::Bounds::new(scene_bounds.min, scene_bounds.max)?,
            primitive_bounds,
            standard_primitive_bounds,
            shadow_parameters: shadow::Parameters::default(),
            camera,
            local_lights,
            defaults,
            packed,
            upload_owners,
        })
    }
}

pub(super) struct InstancePlacements {
    fixture: instancing::Placements,
    transforms: [pbr::Transform; instancing::COUNT],
    shadow_bounds: pbr::Bounds,
}

impl InstancePlacements {
    fn append_draws(
        &self,
        mode: instancing::Mode,
        geometry: pbr::IndexedDraw,
        out: &mut Vec<pbr::Draw>,
    ) {
        let instance_count = if mode == instancing::Mode::Instanced {
            instancing::COUNT as u32
        } else {
            1
        };
        out.extend(
            (0..mode.draws()).map(|first_transform| pbr::Draw::Instanced {
                geometry,
                first_transform: first_transform as u32,
                instance_count,
            }),
        );
    }
}

pub(super) struct Workspace {
    vertices: Vec<BufferHandle>,
    indices: Vec<BufferHandle>,
    materials: Vec<MaterialUniform>,
    images: Vec<ImageHandle>,
    bindings: Vec<RasterBindings>,
    shadow_bindings: Vec<Option<RasterBindings>>,
    shadow_reuse: Option<ShadowReuse>,
    pub(super) placements: Option<InstancePlacements>,
    pub(super) prepared: PreparedSet,
    opaque: Vec<usize>,
    blended: Vec<usize>,
    casters: Vec<RasterDraw>,
    indirect: Vec<Option<IndexedIndirectArgs>>,
    standard_inputs: Option<standard_forward::Inputs>,
}

fn caster_revision(options: &Options) -> u64 {
    // Geometry, material coverage and the shadow program stay fixed while
    // history is enabled. These controls can change the draw list.
    u64::from(options.alpha_only)
        | (u64::from(options.shadows) << 1)
        | (u64::from(options.instances.enabled()) << 2)
}

#[derive(Default)]
struct ShadowReuse {
    history: ShadowHistory,
    hits: [u64; shadow::CASCADE_COUNT],
    misses: [u64; shadow::CASCADE_COUNT],
}

fn begin_shadow_frame(
    reuse: Option<&ShadowReuse>,
    graph: &mut Graph,
    resolution: u32,
    matrices: &[[[f32; 4]; 4]; shadow::CASCADE_COUNT],
    options: &Options,
) -> Result<Option<ShadowFrame>, GraphError> {
    let revision = caster_revision(options);
    reuse
        .map(|reuse| {
            let keys = matrices.map(|matrix| ShadowKey::new(matrix, revision));
            reuse.history.begin_frame(graph, resolution, keys)
        })
        .transpose()
}

impl Workspace {
    pub fn new(scene: &Scene, prepared: PreparedSet) -> Self {
        let primitives = scene.primitives.len();
        let standard_inputs = prepared
            .main
            .uses_standard()
            .then(|| standard_forward::Inputs::new(scene, prepared.instance_draws));
        Self {
            vertices: Vec::with_capacity(primitives),
            indices: Vec::with_capacity(primitives),
            materials: Vec::with_capacity(scene.materials.len()),
            images: Vec::with_capacity(scene.images.len()),
            bindings: Vec::with_capacity(scene.materials.len()),
            shadow_bindings: vec![None; scene.materials.len()],
            shadow_reuse: None,
            placements: None,
            prepared,
            opaque: Vec::with_capacity(primitives),
            blended: Vec::with_capacity(primitives),
            casters: Vec::with_capacity(primitives),
            indirect: vec![None; scene.materials.len()],
            standard_inputs,
        }
    }

    /// The scene and resident caster inputs must stay fixed until the caller
    /// clears reuse. The resource mutation probes use a separate Workspace.
    pub fn enable_shadow_reuse(&mut self) {
        self.shadow_reuse.get_or_insert_with(ShadowReuse::default);
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept_shadow_updates(
        &mut self,
        pending: &FrameSubmission,
        updates: Option<ShadowUpdates>,
    ) -> Result<(), Error> {
        let (Some(reuse), Some(mut updates)) = (&mut self.shadow_reuse, updates) else {
            return Ok(());
        };
        let reused = updates.reused();
        reuse.history.accept(pending, &mut updates)?;
        for (cascade, hit) in reused.into_iter().enumerate() {
            reuse.hits[cascade] += u64::from(hit);
            reuse.misses[cascade] += u64::from(!hit);
        }
        Ok(())
    }

    pub fn reset_shadow_reuse_counts(&mut self) {
        if let Some(reuse) = &mut self.shadow_reuse {
            reuse.hits.fill(0);
            reuse.misses.fill(0);
        }
    }

    pub fn shadow_reuse_counts(
        &self,
    ) -> Option<([u64; shadow::CASCADE_COUNT], [u64; shadow::CASCADE_COUNT])> {
        self.shadow_reuse
            .as_ref()
            .map(|reuse| (reuse.hits, reuse.misses))
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear_shadow_reuse(&mut self, backend: &mut Runtime) -> Result<(), Error> {
        if let Some(reuse) = &mut self.shadow_reuse {
            reuse.history.clear(backend)?;
        }
        self.shadow_reuse = None;
        Ok(())
    }
}
pub(super) struct Built {
    pub graph: Graph,
    pub output: renderer_pipeline::passes::output::SdrImage,
    pub camera_update: BufferHandle,
    pub captures: Option<Captures>,
    pub bloom_capture: Option<PassHandle>,
    #[cfg(all(windows, feature = "presentation"))]
    pub present_capture: Option<PassHandle>,
    pub shadow_captures: Option<[PassHandle; 4]>,
    pub shadow_updates: Option<ShadowUpdates>,
    pub draws: Option<usize>,
}

impl Resident {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub(super) fn refresh_surfaces(&mut self) -> Result<(), Error> {
        Ok(self.surfaces.refresh(&self.materials, &self.images)?)
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept_camera(
        &mut self,
        pending: &FrameSubmission,
        camera: BufferHandle,
    ) -> Result<(), Error> {
        self.camera = pending.export_buffer(camera)?;
        Ok(())
    }
}

pub(super) struct Captures {
    pub hdr: PassHandle,
    pub sdr: PassHandle,
    pub depth: Option<PassHandle>,
}

fn shadow_casters<'a>(
    scene: &'a Scene,
    options: &'a Options,
    overlaps: impl Fn(usize) -> bool + 'a,
) -> impl Iterator<Item = usize> + 'a {
    (0..scene.primitives.len()).filter(move |&index| {
        let material = &scene.materials[scene.primitives[index].material];
        options.shadows
            && !material.blend
            && (!options.alpha_only || material.alpha_zero)
            && overlaps(index)
    })
}

#[derive(Clone, Copy)]
struct Geometry<'a> {
    vertices: &'a [BufferHandle],
    indices: &'a [BufferHandle],
}

impl Geometry<'_> {
    fn draw(
        self,
        scene: &Scene,
        index: usize,
        bindings: RasterBindings,
        parameters: RasterConstants,
    ) -> RasterDraw {
        RasterDraw {
            bindings: Some(bindings),
            vertices: self.vertices[index],
            indices: self.indices[index],
            first_index: 0,
            index_count: scene.primitives[index].index_count,
            instance_count: 1,
            parameters,
        }
    }
}

#[derive(Clone, Copy)]
struct MaterialBindings<'a> {
    uniform: BufferHandle,
    factors: &'a [MaterialUniform],
    images: &'a [ImageHandle],
}

impl MaterialBindings<'_> {
    fn camera(self) -> Binding {
        buffer_binding(0, self.uniform, program::CAMERA_BYTES as u64)
    }

    fn factors(self, material: usize) -> MaterialUniform {
        self.factors[material]
    }

    fn texture(
        self,
        graph: &Graph,
        scene: &Scene,
        texture: usize,
        binding: u32,
    ) -> Result<Binding, GraphError> {
        let view = graph.sampled_image_view(
            self.images[texture],
            0,
            scene.images[texture].mips.len() as u32,
        )?;
        Ok(image_binding(1, binding, view))
    }
}

fn buffer_binding(binding: u32, buffer: BufferHandle, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size,
        },
    }
}

fn image_binding(group: u32, binding: u32, view: ImageView) -> Binding {
    Binding {
        group,
        binding,
        resource: BindingResource::ImageView(view),
    }
}

fn sampler_binding(group: u32, binding: u32, sampler: SamplerDescriptor) -> Binding {
    Binding {
        group,
        binding,
        resource: BindingResource::Sampler(sampler),
    }
}

struct Shadows {
    views: [ImageView; shadow::CASCADE_COUNT],
    updates: Option<ShadowUpdates>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "borrow disjoint frame storage and the prepared shadow Program"
)]
fn append_shadows(
    graph: &mut Graph,
    scene: &Scene,
    resident: &Resident,
    options: &Options,
    reuse: Option<&ShadowReuse>,
    shadow_bindings: &mut [Option<RasterBindings>],
    casters: &mut Vec<RasterDraw>,
    geometry: Geometry<'_>,
    materials: MaterialBindings<'_>,
    program: &ManagedRasterProgram,
    data: &shadow::ShadowData,
) -> Result<Shadows, Error> {
    let resolution = resident.shadow_parameters.resolution;
    let desc = ImageDesc::depth32(resolution, resolution);
    let mut views = [None; shadow::CASCADE_COUNT];
    let history_frame = begin_shadow_frame(reuse, graph, resolution, &data.matrices, options)?;
    shadow_bindings.fill(None);
    for (cascade, name) in CASCADES.into_iter().enumerate() {
        let matrix = data.matrices[cascade];
        let image = match &history_frame {
            Some(frame) => frame.target(cascade),
            None => graph.image(name, desc)?,
        };
        let view = graph.image_view(image, 0, 0)?;
        views[cascade] = Some(view);
        if history_frame
            .as_ref()
            .is_some_and(|frame| frame.is_reused(cascade))
        {
            continue;
        }
        let clip = shadow::CascadeClip::new(&matrix, resident.scene_bounds);
        let parameters = RasterConstants::new(&[cascade as u32])?;
        casters.clear();
        for index in shadow_casters(scene, options, |index| {
            clip.overlaps(resident.primitive_bounds[index])
        }) {
            let material = scene.primitives[index].material;
            let slot = &mut shadow_bindings[material];
            let bindings = match *slot {
                Some(bindings) => bindings,
                None => {
                    let base_color = scene.materials[material].textures[0];
                    let bindings = materials.factors(material).raster_bindings(
                        graph,
                        program,
                        &[
                            materials.camera(),
                            materials.texture(graph, scene, base_color, 0)?,
                            sampler_binding(1, 4, SamplerDescriptor::LinearRepeat),
                        ],
                    )?;
                    *slot = Some(bindings);
                    bindings
                }
            };
            casters.push(geometry.draw(scene, index, bindings, parameters));
        }
        pbr_forward::add_shadow_pass(graph, cascade, view, |raster| {
            raster.draw(program, casters.iter())
        })?;
    }
    Ok(Shadows {
        views: views.map(Option::unwrap),
        updates: history_frame.map(|frame| frame.export(graph)).transpose()?,
    })
}

#[derive(Clone, Copy)]
struct FrameLights {
    lights: BufferHandle,
    tiles: Option<ImageView>,
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn frame_lights(
    graph: &mut Graph,
    resident: &Resident,
    options: &Options,
    camera: BufferHandle,
) -> Result<Option<FrameLights>, Error> {
    let Some(preset) = &resident.local_lights else {
        return Ok(None);
    };
    let (lights, tiles) = lighting::append_reference(
        graph,
        [options.width, options.height],
        camera,
        preset,
        options.pipeline == PipelineMode::ForwardPlus,
    )?;
    Ok(Some(FrameLights { lights, tiles }))
}

fn import_environment(
    graph: &mut Graph,
    resident: &Resident,
) -> Result<[ImageView; 3], GraphError> {
    let mut environment = [None; 3];
    for (index, name) in ["irradiance", "reflection", "brdf"].into_iter().enumerate() {
        let image = graph.import_image(name, &resident.environment[index])?;
        environment[index] =
            Some(graph.sampled_image_view(image, 0, resident.environment_mips[index])?);
    }
    Ok(environment.map(Option::unwrap))
}

fn lighting_bindings(
    graph: &mut Graph,
    resident: &Resident,
    shadows: [ImageView; shadow::CASCADE_COUNT],
) -> Result<[Binding; 9], GraphError> {
    let [irradiance, reflection, brdf] = import_environment(graph, resident)?;
    Ok([
        image_binding(2, 0, irradiance),
        image_binding(2, 1, reflection),
        image_binding(2, 2, brdf),
        sampler_binding(2, 3, SamplerDescriptor::LinearRepeatUClampV),
        image_binding(3, 0, shadows[0]),
        image_binding(3, 1, shadows[1]),
        image_binding(3, 2, shadows[2]),
        image_binding(3, 3, shadows[3]),
        sampler_binding(3, 4, SamplerDescriptor::NearestClamp),
    ])
}

#[allow(
    clippy::too_many_arguments,
    reason = "compose the existing Bistro shader ABI"
)]
fn scene_bindings(
    graph: &mut Graph,
    scene: &Scene,
    prepared: &recipes::Reference,
    materials: MaterialBindings<'_>,
    lighting: &[Binding; 9],
    local: Option<FrameLights>,
    instance_material: Option<usize>,
    bindings: &mut Vec<RasterBindings>,
) -> Result<Option<RasterBindings>, GraphError> {
    #[cfg(feature = "tracy")]
    let _span = tracy_client::Client::running()
        .map(|client| client.span(tracy_client::span_location!("bistro.build.bindings"), 0));
    let mut instance_bindings = None;
    let mut local_bindings = [None; 2];
    if let Some(local) = local {
        for (slot, binding) in local_bindings
            .iter_mut()
            .zip(pbr_forward_plus::raster_bindings(local.lights, local.tiles))
        {
            *slot = Some(binding);
        }
    }
    bindings.clear();
    for (index, material) in scene.materials.iter().enumerate() {
        let program = if material.blend {
            &prepared.transparent
        } else {
            &prepared.opaque
        };
        let [base, normal, orm, emissive] = material.textures;
        let material_bindings = [
            materials.camera(),
            materials.texture(graph, scene, base, 0)?,
            materials.texture(graph, scene, normal, 1)?,
            materials.texture(graph, scene, orm, 2)?,
            materials.texture(graph, scene, emissive, 3)?,
            sampler_binding(1, 4, SamplerDescriptor::LinearRepeat),
        ];
        let mut all = [material_bindings[0]; MAX_RASTER_BINDINGS];
        let shared = material_bindings.len() + lighting.len();
        all[..material_bindings.len()].copy_from_slice(&material_bindings);
        all[material_bindings.len()..shared].copy_from_slice(lighting);
        for (slot, binding) in all[shared..]
            .iter_mut()
            .zip(local_bindings.into_iter().flatten())
        {
            *slot = binding;
        }
        if instance_material == Some(index)
            && let Some(instances) = &prepared.instances
        {
            instance_bindings = Some(materials.factors(index).raster_bindings(
                graph,
                instances,
                &all[..shared],
            )?);
        }
        bindings.push(materials.factors(index).raster_bindings(
            graph,
            program,
            &all[..program.bindings().len() - 1],
        )?);
    }
    Ok(instance_bindings)
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit per-frame inputs share no additional ownership"
)]
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn build(
    pool: &mut GraphPool,
    workspace: &mut Workspace,
    resident: &Resident,
    scene: &Scene,
    options: &Options,
    camera: Camera,
    camera_bytes: &[u8; 128],
    capture: bool,
) -> Result<Built, Error> {
    if workspace.prepared.select(options).uses_standard() {
        return standard_forward::build(
            pool,
            workspace,
            resident,
            scene,
            options,
            camera,
            camera_bytes,
            capture,
        );
    }
    let read_word =
        |word: usize| f32::from_le_bytes(camera_bytes[word * 4..word * 4 + 4].try_into().unwrap());
    let shadow_data = shadow::calculate(
        camera,
        options.width as f32 / options.height as f32,
        read_word(19),
        read_word(23),
        [read_word(20), read_word(21), read_word(22)],
        resident.scene_bounds,
        resident.shadow_parameters,
    )?;
    #[cfg(feature = "development-validation")]
    let shadow_data = {
        let mut data = shadow_data;
        if options.offset_shadow_receiver {
            data.bias[2] = 16.0;
        }
        data
    };
    let mut frame_uniform = [0u8; program::CAMERA_BYTES];
    frame_uniform[..128].copy_from_slice(camera_bytes);
    for (word, value) in resident
        .environment_parameters
        .into_iter()
        .chain([if options.shadows { 1.0 } else { 0.0 }])
        .enumerate()
    {
        frame_uniform[112 + word * 4..116 + word * 4].copy_from_slice(&value.to_le_bytes());
    }
    frame_uniform[128..].copy_from_slice(&shadow_data.uniform_bytes());

    let mut graph = pool.begin()?;
    let uniform = import_frame(&mut graph, workspace, resident)?;
    import_reference_materials(&mut graph, workspace, resident)?;
    graph.upload_buffer("camera update boundary", uniform, 0, &frame_uniform)?;
    graph.export_buffer(uniform)?;
    let (prepared, bloom) = workspace.prepared.select(options).reference();
    let lighting = frame_lights(&mut graph, resident, options, uniform)?;
    let materials = MaterialBindings {
        uniform,
        factors: &workspace.materials,
        images: &workspace.images,
    };
    let shadow = append_shadows(
        &mut graph,
        scene,
        resident,
        options,
        workspace.shadow_reuse.as_ref(),
        &mut workspace.shadow_bindings,
        &mut workspace.casters,
        Geometry {
            vertices: &workspace.vertices,
            indices: &workspace.indices,
        },
        materials,
        &prepared.shadow,
        &shadow_data,
    )?;
    let lighting_bindings = lighting_bindings(&mut graph, resident, shadow.views)?;
    let instance_material = workspace
        .placements
        .as_ref()
        .map(|placements| scene.primitives[placements.fixture.primitive].material);
    let instance_bindings = scene_bindings(
        &mut graph,
        scene,
        prepared,
        materials,
        &lighting_bindings,
        lighting,
        instance_material,
        &mut workspace.bindings,
    )?;

    let camera_matrix =
        std::array::from_fn(|column| std::array::from_fn(|row| read_word(column * 4 + row)));
    let frustum = Frustum::new(camera_matrix, resident.scene_bounds);
    let include_material = |index| material_visible(options, scene, index);
    let packed = resident
        .packed
        .as_ref()
        .map(|packed| {
            packed.prepare_frame(
                &mut graph,
                uniform,
                &mut workspace.indirect,
                include_material,
            )
        })
        .transpose()?;

    select_draws(
        [&mut workspace.opaque, &mut workspace.blended],
        scene,
        resident,
        options,
        camera,
        |index| frustum.overlaps(resident.primitive_bounds[index]),
    );

    let geometry = Geometry {
        vertices: &workspace.vertices,
        indices: &workspace.indices,
    };
    let draw = |index: usize| {
        let material = scene.primitives[index].material;
        geometry.draw(
            scene,
            index,
            workspace.bindings[material],
            RasterConstants::default(),
        )
    };
    let draws = if capture && !options.windowed {
        match &packed {
            Some(packed) if packed.is_indirect() => None,
            Some(packed) => {
                packed.visible_direct_draws(&frustum, &resident.primitive_bounds, include_material)
            }
            None => Some(workspace.opaque.len()),
        }
        .map(|opaque| {
            opaque
                + workspace.blended.len()
                + usize::from(workspace.placements.is_some()) * options.instances.draws()
        })
    } else {
        None
    };

    let (hdr, depth) = prepared.scene(
        &mut graph,
        recipes::SceneFrame {
            camera: uniform,
            view_projection: camera_matrix,
            lighting: lighting_bindings,
            has_transparency: !workspace.blended.is_empty(),
        },
        |raster| {
            if let Some(packed) = &packed {
                packed.draw(
                    raster,
                    &prepared.opaque,
                    &workspace.bindings,
                    &frustum,
                    &resident.primitive_bounds,
                    include_material,
                )?;
            } else {
                raster.draw(&prepared.opaque, workspace.opaque.iter().copied().map(draw))?;
            }
            if let (Some(placements), Some(program)) = (&workspace.placements, &prepared.instances)
            {
                let prototype = RasterDraw {
                    bindings: instance_bindings,
                    ..draw(placements.fixture.primitive)
                };
                raster.draw(
                    program,
                    placements.fixture.draws(options.instances, prototype),
                )?;
            }
            Ok(())
        },
        |raster| {
            raster.draw(
                &prepared.transparent,
                workspace.blended.iter().copied().map(draw),
            )
        },
    )?;
    let processed = bloom.add_passes(&mut graph, hdr)?;
    let output = prepared.add_output(&mut graph, processed)?;
    finish_frame(
        graph,
        SceneImages {
            hdr,
            depth,
            output,
            bloom: (processed != hdr).then_some(processed),
            shadows: Some(shadow.views.map(ImageView::image)),
        },
        uniform,
        shadow.updates,
        draws,
        options,
        capture,
    )
}

fn import_frame(
    graph: &mut Graph,
    workspace: &mut Workspace,
    resident: &Resident,
) -> Result<BufferHandle, Error> {
    #[cfg(feature = "tracy")]
    let _imports = tracy_client::Client::running()
        .map(|client| client.span(tracy_client::span_location!("bistro.build.imports"), 0));
    workspace.vertices.clear();
    workspace.indices.clear();

    for value in &resident.vertices {
        workspace
            .vertices
            .push(graph.import_buffer("mesh vertices", value)?);
    }
    for value in &resident.indices {
        workspace
            .indices
            .push(graph.import_buffer("mesh indices", value)?);
    }
    Ok(graph.import_buffer("camera", &resident.camera)?)
}
fn import_reference_materials(
    graph: &mut Graph,
    workspace: &mut Workspace,
    resident: &Resident,
) -> Result<(), Error> {
    workspace.materials.clear();
    workspace.images.clear();
    for value in &resident.materials {
        workspace
            .materials
            .push(value.import(graph, "material factors")?);
    }
    for value in &resident.images {
        workspace
            .images
            .push(graph.import_image("material image", value)?);
    }
    Ok(())
}

fn material_visible(options: &Options, scene: &Scene, index: usize) -> bool {
    #[cfg(feature = "development-validation")]
    if options.omit_material == Some(index) {
        return false;
    }
    !options.alpha_only || scene.materials[index].alpha_zero
}

fn select_draws(
    [opaque, blended]: [&mut Vec<usize>; 2],
    scene: &Scene,
    resident: &Resident,
    options: &Options,
    camera: Camera,
    visible: impl Fn(usize) -> bool,
) {
    opaque.clear();
    blended.clear();
    for (index, primitive) in scene.primitives.iter().enumerate() {
        let material = &scene.materials[primitive.material];
        if (resident.packed.is_some() && !material.blend)
            || !material_visible(options, scene, primitive.material)
            || !visible(index)
        {
            continue;
        }
        if material.blend {
            blended.push(index);
        } else {
            opaque.push(index);
        }
    }
    sort_blended(blended, scene, camera);
}

fn sort_blended(blended: &mut [usize], scene: &Scene, camera: Camera) {
    #[cfg(feature = "tracy")]
    let _sort = tracy_client::Client::running()
        .map(|client| client.span(tracy_client::span_location!("bistro.build.blended_sort"), 0));
    // A deterministic far-to-near primitive order. This is ordinary alpha
    // blending, not order-independent transparency or per-triangle sorting.
    blended.sort_unstable_by(|&a, &b| {
        camera
            .distance_squared(scene.primitives[b].center)
            .total_cmp(&camera.distance_squared(scene.primitives[a].center))
            .then(a.cmp(&b))
    });
}

struct SceneImages {
    hdr: ImageHandle,
    depth: Option<ImageHandle>,
    output: renderer_pipeline::passes::output::SdrImage,
    bloom: Option<ImageHandle>,
    shadows: Option<[ImageHandle; shadow::CASCADE_COUNT]>,
}

fn finish_frame(
    mut graph: Graph,
    images: SceneImages,
    camera_update: BufferHandle,
    shadow_updates: Option<ShadowUpdates>,
    draws: Option<usize>,
    options: &Options,
    capture: bool,
) -> Result<Built, Error> {
    let SceneImages {
        hdr,
        depth,
        output,
        bloom,
        shadows,
    } = images;
    #[cfg(feature = "tracy")]
    let _resolve = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("bistro.build.resolve_and_exports"),
            0,
        )
    });
    if let Some(display) = output.display() {
        display.export(&mut graph)?;
    } else {
        // The headless benchmark also tracks when this registration retires.
        graph.export_image(output.raw_image())?;
    }
    #[cfg(all(windows, feature = "presentation"))]
    let present_capture = if capture && options.windowed {
        Some(graph.readback_image("capture linear presentation source", output.raw_image())?)
    } else {
        None
    };
    let capture = capture && !options.windowed;
    let captures = if capture {
        Some(Captures {
            hdr: graph.readback_image("capture linear HDR", hdr)?,
            sdr: output
                .capture()
                .ok_or("headless Bistro requires sRGB capture output")?
                .readback(&mut graph, "capture sRGB")?,
            depth: depth
                .map(|depth| graph.readback_image("capture depth", depth))
                .transpose()?,
        })
    } else {
        None
    };
    let bloom_capture = if capture {
        bloom
            .map(|image| graph.readback_image("capture post-Bloom HDR", image))
            .transpose()?
    } else {
        None
    };
    let shadow_captures = if capture && options.capture_shadows {
        let mut captures = [None; shadow::CASCADE_COUNT];
        let names = [
            "capture cascade 0",
            "capture cascade 1",
            "capture cascade 2",
            "capture cascade 3",
        ];
        let resolution = shadow::Parameters::default().resolution;
        for (cascade, capture) in captures.iter_mut().enumerate() {
            let image = if let Some(shadows) = shadows {
                shadows[cascade]
            } else {
                let image = graph.image(
                    "diagnostic disabled cascade",
                    ImageDesc::depth32(resolution, resolution),
                )?;
                let view = graph.image_view(image, 0, 0)?;
                pbr_forward::add_shadow_pass(&mut graph, cascade, view, |_| Ok(()))?;
                image
            };
            *capture = Some(graph.readback_image(names[cascade], image)?);
        }
        Some(captures.map(Option::unwrap))
    } else {
        None
    };
    Ok(Built {
        graph,
        output,
        camera_update,
        captures,
        bloom_capture,
        #[cfg(all(windows, feature = "presentation"))]
        present_capture,
        shadow_captures,
        shadow_updates,
        draws,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::graph::SampleCount;

    #[test]
    fn shadow_caster_policy_changes_the_revision() {
        let mut options = tiny_output_options();
        let original = caster_revision(&options);
        options.alpha_only = !options.alpha_only;
        assert_ne!(original, caster_revision(&options));
        options.alpha_only = !options.alpha_only;
        options.shadows = !options.shadows;
        assert_ne!(original, caster_revision(&options));
    }

    #[test]
    fn in_flight_budget_replicates_targets_and_keeps_assets_global() {
        let scene = Scene::fixture(false);
        let mut options = Options::parse(
            ["--fixture", "--width", "101", "--height", "73"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        options.inflight = 1;
        let environment = ibl::Environment::fixture();
        let one = Budget::new(&scene, &environment, &options, [0.0; 3]).unwrap();
        options.inflight = 4;
        let four = Budget::new(&scene, &environment, &options, [0.0; 3]).unwrap();
        assert_eq!(one.buffers, four.buffers);
        assert_eq!(four.physical_resources - one.physical_resources, 24);
        assert_eq!(four.graph.resources, one.graph.resources);
        assert!(
            one.readback_bytes > 101 * 73 * 16,
            "odd output rows need padding beyond packed texels"
        );
        options.sample_count = SampleCount::X4;
        let multisampled = Budget::new(&scene, &environment, &options, [0.0; 3]).unwrap();
        assert_eq!(multisampled.physical_resources - four.physical_resources, 4);
        for format in [ImageFormat::Rgba16Float, ImageFormat::Depth32Float] {
            assert!(multisampled.images.iter().any(|&(desc, count)| {
                desc.format == format && desc.sample_count == SampleCount::X4 && count == 4
            }));
        }
        options.sample_count = SampleCount::X1;
        options.pipeline = PipelineMode::Deferred;
        let deferred = Budget::new(&scene, &environment, &options, [0.0; 3]).unwrap();
        assert_eq!(deferred.physical_resources - four.physical_resources, 16);
        assert_eq!(deferred.graph.resources - four.graph.resources, 4);
        assert_eq!(deferred.graph.upload_bytes - four.graph.upload_bytes, 64);
    }

    fn large_geometry_scene() -> Scene {
        let mut scene = Scene::fixture(false);
        let vertex = scene.vertices[..48].to_vec();
        scene.vertices = vertex.repeat(1365);
        scene.indices = (0u32..4095)
            .flat_map(|index| (index % 1365).to_le_bytes())
            .collect();
        scene.primitives.truncate(1);
        scene.primitives[0].vertices = 0..scene.vertices.len();
        scene.primitives[0].indices = 0..scene.indices.len();
        scene.primitives[0].index_count = 4095;
        scene
    }

    fn tiny_output_options() -> Options {
        Options::parse(
            [
                "--fixture",
                "--width",
                "1",
                "--height",
                "1",
                "--inflight",
                "1",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap()
    }

    #[test]
    fn upload_budget_covers_both_geometry_streams_in_the_same_graph() {
        let scene = large_geometry_scene();
        let indices = scene
            .indices
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect::<Vec<_>>();
        renderer::geometry::GeometryData::new(48, &scene.vertices, &indices).unwrap();
        let pair_bytes = scene.vertices.len() + scene.indices.len();
        assert!(
            pair_bytes > scene.vertices.len() + 4096,
            "the second stream exceeds ordinary alignment slack"
        );
        assert!(
            Budget::new(
                &scene,
                &ibl::Environment::fixture(),
                &tiny_output_options(),
                [0.0; 3]
            )
            .unwrap()
            .upload_bytes
                >= pair_bytes
        );
    }

    #[test]
    fn upload_budget_covers_direct_forward_plus_with_model_transforms() {
        let mut options = tiny_output_options();
        options.width = 129;
        options.height = 97;
        options.pipeline = PipelineMode::ForwardPlus;
        options.sample_count = SampleCount::X4;
        options.instances = instancing::Mode::Individual;
        let budget = Budget::new(
            &Scene::fixture(false),
            &ibl::Environment::fixture(),
            &options,
            [0.0; 3],
        )
        .unwrap();

        assert!(budget.upload_bytes >= budget.graph.upload_bytes);
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires real D3D12 GPU; verifies prepared paired-stream upload capacity"]
    fn d3d12_large_geometry_pair_fits_prepared_upload_capacity() {
        let scene = large_geometry_scene();
        let mut options = tiny_output_options();
        options.api = "d3d12".into();
        let environment = ibl::Environment::fixture();
        let budget = Budget::new(&scene, &environment, &options, [0.0; 3]).unwrap();
        let mut backend = super::super::backend::create(&options, &budget).unwrap();
        let resident =
            Resident::upload(&mut backend, &scene, &environment, &budget, &[0; 128]).unwrap();
        drop(resident);
        assert_eq!(
            super::super::backend::close(backend).unwrap()["clean"],
            true
        );
    }
}
