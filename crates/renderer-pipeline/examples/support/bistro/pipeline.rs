//! Public Graph composition and explicit working-set budgets for this consumer.

use super::{
    Error, Options, PipelineMode, SubmissionMode,
    asset::Scene,
    bloom::Bloom,
    camera::{Camera, Frustum},
    forward_lighting, ibl, instancing,
    program::{self, forward_plus},
    recipes, shadow,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::runtime::{FrameSubmission, Runtime};
use renderer::{
    ImageFormat,
    capacity::{ResourceClass, WorkingSet},
    graph::{
        BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity, GraphPool, ImageDesc,
        ImageHandle, ImportBuffer, ImportImage, PassHandle, RasterConstants, RasterDraw,
        SampleCount,
    },
};
use renderer_pipeline::{
    DrawCandidate, FrameImageSource, FrameIndirectGroup, FrameIndirectList, FrameInputs,
    FrameMaterial, FrameResource, FrameSampledView, FrameView, ImageSource, MaterialBinding,
};
use std::sync::Arc;

pub(super) mod indirect;

const CASCADES: [&str; shadow::CASCADE_COUNT] =
    ["cascade_0", "cascade_1", "cascade_2", "cascade_3"];
const SHADOW_VIEWS: [&str; shadow::CASCADE_COUNT] = ["sun_0", "sun_1", "sun_2", "sun_3"];

pub(super) struct PreparedSet {
    main: recipes::Prepared,
    aa_off: Option<recipes::Prepared>,
    bloom_bypass: Option<recipes::Prepared>,
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
        Ok(Self {
            main,
            aa_off,
            bloom_bypass,
        })
    }

    fn variants(&self) -> impl Iterator<Item = &recipes::Prepared> {
        std::iter::once(&self.main)
            .chain(self.aa_off.as_ref())
            .chain(self.bloom_bypass.as_ref())
    }

    pub fn working_sets(&self) -> impl Iterator<Item = WorkingSet<'_>> {
        self.variants().map(recipes::Prepared::working_set)
    }

    pub fn sha256(&self, options: &Options) -> &str {
        if options.bloom == Bloom::Off
            && let Some(prepared) = &self.bloom_bypass
        {
            return prepared.sha256();
        }
        if !options.aa
            && let Some(prepared) = &self.aa_off
        {
            return prepared.sha256();
        }
        self.main.sha256()
    }

    fn select_mut(&mut self, options: &Options) -> &mut recipes::Prepared {
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

fn reserve_max<T: PartialEq>(classes: &mut Vec<(T, usize)>, desc: T, count: usize) {
    if let Some((_, available)) = classes.iter_mut().find(|(item, _)| *item == desc) {
        *available = (*available).max(count);
    } else {
        classes.push((desc, count));
    }
}

impl Budget {
    fn frame_buffers(options: &Options, prepared: &PreparedSet) -> Vec<(u64, usize)> {
        let mut classes = Vec::new();
        for set in prepared.working_sets() {
            for &class in set.resources {
                if let ResourceClass::Buffer { desc, count } = class {
                    reserve_max(&mut classes, desc.size, count);
                }
            }
        }
        let bytes = match options.pipeline {
            PipelineMode::Deferred => Some(program::deferred::DEFERRED_RECONSTRUCTION_BYTES as u64),
            PipelineMode::ForwardPlus | PipelineMode::ForwardReference => {
                Some(forward_plus::LIGHT_BUFFER_BYTES)
            }
            PipelineMode::Forward => None,
        };
        if let Some(size) = bytes {
            add_instances(&mut classes, size, options.inflight);
        }
        classes
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn resized_buffers(&self, options: &Options, prepared: &PreparedSet) -> Vec<(u64, usize)> {
        Self::frame_buffers(options, prepared)
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
        let mut classes = Vec::new();
        // Supplemental capture profiles run after the measured frames drain.
        for set in prepared.working_sets() {
            for &class in set.resources {
                if let ResourceClass::Image { desc, count } = class {
                    reserve_max(&mut classes, desc, count);
                }
            }
        }
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
        let deferred = usize::from(options.pipeline == PipelineMode::Deferred);
        let local_lights = usize::from(options.pipeline.uses_local_lights());
        let tiled_lights = usize::from(options.pipeline == PipelineMode::ForwardPlus);
        let packed = if options.submission == SubmissionMode::Direct {
            None
        } else {
            indirect::Layout::new(scene)?.map(Arc::new)
        };
        let indirect = options.submission == SubmissionMode::Indirect;
        let prepared = PreparedSet::new(options, scene, packed.as_deref(), background)?;
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
        for (size, count) in Self::frame_buffers(options, &prepared) {
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
        if tiled_lights != 0 {
            // The one shared list is fully initialized by an explicit
            // preparation dispatch. In-flight frames share one ordered queue.
            add(forward_plus::TILE_BUFFER_BYTES);
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
        let requirements = |field: fn(renderer_pipeline::PipelineRequirements) -> usize| {
            prepared
                .variants()
                .map(|recipe| field(recipe.requirements()))
                .max()
                .unwrap_or(0)
        };
        let resources = buffer_count
            + scene.images.len()
            + 3 // IBL
            + shadow::CASCADE_COUNT // cached imports
            + local_lights
            + tiled_lights
            + deferred
            + requirements(|graph| graph.resources);
        let physical_resources = buffer_count
            + scene.images.len()
            + 3
            + frame_instances
            + frame_buffers
            + tiled_lights
            + check_images
            + usize::from(options.resource_checks);
        let accesses = requirements(|graph| graph.accesses) + 256;
        let graph = GraphCapacity {
            resources,
            passes: requirements(|graph| graph.passes) + 20,
            accesses,
            draws: requirements(|graph| graph.draws),
            raster_constant_words: Some(requirements(|graph| graph.raster_constant_words)),
            bindings: requirements(|graph| graph.bindings) + 16,
            attachments: requirements(|graph| graph.attachments) + 4,
            dependencies: 32,
            initial_ranges: resources * 2,
            name_bytes: resources * 32 + 2048 + requirements(|graph| graph.name_bytes),
            upload_bytes: program::CAMERA_BYTES
                + deferred * program::deferred::DEFERRED_RECONSTRUCTION_BYTES
                + local_lights * forward_plus::LIGHT_BUFFER_BYTES as usize,
            edges: accesses * 4,
            segments: accesses * 2,
            readers: accesses * 4,
        };
        #[cfg(feature = "vulkan")]
        let vulkan_shader_bindings = {
            // Vulkan budgets each Compute dispatch in addition to Raster draw bindings.
            let dispatches = requirements(|graph| graph.compute_dispatches);
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
                forward_plus::LIGHT_BUFFER_BYTES as usize
            })
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
            buffer_count: buffer_count + frame_buffers + tiled_lights,
            physical_resources,
            max_buffer_bytes,
            upload_bytes,
            readback_bytes,
            ranges: resources * (options.inflight + 3) + 32,
            pipeline: options.pipeline,
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
    pub materials: Vec<ImportBuffer>,
    pub images: Vec<ImportImage>,
    pub environment: [ImportImage; 3],
    pub environment_mips: [u32; 3],
    pub environment_parameters: [f32; 3],
    scene_bounds: shadow::Bounds,
    primitive_bounds: Vec<shadow::Bounds>,
    pub shadow_parameters: shadow::Parameters,
    pub camera: ImportBuffer,
    local_lights: Option<forward_lighting::LightBytes>,
    tiles: Option<ImportBuffer>,
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
            upload_bytes: bytes.len(),
            edges: 2,
            segments: 2,
            readers: 2,
        },
        1,
    )?;
    let mut graph = pool.begin()?;
    let buffer = graph.buffer(
        "immutable asset buffer",
        BufferDesc {
            size: bytes.len() as u64,
        },
    )?;
    graph.upload_buffer("asset import boundary", buffer, 0, bytes)?;
    graph.export_buffer(buffer)?;
    let plan = graph.compile()?;
    let pending = backend.submit(&plan)?;
    let imported = pending.export_buffer(buffer)?;
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
    ) -> Result<Option<instancing::Placements>, Error> {
        options
            .instances
            .enabled()
            .then(|| {
                instancing::Placements::new(
                    scene,
                    &self.primitive_bounds,
                    camera,
                    options.width as f32 / options.height as f32,
                    near_far[0],
                    near_far[1],
                )
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
        let materials = take_keys(&mut self.materials);
        let camera = self.camera.key();
        let tiles = self.tiles.take().map(|tiles| tiles.key());
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
        self.materials = refresh(materials)?;
        self.camera = backend.refresh_buffer(camera)?;
        self.tiles = tiles.map(|key| backend.refresh_buffer(key)).transpose()?;
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
            materials.push(upload_buffer(
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
            .then(|| forward_lighting::preset(budget.fixture))
            .transpose()?;
        let tiles = (budget.pipeline == PipelineMode::ForwardPlus)
            .then(|| forward_lighting::initialize_tiles(backend, &camera))
            .transpose()?;
        let mut images = Vec::with_capacity(scene.images.len());
        for (index, image) in scene.images.iter().enumerate() {
            // This is the immutable publication boundary. Read/hash one chain,
            // upload all its subresources, then release CPU pixels before ROI.
            let pixels = image.load_bytes()?;
            let mut graph = pool.begin()?;
            let target = graph.image("immutable texture", image_desc(image))?;
            graph.upload_image("texture import boundary", target, &pixels)?;
            graph.export_image(target)?;
            let plan = graph.compile()?;
            let pending = backend.submit(&plan)?;
            images.push(pending.export_image(target)?);
            drop(super::backend::wait_for(backend, pending.id())?);
            drop((pending, plan, pixels));
            if (index + 1).is_multiple_of(32) {
                eprintln!("Bistro image upload {}/{}", index + 1, scene.images.len());
            }
        }
        let mut upload_environment = |image: &ibl::Image| -> Result<ImportImage, Error> {
            let mut graph = pool.begin()?;
            let target = graph.image("immutable environment", environment_desc(image))?;
            graph.upload_image("environment import boundary", target, &image.bytes)?;
            graph.export_image(target)?;
            let plan = graph.compile()?;
            let pending = backend.submit(&plan)?;
            let imported = pending.export_image(target)?;
            drop(super::backend::wait_for(backend, pending.id())?);
            drop((pending, plan));
            Ok(imported)
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
        Ok(Self {
            vertices,
            indices,
            materials,
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
            primitive_bounds,
            shadow_parameters: shadow::Parameters::default(),
            camera,
            local_lights,
            tiles,
            packed,
            upload_owners,
        })
    }
}

const MATERIAL_TYPE: &str = "bistro/pbr";
const COLOR_TEXTURES: [&str; 4] = ["base_color", "normal", "orm", "emissive"];

/// Fixed material rows and reserved binding storage shared by the scene passes.
struct Materials {
    rows: Vec<FrameMaterial<'static>>,
    bindings: Vec<MaterialBinding<'static>>,
}

impl Materials {
    pub fn new(count: usize) -> Self {
        let width = 1 + COLOR_TEXTURES.len();
        Self {
            rows: (0..count)
                .map(|index| FrameMaterial {
                    material_type: MATERIAL_TYPE,
                    bindings: index * width..(index + 1) * width,
                })
                .collect(),
            bindings: Vec::with_capacity(count * width),
        }
    }

    pub fn populate(
        &mut self,
        graph: &Graph,
        scene: &Scene,
        factors: &[BufferHandle],
        images: &[ImageHandle],
    ) -> Result<(), Error> {
        self.bindings.clear();
        for (material, &buffer) in scene.materials.iter().zip(factors) {
            self.bindings.push(MaterialBinding {
                name: "factors",
                resource: BindingResource::Buffer {
                    buffer,
                    offset: 0,
                    size: program::MATERIAL_BYTES as u64,
                },
            });
            for (name, index) in COLOR_TEXTURES.into_iter().zip(material.textures) {
                let view = graph.sampled_image_view(
                    images[index],
                    0,
                    scene.images[index].mips.len() as u32,
                )?;
                self.bindings.push(MaterialBinding {
                    name,
                    resource: BindingResource::ImageView(view),
                });
            }
        }
        Ok(())
    }

    pub fn inputs(&self) -> (&[FrameMaterial<'static>], &[MaterialBinding<'static>]) {
        (&self.rows, &self.bindings)
    }
}

pub(super) struct Workspace {
    vertices: Vec<BufferHandle>,
    indices: Vec<BufferHandle>,
    materials: Vec<BufferHandle>,
    images: Vec<ImageHandle>,
    material_inputs: Materials,
    shadow_cache: Option<ShadowCache>,
    pub(super) placements: Option<instancing::Placements>,
    pub(super) prepared: PreparedSet,
    opaque: Vec<DrawCandidate<'static>>,
    blended: Vec<usize>,
    transparent: Vec<DrawCandidate<'static>>,
    instances: Vec<DrawCandidate<'static>>,
    casters: [Vec<DrawCandidate<'static>>; shadow::CASCADE_COUNT],
    indirect: Vec<FrameIndirectGroup>,
}

/// The shadow shader uses the selected packed matrix from the camera constants.
/// Geometry, materials, textures, shadow extent and shader program are fixed
/// for the lifetime of this cache.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ShadowKey {
    matrix: [[u32; 4]; 4],
    alpha_only: bool,
    shadows: bool,
}

impl ShadowKey {
    fn new(matrix: [[f32; 4]; 4], alpha_only: bool, shadows: bool) -> Self {
        Self {
            matrix: matrix.map(|column| column.map(f32::to_bits)),
            alpha_only,
            shadows,
        }
    }
}

struct ShadowEntry {
    key: ShadowKey,
    image: ImportImage,
}

#[derive(Clone, Copy)]
pub(super) struct ShadowUpdate {
    key: ShadowKey,
    handle: ImageHandle,
}

#[derive(Default)]
struct ShadowCache {
    entries: [Option<ShadowEntry>; shadow::CASCADE_COUNT],
    hits: [u64; shadow::CASCADE_COUNT],
    misses: [u64; shadow::CASCADE_COUNT],
}

impl Workspace {
    pub fn new(scene: &Scene, prepared: PreparedSet) -> Self {
        let primitives = scene.primitives.len();
        Self {
            vertices: Vec::with_capacity(primitives),
            indices: Vec::with_capacity(primitives),
            materials: Vec::with_capacity(scene.materials.len()),
            images: Vec::with_capacity(scene.images.len()),
            material_inputs: Materials::new(scene.materials.len()),
            shadow_cache: None,
            placements: None,
            prepared,
            opaque: Vec::with_capacity(primitives),
            blended: Vec::with_capacity(primitives),
            transparent: Vec::with_capacity(primitives),
            instances: Vec::with_capacity(instancing::COUNT),
            casters: std::array::from_fn(|_| Vec::with_capacity(primitives)),
            indirect: Vec::with_capacity(scene.materials.len()),
        }
    }

    /// The scene and resident caster inputs must stay fixed until the caller
    /// clears reuse. The resource mutation probes use a separate Workspace.
    pub fn enable_shadow_reuse(&mut self) {
        self.shadow_cache.get_or_insert_with(ShadowCache::default);
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept_shadow_updates(
        &mut self,
        pending: &FrameSubmission,
        updates: Option<[Option<ShadowUpdate>; shadow::CASCADE_COUNT]>,
    ) -> Result<(), Error> {
        let (Some(cache), Some(updates)) = (&mut self.shadow_cache, updates) else {
            return Ok(());
        };
        // Publish the complete set of accepted revisions together. Importing
        // the scheduled state on the same queue does not require a CPU wait.
        let mut incoming: [Option<ShadowEntry>; shadow::CASCADE_COUNT] =
            std::array::from_fn(|_| None);
        for (destination, update) in incoming.iter_mut().zip(updates) {
            if let Some(update) = update {
                *destination = Some(ShadowEntry {
                    key: update.key,
                    image: pending.export_image(update.handle)?,
                });
            }
        }
        for (cascade, entry) in incoming.into_iter().enumerate() {
            if let Some(entry) = entry {
                cache.entries[cascade] = Some(entry);
                cache.misses[cascade] += 1;
            } else {
                cache.hits[cascade] += 1;
            }
        }
        Ok(())
    }

    pub fn reset_shadow_reuse_counts(&mut self) {
        if let Some(cache) = &mut self.shadow_cache {
            cache.hits.fill(0);
            cache.misses.fill(0);
        }
    }

    pub fn shadow_reuse_counts(
        &self,
    ) -> Option<([u64; shadow::CASCADE_COUNT], [u64; shadow::CASCADE_COUNT])> {
        self.shadow_cache
            .as_ref()
            .map(|cache| (cache.hits, cache.misses))
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear_shadow_reuse(&mut self, backend: &mut Runtime) -> Result<(), Error> {
        if let Some(cache) = &mut self.shadow_cache {
            for entry in &mut cache.entries {
                if let Some(old) = entry {
                    backend.delete(old.image.key())?;
                    *entry = None;
                }
            }
        }
        self.shadow_cache = None;
        Ok(())
    }
}
pub(super) struct Built {
    pub graph: Graph,
    pub output: ImageHandle,
    pub updates: BufferUpdates,
    pub captures: Option<Captures>,
    pub bloom_capture: Option<PassHandle>,
    #[cfg(all(windows, feature = "presentation"))]
    pub present_capture: Option<PassHandle>,
    pub shadow_captures: Option<[PassHandle; 4]>,
    pub shadow_updates: Option<[Option<ShadowUpdate>; shadow::CASCADE_COUNT]>,
    pub draws: Option<usize>,
}

#[derive(Clone, Copy)]
pub(super) struct BufferUpdates {
    camera: BufferHandle,
    tiles: Option<BufferHandle>,
}

impl Resident {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept_buffer_updates(
        &mut self,
        pending: &FrameSubmission,
        updates: BufferUpdates,
    ) -> Result<(), Error> {
        let camera = pending.export_buffer(updates.camera)?;
        let tiles = updates
            .tiles
            .map(|handle| pending.export_buffer(handle))
            .transpose()?;
        self.camera = camera;
        self.tiles = tiles;
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
    resident: &'a Resident,
    options: &'a Options,
    clip: shadow::CascadeClip,
) -> impl Iterator<Item = usize> + 'a {
    (0..scene.primitives.len()).filter(move |&index| {
        let material = &scene.materials[scene.primitives[index].material];
        options.shadows
            && !material.blend
            && (!options.alpha_only || material.alpha_zero)
            && clip.overlaps(resident.primitive_bounds[index])
    })
}

struct ShadowInputs {
    sources: [FrameImageSource<'static>; shadow::CASCADE_COUNT],
    updates: [Option<ShadowKey>; shadow::CASCADE_COUNT],
}

fn shadow_inputs(
    graph: &mut Graph,
    workspace: &mut Workspace,
    resident: &Resident,
    scene: &Scene,
    options: &Options,
    shadow_data: &shadow::ShadowData,
) -> Result<ShadowInputs, Error> {
    let mut keys = [None; shadow::CASCADE_COUNT];
    let mut sources = CASCADES.map(|name| FrameImageSource {
        name,
        source: ImageSource::Render { target: None },
    });
    for (cascade, (slot, candidates)) in sources.iter_mut().zip(&mut workspace.casters).enumerate()
    {
        candidates.clear();
        let matrix = shadow_data.matrices[cascade];
        if let Some(cache) = &workspace.shadow_cache {
            let key = ShadowKey::new(matrix, options.alpha_only, options.shadows);
            let mut target = None;
            if let Some(entry) = &cache.entries[cascade] {
                let image = graph.import_image(slot.name, &entry.image)?;
                if entry.key == key {
                    slot.source = ImageSource::Reuse(image);
                    continue;
                }
                target = Some(image);
            }
            keys[cascade] = Some(key);
            slot.source = ImageSource::Render { target };
        }
        let clip = shadow::CascadeClip::new(&matrix, resident.scene_bounds);
        let parameters = RasterConstants::new(&[cascade as u32])?;
        candidates.extend(shadow_casters(scene, resident, options, clip).map(|index| {
            candidate(
                scene,
                &workspace.vertices,
                &workspace.indices,
                index,
                "shadow",
                parameters,
            )
        }));
    }
    Ok(ShadowInputs {
        sources,
        updates: keys,
    })
}

fn candidate(
    scene: &Scene,
    vertices: &[BufferHandle],
    indices: &[BufferHandle],
    index: usize,
    queue: &'static str,
    parameters: RasterConstants,
) -> DrawCandidate<'static> {
    let primitive = &scene.primitives[index];
    DrawCandidate {
        queue,
        material: Some(primitive.material),
        draw: RasterDraw {
            bindings: None,
            vertices: vertices[index],
            indices: indices[index],
            first_index: 0,
            index_count: primitive.index_count,
            instance_count: 1,
            parameters,
        },
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit per-frame inputs share no additional ownership"
)]
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
    let uniform = import_frame(&mut graph, workspace, resident, scene, &frame_uniform)?;
    let mut resources = [FrameResource {
        name: "camera",
        resource: uniform.into(),
    }; 5];
    let mut resource_count = 1;
    let mut input = |name, buffer: BufferHandle| {
        resources[resource_count] = FrameResource {
            name,
            resource: buffer.into(),
        };
        resource_count += 1;
    };
    let tiles = resident
        .tiles
        .as_ref()
        .map(|tiles| graph.import_buffer("persistent tile light lists", tiles))
        .transpose()?;
    if let Some(tiles) = tiles {
        input("tiles", tiles);
        graph.export_buffer(tiles)?;
    }
    if let Some(preset) = &resident.local_lights {
        let mut bytes = *preset;
        bytes[..4].copy_from_slice(&options.width.to_le_bytes());
        bytes[4..8].copy_from_slice(&options.height.to_le_bytes());
        let lights = upload_frame_buffer(&mut graph, "local point lights", &bytes)?;
        input("lights", lights);
    }
    let camera_matrix =
        std::array::from_fn(|column| std::array::from_fn(|row| read_word(column * 4 + row)));
    if options.pipeline == PipelineMode::Deferred {
        let inverse = upload_frame_buffer(
            &mut graph,
            "deferred reconstruction",
            &inverse_bytes(camera_matrix)?,
        )?;
        input("reconstruction", inverse);
    }
    let environment_names = ["irradiance", "reflection", "brdf"];
    let mut environment = [None; 3];
    for (index, name) in environment_names.into_iter().enumerate() {
        let image = graph.import_image(name, &resident.environment[index])?;
        environment[index] = Some(FrameSampledView {
            name,
            view: graph.sampled_image_view(image, 0, resident.environment_mips[index])?,
        });
    }
    let sampled_views = environment.map(Option::unwrap);
    let shadows = shadow_inputs(
        &mut graph,
        workspace,
        resident,
        scene,
        options,
        &shadow_data,
    )?;

    let packed = resident
        .packed
        .as_ref()
        .map(|packed| packed.import_frame(&mut graph))
        .transpose()?;
    if let Some(bounds) = packed.as_ref().and_then(|packed| packed.bounds) {
        input("packed_bounds", bounds);
    }
    let frustum = Frustum::new(camera_matrix, resident.scene_bounds);
    let include_material = |index: usize| {
        #[cfg(feature = "development-validation")]
        if options.omit_material == Some(index) {
            return false;
        }
        !options.alpha_only || scene.materials[index].alpha_zero
    };

    workspace.opaque.clear();
    workspace.blended.clear();
    workspace.transparent.clear();
    workspace.instances.clear();
    workspace.indirect.clear();
    let draw = |index, queue| {
        candidate(
            scene,
            &workspace.vertices,
            &workspace.indices,
            index,
            queue,
            RasterConstants::default(),
        )
    };
    for (index, primitive) in scene.primitives.iter().enumerate() {
        let material = &scene.materials[primitive.material];
        if (packed.is_some() && !material.blend) || !include_material(primitive.material) {
            continue;
        }
        if frustum.overlaps(resident.primitive_bounds[index]) {
            if material.blend {
                workspace.blended.push(index);
            } else {
                workspace.opaque.push(draw(index, "opaque"));
            }
        }
    }
    let indirect = packed
        .as_ref()
        .is_some_and(|packed| packed.bounds.is_some());
    if let Some(packed) = &packed {
        if indirect {
            workspace
                .indirect
                .extend(packed.indirect_groups(include_material));
        } else {
            workspace.opaque.extend(packed.direct_candidates(
                &frustum,
                &resident.primitive_bounds,
                include_material,
            ));
        }
    }
    sort_blended(&mut workspace.blended, scene, camera);
    workspace.transparent.extend(
        workspace
            .blended
            .iter()
            .map(|&index| draw(index, "transparent")),
    );
    if let Some(placements) = &workspace.placements {
        let prototype = draw(placements.primitive, "opaque");
        workspace.instances.extend(
            placements
                .draws(options.instances, prototype.draw)
                .map(|draw| DrawCandidate { draw, ..prototype }),
        );
    }
    let draws = (capture && !options.windowed && !indirect).then_some(
        workspace.opaque.len() + workspace.transparent.len() + workspace.instances.len(),
    );

    let views = [
        FrameView {
            name: "main",
            candidates: &workspace.opaque,
        },
        FrameView {
            name: "transparent",
            candidates: &workspace.transparent,
        },
        FrameView {
            name: "instances",
            candidates: &workspace.instances,
        },
        FrameView {
            name: SHADOW_VIEWS[0],
            candidates: &workspace.casters[0],
        },
        FrameView {
            name: SHADOW_VIEWS[1],
            candidates: &workspace.casters[1],
        },
        FrameView {
            name: SHADOW_VIEWS[2],
            candidates: &workspace.casters[2],
        },
        FrameView {
            name: SHADOW_VIEWS[3],
            candidates: &workspace.casters[3],
        },
    ];
    let indirect_list = indirect.then_some(FrameIndirectList {
        name: "packed_opaque",
        groups: &workspace.indirect,
    });
    let (materials, material_bindings) = workspace.material_inputs.inputs();
    let (mut graph, output) = workspace.prepared.select_mut(options).build(
        graph,
        FrameInputs {
            resources: &resources[..resource_count],
            image_sources: &shadows.sources,
            sampled_views: &sampled_views,
            views: &views,
            indirect_lists: indirect_list.as_slice(),
            materials,
            material_bindings,
        },
    )?;
    let named_image = |name| output.named_image(name).expect("prepared Bistro image");
    let images = SceneImages {
        hdr: named_image("hdr"),
        depth: named_image("depth"),
        output: output.image(),
        bloom: output.named_image("bloom_hdr"),
        shadows: CASCADES.map(named_image),
    };
    let mut shadow_updates = workspace
        .shadow_cache
        .as_ref()
        .map(|_| [None; shadow::CASCADE_COUNT]);
    if let Some(updates) = &mut shadow_updates {
        for (cascade, key) in shadows.updates.into_iter().enumerate() {
            if let Some(key) = key {
                let handle = images.shadows[cascade];
                graph.export_image(handle)?;
                updates[cascade] = Some(ShadowUpdate { key, handle });
            }
        }
    }
    finish_frame(
        graph,
        images,
        BufferUpdates {
            camera: uniform,
            tiles,
        },
        shadow_updates,
        draws,
        options,
        capture,
    )
}

fn import_frame(
    graph: &mut Graph,
    workspace: &mut Workspace,
    resident: &Resident,
    scene: &Scene,
    uniform_bytes: &[u8; program::CAMERA_BYTES],
) -> Result<BufferHandle, Error> {
    #[cfg(feature = "tracy")]
    let _imports = tracy_client::Client::running()
        .map(|client| client.span(tracy_client::span_location!("bistro.build.imports"), 0));
    workspace.vertices.clear();
    workspace.indices.clear();
    workspace.materials.clear();
    workspace.images.clear();
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
    for value in &resident.materials {
        workspace
            .materials
            .push(graph.import_buffer("material factors", value)?);
    }
    for value in &resident.images {
        workspace
            .images
            .push(graph.import_image("material image", value)?);
    }
    workspace
        .material_inputs
        .populate(graph, scene, &workspace.materials, &workspace.images)?;
    let uniform = graph.import_buffer("camera", &resident.camera)?;
    graph.upload_buffer("camera update boundary", uniform, 0, uniform_bytes)?;
    graph.export_buffer(uniform)?;
    Ok(uniform)
}

fn upload_frame_buffer(graph: &mut Graph, name: &str, bytes: &[u8]) -> Result<BufferHandle, Error> {
    let buffer = graph.buffer(
        name,
        BufferDesc {
            size: bytes.len() as u64,
        },
    )?;
    graph.upload_buffer("upload Bistro frame inputs", buffer, 0, bytes)?;
    Ok(buffer)
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
    depth: ImageHandle,
    output: ImageHandle,
    bloom: Option<ImageHandle>,
    shadows: [ImageHandle; shadow::CASCADE_COUNT],
}

fn finish_frame(
    mut graph: Graph,
    images: SceneImages,
    updates: BufferUpdates,
    shadow_updates: Option<[Option<ShadowUpdate>; shadow::CASCADE_COUNT]>,
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
    let multisampled = options.sample_count == SampleCount::X4;
    #[cfg(feature = "tracy")]
    let _resolve = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("bistro.build.resolve_and_exports"),
            0,
        )
    });
    graph.export_image(output)?;
    #[cfg(all(windows, feature = "presentation"))]
    let present_capture = if capture && options.windowed {
        Some(graph.readback_image("capture linear presentation source", output)?)
    } else {
        None
    };
    let capture = capture && !options.windowed;
    let captures = if capture {
        Some(Captures {
            hdr: graph.readback_image("capture linear HDR", hdr)?,
            sdr: graph.readback_image("capture sRGB", output)?,
            depth: if multisampled {
                None
            } else {
                Some(graph.readback_image("capture depth", depth)?)
            },
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
        Some([
            graph.readback_image("capture cascade 0", shadows[0])?,
            graph.readback_image("capture cascade 1", shadows[1])?,
            graph.readback_image("capture cascade 2", shadows[2])?,
            graph.readback_image("capture cascade 3", shadows[3])?,
        ])
    } else {
        None
    };
    Ok(Built {
        graph,
        output,
        updates,
        captures,
        bloom_capture,
        #[cfg(all(windows, feature = "presentation"))]
        present_capture,
        shadow_captures,
        shadow_updates,
        draws,
    })
}

fn inverse_bytes(matrix: [[f32; 4]; 4]) -> Result<[u8; 64], Error> {
    // Row operations use f64 and partial pivoting. Serialize the inverse in the
    // original column-major convention shared by the two shader binaries.
    let mut rows = std::array::from_fn::<_, 4, _>(|row| {
        let mut values = [0.0; 8];
        for (column, values_column) in matrix.iter().enumerate() {
            values[column] = f64::from(values_column[row]);
        }
        values[4 + row] = 1.0;
        values
    });
    for column in 0..4 {
        let pivot = (column..4)
            .max_by(|&a, &b| rows[a][column].abs().total_cmp(&rows[b][column].abs()))
            .unwrap();
        rows.swap(column, pivot);
        let diagonal = rows[column][column];
        if !diagonal.is_finite() || diagonal == 0.0 {
            return Err("deferred reconstruction needs an invertible view projection".into());
        }
        for value in &mut rows[column] {
            *value /= diagonal;
        }
        let normalized = rows[column];
        for (row_index, row) in rows.iter_mut().enumerate() {
            if row_index != column {
                let factor = row[column];
                for (value, reference) in row.iter_mut().zip(normalized) {
                    *value -= factor * reference;
                }
            }
        }
    }
    let mut bytes = [0; 64];
    for (word, output) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let value = rows[word % 4][4 + word / 4] as f32;
        if !value.is_finite() {
            return Err("deferred inverse view projection is not finite".into());
        }
        *output = value.to_le_bytes();
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_view_projection_reconstructs_a_world_position() {
        let camera = super::super::camera::Camera::fixture();
        let matrix = camera.matrix(1.75, 0.1, 100.0).unwrap();
        let bytes = inverse_bytes(matrix).unwrap();
        let words = bytes.as_chunks::<4>().0;
        let inverse = std::array::from_fn(|column| {
            std::array::from_fn(|row| f32::from_le_bytes(words[column * 4 + row]))
        });
        let multiply = |matrix: [[f32; 4]; 4], vector: [f32; 4]| {
            std::array::from_fn(|row| {
                matrix
                    .iter()
                    .zip(vector)
                    .map(|(col, scale)| col[row] * scale)
                    .sum::<f32>()
            })
        };
        let world = [0.5, -0.75, 0.25, 1.0];
        let clip = multiply(matrix, world);
        let restored = multiply(inverse, clip);
        for (actual, expected) in restored.into_iter().zip(world) {
            assert!((actual - expected).abs() <= 1e-4);
        }
        assert!(inverse_bytes([[0.0; 4]; 4]).is_err());
    }
    #[test]
    fn shadow_reuse_keys_use_exact_cascade_bits_and_caster_policy() {
        let matrix = [[0.0; 4]; 4];
        let original = ShadowKey::new(matrix, false, true);
        assert!(original == ShadowKey::new(matrix, false, true));
        assert!(original != ShadowKey::new(matrix, true, true));
        assert!(original != ShadowKey::new(matrix, false, false));
        let mut changed = matrix;
        changed[3][0] = -0.0;
        assert!(original != ShadowKey::new(changed, false, true));
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
