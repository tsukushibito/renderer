//! Public Graph composition and explicit working-set budgets for this consumer.

use super::{
    Error, Options, PipelineMode, SubmissionMode,
    asset::Scene,
    camera::{Camera, Frustum},
    deferred, ibl, instancing, lighting,
    output::{tone_map_neutral, write_sdr},
    program::{self, forward_plus},
    shadow,
};
use renderer::{
    ImageFormat,
    graph::{
        Attachment, Binding, BindingResource, BufferHandle, Graph, GraphCapacity, GraphPool,
        ImageDesc, ImageHandle, ImportBuffer, ImportImage, IndexedIndirectArgs, LoadOp, PassHandle,
        RasterAttachments, RasterBindings, RasterConstants, RasterDraw, RasterPass, SampleCount,
        StoreOp,
    },
    program::{ManagedRasterProgram, SamplerDescriptor},
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    graph::BufferDesc,
    runtime::{FrameSubmission, Runtime},
};
use std::sync::Arc;

mod indirect;

pub(super) struct Budget {
    pub graph: GraphCapacity,
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
    extent: [u32; 2],
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

impl Budget {
    pub fn frame_buffers(options: &Options) -> impl Iterator<Item = (u64, usize)> {
        let bytes = match options.pipeline {
            PipelineMode::Deferred => Some(program::deferred::DEFERRED_RECONSTRUCTION_BYTES as u64),
            PipelineMode::ForwardPlus | PipelineMode::ForwardReference | PipelineMode::Forward => {
                None
            }
        };
        bytes.map(|size| (size, options.inflight)).into_iter()
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn resized_buffers(&self, options: &Options) -> Vec<(u64, usize)> {
        let mut classes = Vec::new();
        let temporary = self
            .packed
            .iter()
            .flat_map(|layout| layout.transient_buffers(options));
        for (size, count) in Self::frame_buffers(options).chain(temporary) {
            if let Some((_, available)) = classes.iter_mut().find(|(old, _)| *old == size) {
                *available += count;
            } else {
                classes.push((size, count));
            }
        }
        classes
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

    pub fn frame_images(options: &Options) -> Vec<(ImageDesc, usize)> {
        let mut images = Vec::new();
        let mut add = |desc, count| {
            if let Some((_, available)) = images.iter_mut().find(|(item, _)| *item == desc) {
                *available += count;
            } else {
                images.push((desc, count));
            }
        };
        let hdr = ImageDesc::rgba16_float(options.width, options.height);
        add(hdr, options.inflight * 2);
        if options.pipeline.uses_local_lights() {
            add(lighting::layout().image_desc(), options.inflight);
        }
        if options.pipeline == PipelineMode::ForwardPlus {
            let desc = lighting::layout()
                .tile_mask_desc(options.width, options.height)
                .expect("Forward+ extent validated at preparation");
            add(desc, options.inflight);
        }
        if options.pipeline == PipelineMode::Deferred {
            add(hdr, options.inflight * deferred::PLANES.len());
        }
        if options.sample_count == SampleCount::X4 {
            add(hdr.with_sample_count(SampleCount::X4), options.inflight);
        }
        if let Some(extra) = options.bloom.images(hdr) {
            for desc in extra {
                add(desc, options.inflight);
            }
        }
        add(
            ImageDesc::depth32(options.width, options.height)
                .with_sample_count(options.sample_count),
            options.inflight,
        );
        // Presented images remain owned by the Presenter after Graph completion.
        let presentation_slots = if options.windowed {
            options.inflight
        } else {
            0
        };
        add(
            ImageDesc::rgba8(options.width, options.height),
            options.inflight + presentation_slots,
        );
        let shadow = shadow::Parameters::default().resolution;
        add(ImageDesc::depth32(shadow, shadow), options.inflight * 4);
        images
    }

    pub fn new(
        scene: &Scene,
        environment: &ibl::Environment,
        options: &Options,
    ) -> Result<Self, Error> {
        options.validate_lighting_extent()?;
        let deferred = usize::from(options.pipeline == PipelineMode::Deferred);
        let local_lights = usize::from(options.pipeline.uses_local_lights());
        let tiled_lights = usize::from(options.pipeline == PipelineMode::ForwardPlus);
        let packed = if options.submission == SubmissionMode::Direct {
            None
        } else {
            indirect::Layout::new(scene)?.map(Arc::new)
        };
        let indirect = options.submission == SubmissionMode::Indirect;
        let groups = packed
            .as_ref()
            .filter(|_| indirect)
            .map_or(0, |layout| layout.group_count());
        let packed_buffers = packed.as_ref().map_or(0, |_| 2 + usize::from(indirect));
        let indirect_passes = if groups == 0 { 0 } else { 1 + 2 * groups };
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
        let temporary = packed
            .iter()
            .flat_map(|layout| layout.transient_buffers(options));
        for (size, count) in Self::frame_buffers(options).chain(temporary) {
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
        let bloom_passes = if options.bloom == super::bloom::Bloom::Off {
            0
        } else {
            4
        };
        let mut frame_instances = 0;
        for (desc, count) in Self::frame_images(options) {
            add_image(desc, count);
            frame_instances += count;
        }
        let resources = buffer_count
            + scene.images.len()
            + 11
            + bloom_passes
            + deferred * (1 + deferred::PLANES.len())
            + local_lights
            + tiled_lights
            + usize::from(options.sample_count == SampleCount::X4)
            + indirect_passes;
        let physical_resources = buffer_count
            + scene.images.len()
            + 3
            + frame_instances
            + frame_buffers
            + check_images
            + usize::from(options.resource_checks);
        let bloom_bindings = bloom_passes * 2 + usize::from(bloom_passes != 0);
        let accesses = scene.primitives.len() * 64 + 256 + bloom_bindings + 8 * groups;
        let shadow_words = scene
            .primitives
            .len()
            .checked_mul(shadow::CASCADE_COUNT)
            .ok_or("Raster shadow constant budget overflow")?;
        let instance_words = options
            .instances
            .draws()
            .checked_mul(program::INSTANCE_INLINE_BYTES as usize / size_of::<u32>())
            .ok_or("Raster instance constant budget overflow")?;
        let raster_constant_words = shadow_words
            .checked_add(instance_words)
            .and_then(|words| words.checked_add(1))
            .ok_or("Raster constant budget overflow")?;
        let graph = GraphCapacity {
            resources,
            passes: 24
                + bloom_passes
                + usize::from(bloom_passes != 0)
                + deferred * 2
                + local_lights
                + tiled_lights
                + indirect_passes,
            accesses,
            draws: scene.primitives.len() * 5 + 2 + deferred + options.instances.draws(),
            raster_constant_words: Some(raster_constant_words),
            bindings: scene.materials.len() * (20 + local_lights + tiled_lights)
                + 4
                + bloom_bindings
                + deferred * 16
                + tiled_lights * 3
                + usize::from(options.instances.enabled()) * 16
                + 3 * groups
                + 3 * usize::from(groups != 0),
            attachments: 24 + deferred * deferred::PLANES.len(),
            dependencies: 32,
            initial_ranges: resources * 2,
            name_bytes: resources * 32 + 2048,
            upload_bytes: program::CAMERA_BYTES
                + deferred * program::deferred::DEFERRED_RECONSTRUCTION_BYTES
                + local_lights * lighting::layout().byte_len(),
            edges: accesses * 4,
            segments: accesses * 2,
            readers: accesses * 4,
        };
        // Reserve the conservative D3D12 row/placement-aligned copy footprint,
        // not merely packed asset bytes. Vulkan can use the same bounded budget.
        let frame_upload_bytes = graph.upload_bytes
            + usize::from(options.pipeline.uses_local_lights()) * lighting::upload_padding();
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
            .max(max_buffer_bytes as usize)
            .max(frame_upload_bytes)
            .max(packed.as_ref().filter(|_| indirect).map_or(0, |layout| {
                layout.template_upload_bytes() + frame_upload_bytes
            }))
            .max(4096)
            .checked_add(4096)
            .ok_or("upload byte overflow")?;
        let hdr_captures = if bloom_passes == 0 { 1 } else { 2 };
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
            extent: [options.width, options.height],
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
    local_lights: Option<lighting::LightTable>,
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
    #[cfg(all(windows, feature = "presentation"))]
    pub fn resize_lights(&mut self, options: &Options) -> Result<(), Error> {
        if self.local_lights.is_some() {
            self.local_lights = Some(lighting::LightTable::new(
                options.fixture,
                [options.width, options.height],
                options.pipeline == PipelineMode::ForwardPlus,
            )?);
        }
        Ok(())
    }

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
            .then(|| {
                lighting::LightTable::new(
                    budget.fixture,
                    budget.extent,
                    budget.pipeline == PipelineMode::ForwardPlus,
                )
            })
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
            packed,
            upload_owners,
        })
    }
}

pub(super) struct Workspace {
    vertices: Vec<BufferHandle>,
    indices: Vec<BufferHandle>,
    materials: Vec<BufferHandle>,
    images: Vec<ImageHandle>,
    bindings: Vec<RasterBindings>,
    shadow_bindings: Vec<RasterBindings>,
    shadow_cache: Option<ShadowCache>,
    pub(super) placements: Option<instancing::Placements>,
    opaque: Vec<usize>,
    indirect: Vec<IndexedIndirectArgs>,
    blended: Vec<usize>,
}

/// The shadow shader uses the selected packed matrix from the camera constants.
/// Geometry, materials, textures, shadow extent and shader program are fixed
/// for the lifetime of this cache.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ShadowKey {
    matrix: [[u32; 4]; 4],
    alpha_only: bool,
}

impl ShadowKey {
    fn new(matrix: [[f32; 4]; 4], alpha_only: bool) -> Self {
        Self {
            matrix: matrix.map(|column| column.map(f32::to_bits)),
            alpha_only,
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
    pub fn new(scene: &Scene) -> Self {
        Self {
            vertices: Vec::with_capacity(scene.primitives.len()),
            indices: Vec::with_capacity(scene.primitives.len()),
            materials: Vec::with_capacity(scene.materials.len()),
            images: Vec::with_capacity(scene.images.len()),
            bindings: Vec::with_capacity(scene.materials.len()),
            shadow_bindings: Vec::with_capacity(scene.materials.len()),
            shadow_cache: None,
            placements: None,
            opaque: Vec::with_capacity(scene.primitives.len()),
            indirect: Vec::with_capacity(scene.materials.len()),
            blended: Vec::with_capacity(scene.primitives.len()),
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
}

impl Resident {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept_buffer_updates(
        &mut self,
        pending: &FrameSubmission,
        updates: BufferUpdates,
    ) -> Result<(), Error> {
        let camera = pending.export_buffer(updates.camera)?;
        self.camera = camera;
        Ok(())
    }
}

pub(super) struct Captures {
    pub hdr: PassHandle,
    pub sdr: PassHandle,
    pub depth: Option<PassHandle>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit consumer frame inputs share no additional ownership"
)]
pub(super) fn build(
    pool: &mut GraphPool,
    workspace: &mut Workspace,
    resident: &Resident,
    scene: &Scene,
    options: &Options,
    camera: Camera,
    camera_bytes: &[u8; 128],
    background: [f32; 3],
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
    let uniform = {
        #[cfg(feature = "tracy")]
        let _imports = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("bistro.build.imports"), 0));
        workspace.vertices.clear();
        workspace.indices.clear();
        workspace.materials.clear();
        workspace.images.clear();
        workspace.bindings.clear();
        workspace.shadow_bindings.clear();
        workspace.opaque.clear();
        workspace.blended.clear();
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
        let uniform = graph.import_buffer("camera", &resident.camera)?;
        graph.upload_buffer("camera update boundary", uniform, 0, &frame_uniform)?;
        graph.export_buffer(uniform)?;
        uniform
    };
    let mut local_light_bindings = [None; 2];
    if let Some(table) = &resident.local_lights {
        let (lights, tiles) = table.append(&mut graph, uniform)?;
        for (slot, binding) in local_light_bindings
            .iter_mut()
            .zip(forward_plus::raster_bindings(lights, tiles))
        {
            *slot = Some(binding);
        }
    }
    let environment = [
        graph.import_image("diffuse environment", &resident.environment[0])?,
        graph.import_image("specular environment", &resident.environment[1])?,
        graph.import_image("BRDF integration", &resident.environment[2])?,
    ];
    let resolution = resident.shadow_parameters.resolution;
    let shadow_desc = ImageDesc::depth32(resolution, resolution);
    let shadow_cache = workspace.shadow_cache.as_ref();
    let mut shadow_updates = shadow_cache.map(|_| [None; shadow::CASCADE_COUNT]);
    let mut redraw = [true; shadow::CASCADE_COUNT];
    let mut make_shadow = |cascade: usize| -> Result<ImageHandle, Error> {
        let (Some(cache), Some(updates)) = (shadow_cache, &mut shadow_updates) else {
            return Ok(graph.image("sun cascade", shadow_desc)?);
        };
        let key = ShadowKey::new(shadow_data.matrices[cascade], options.alpha_only);
        let old = cache.entries[cascade].as_ref();
        let handle = match old {
            Some(entry) => graph.import_image("sun cascade", &entry.image)?,
            None => graph.image("sun cascade", shadow_desc)?,
        };
        if old.is_some_and(|entry| entry.key == key) {
            redraw[cascade] = false;
        } else {
            graph.export_image(handle)?;
            updates[cascade] = Some(ShadowUpdate { key, handle });
        }
        Ok(handle)
    };
    let shadows = [
        make_shadow(0)?,
        make_shadow(1)?,
        make_shadow(2)?,
        make_shadow(3)?,
    ];
    let shadow_views = [
        graph.image_view(shadows[0], 0, 0)?,
        graph.image_view(shadows[1], 0, 0)?,
        graph.image_view(shadows[2], 0, 0)?,
        graph.image_view(shadows[3], 0, 0)?,
    ];
    let shadow_program = program::shadow_program();
    let redraw_any = redraw.into_iter().any(|value| value);
    for (index, material) in scene.materials.iter().enumerate().filter(|_| redraw_any) {
        let base = material.textures[0];
        let bindings = [
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: uniform,
                    offset: 0,
                    size: program::CAMERA_BYTES as u64,
                },
            },
            Binding {
                group: 0,
                binding: 1,
                resource: BindingResource::Buffer {
                    buffer: workspace.materials[index],
                    offset: 0,
                    size: 64,
                },
            },
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::ImageView(graph.sampled_image_view(
                    workspace.images[base],
                    0,
                    scene.images[base].mips.len() as u32,
                )?),
            },
            Binding {
                group: 1,
                binding: 4,
                resource: BindingResource::Sampler(SamplerDescriptor::LinearRepeat),
            },
        ];
        workspace
            .shadow_bindings
            .push(graph.raster_bindings(&shadow_program, &bindings)?);
    }
    for (cascade, view) in shadow_views.into_iter().enumerate() {
        if !redraw[cascade] {
            continue;
        }
        let parameters = RasterConstants::new(&[cascade as u32])?;
        let clip = shadow::CascadeClip::new(&shadow_data.matrices[cascade], resident.scene_bounds);
        let draws = (0..scene.primitives.len())
            .filter(|&index| {
                let material = &scene.materials[scene.primitives[index].material];
                options.shadows
                    && !material.blend
                    && (!options.alpha_only || material.alpha_zero)
                    && clip.overlaps(resident.primitive_bounds[index])
            })
            .map(|index| {
                let primitive = &scene.primitives[index];
                RasterDraw {
                    bindings: Some(workspace.shadow_bindings[primitive.material]),
                    vertices: workspace.vertices[index],
                    indices: workspace.indices[index],
                    first_index: 0,
                    index_count: primitive.index_count,
                    instance_count: 1,
                    parameters,
                }
            });
        graph.raster(
            "sun shadow depth",
            RasterAttachments {
                color_resolves: &[],
                colors: &[],
                depth: Some(Attachment {
                    view,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
            |raster| raster.draw(&shadow_program, draws),
        )?;
    }
    let lighting_bindings = [
        Binding {
            group: 2,
            binding: 0,
            resource: BindingResource::ImageView(graph.sampled_image_view(
                environment[0],
                0,
                resident.environment_mips[0],
            )?),
        },
        Binding {
            group: 2,
            binding: 1,
            resource: BindingResource::ImageView(graph.sampled_image_view(
                environment[1],
                0,
                resident.environment_mips[1],
            )?),
        },
        Binding {
            group: 2,
            binding: 2,
            resource: BindingResource::ImageView(graph.sampled_image_view(
                environment[2],
                0,
                resident.environment_mips[2],
            )?),
        },
        Binding {
            group: 2,
            binding: 3,
            resource: BindingResource::Sampler(SamplerDescriptor::LinearRepeatUClampV),
        },
        Binding {
            group: 3,
            binding: 0,
            resource: BindingResource::ImageView(shadow_views[0]),
        },
        Binding {
            group: 3,
            binding: 1,
            resource: BindingResource::ImageView(shadow_views[1]),
        },
        Binding {
            group: 3,
            binding: 2,
            resource: BindingResource::ImageView(shadow_views[2]),
        },
        Binding {
            group: 3,
            binding: 3,
            resource: BindingResource::ImageView(shadow_views[3]),
        },
        Binding {
            group: 3,
            binding: 4,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ];
    let deferred = options.pipeline == PipelineMode::Deferred;
    let mut instance_bindings = None;
    let instance_material = workspace
        .placements
        .as_ref()
        .map(|placement| scene.primitives[placement.primitive].material);
    {
        #[cfg(feature = "tracy")]
        let _bindings = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("bistro.build.bindings"), 0));
        for (index, material) in scene.materials.iter().enumerate() {
            let image = |slot: usize| -> Result<BindingResource, Error> {
                let handle = workspace.images[material.textures[slot]];
                let mip_count = scene.images[material.textures[slot]].mips.len() as u32;
                Ok(BindingResource::ImageView(
                    graph.sampled_image_view(handle, 0, mip_count)?,
                ))
            };
            let material_bindings = [
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: uniform,
                        offset: 0,
                        size: program::CAMERA_BYTES as u64,
                    },
                },
                Binding {
                    group: 0,
                    binding: 1,
                    resource: BindingResource::Buffer {
                        buffer: workspace.materials[index],
                        offset: 0,
                        size: 64,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: image(0)?,
                },
                Binding {
                    group: 1,
                    binding: 1,
                    resource: image(1)?,
                },
                Binding {
                    group: 1,
                    binding: 2,
                    resource: image(2)?,
                },
                Binding {
                    group: 1,
                    binding: 3,
                    resource: image(3)?,
                },
                Binding {
                    group: 1,
                    binding: 4,
                    resource: BindingResource::Sampler(SamplerDescriptor::LinearRepeat),
                },
            ];
            let bindings = if deferred && !material.blend {
                graph.raster_bindings(&program::deferred::gbuffer_program(), &material_bindings)?
            } else {
                let program = options.pipeline.forward_program(material.blend);
                let mut bindings = [material_bindings[0]; renderer::program::MAX_RASTER_BINDINGS];
                let shared = material_bindings.len() + lighting_bindings.len();
                bindings[..material_bindings.len()].copy_from_slice(&material_bindings);
                bindings[material_bindings.len()..shared].copy_from_slice(&lighting_bindings);
                for (slot, binding) in bindings[shared..]
                    .iter_mut()
                    .zip(local_light_bindings.into_iter().flatten())
                {
                    *slot = binding;
                }
                if instance_material == Some(index) {
                    instance_bindings = Some(
                        graph
                            .raster_bindings(&program::instancing_program(), &bindings[..shared])?,
                    );
                }
                graph.raster_bindings(&program, &bindings[..program.bindings().len()])?
            };
            workspace.bindings.push(bindings);
        }
    }
    let multisampled = options.sample_count == SampleCount::X4;
    let scene_clear = [background[0], background[1], background[2], 1.0];
    let (color, depth, output, color_view, depth_view, resolve_view) = {
        #[cfg(feature = "tracy")]
        let _targets = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("bistro.build.targets"), 0));
        let hdr_desc = ImageDesc::rgba16_float(options.width, options.height);
        let color = graph.image("scene linear HDR", hdr_desc)?;
        let (color_view, resolve_view) = if multisampled {
            let multisample = graph.image(
                "scene multisampled HDR",
                hdr_desc.with_sample_count(options.sample_count),
            )?;
            (
                graph.image_view(multisample, 0, 0)?,
                Some(graph.image_view(color, 0, 0)?),
            )
        } else {
            (graph.image_view(color, 0, 0)?, None)
        };
        let depth = graph.image(
            "scene depth",
            ImageDesc::depth32(options.width, options.height)
                .with_sample_count(options.sample_count),
        )?;
        let output = graph.image(
            "B2 sRGB output",
            ImageDesc::rgba8(options.width, options.height),
        )?;
        let depth_view = graph.image_view(depth, 0, 0)?;
        if !multisampled && !deferred {
            graph.attachment_pass(
                "scene clears",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[Attachment {
                        view: color_view,
                        load: LoadOp::Clear(scene_clear),
                        store: StoreOp::Store,
                    }],
                    depth: Some(Attachment {
                        view: depth_view,
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    }),
                },
            )?;
        }
        (color, depth, output, color_view, depth_view, resolve_view)
    };
    let camera_matrix =
        std::array::from_fn(|column| std::array::from_fn(|row| read_word(column * 4 + row)));
    let frustum = Frustum::new(camera_matrix, resident.scene_bounds);
    let include_material = |index: usize| {
        #[cfg(feature = "development-validation")]
        if options.omit_material == Some(index) {
            return false;
        }
        !options.alpha_only || scene.materials[index].alpha_zero
    };
    let packed = resident
        .packed
        .as_ref()
        .map(|packed| packed.prepare_frame(&mut graph, uniform, &mut workspace.indirect))
        .transpose()?;
    for (index, primitive) in scene.primitives.iter().enumerate() {
        let material = &scene.materials[primitive.material];
        if (packed.is_some() && !material.blend) || !include_material(primitive.material) {
            continue;
        }
        if frustum.overlaps(resident.primitive_bounds[index]) {
            if material.blend {
                workspace.blended.push(index);
            } else {
                workspace.opaque.push(index);
            }
        }
    }
    let draw = |index: usize| {
        let primitive = &scene.primitives[index];
        RasterDraw {
            bindings: Some(workspace.bindings[primitive.material]),
            vertices: workspace.vertices[index],
            indices: workspace.indices[index],
            first_index: 0,
            index_count: primitive.index_count,
            instance_count: 1,
            parameters: RasterConstants::default(),
        }
    };
    let record_opaque = |raster: &mut RasterPass<'_, '_>, program: &ManagedRasterProgram| {
        if let Some(packed) = &packed {
            packed.draw(
                raster,
                program,
                &workspace.bindings,
                &frustum,
                &resident.primitive_bounds,
                include_material,
            )
        } else {
            raster.draw(program, workspace.opaque.iter().copied().map(draw))
        }
    };
    let final_resolve = resolve_view.map(Some);
    if deferred {
        deferred::append(
            &mut graph,
            deferred::Inputs {
                point_lights: None,
                hdr: color_view,
                depth: depth_view,
                clear: scene_clear,
                camera: uniform,
                view_projection: camera_matrix,
                lighting: lighting_bindings,
            },
            record_opaque,
        )?;
    } else {
        #[cfg(feature = "tracy")]
        let _opaque = tracy_client::Client::running().map(|client| {
            client.span(tracy_client::span_location!("bistro.build.opaque_draws"), 0)
        });
        // Bind the resolve to the last scene pass. With no opaque draws, the
        // same operation clears the attachments before continuing or resolving.
        let final_opaque = multisampled && workspace.blended.is_empty();
        let store = if final_opaque {
            StoreOp::Discard
        } else {
            StoreOp::Store
        };
        graph.raster(
            "Forward PBR opaque and mask",
            RasterAttachments {
                color_resolves: if final_opaque {
                    final_resolve.as_slice()
                } else {
                    &[]
                },
                colors: &[Attachment {
                    view: color_view,
                    load: if multisampled {
                        LoadOp::Clear(scene_clear)
                    } else {
                        LoadOp::Load
                    },
                    store,
                }],
                depth: Some(Attachment {
                    view: depth_view,
                    load: if multisampled {
                        LoadOp::Clear(1.0)
                    } else {
                        LoadOp::Load
                    },
                    store,
                }),
            },
            |raster| {
                record_opaque(raster, &options.pipeline.forward_program(false))?;
                if let Some(placements) = &workspace.placements {
                    let instance = RasterDraw {
                        bindings: instance_bindings,
                        ..draw(placements.primitive)
                    };
                    raster.draw(
                        &program::instancing_program(),
                        placements.draws(options.instances, instance),
                    )?;
                }
                Ok(())
            },
        )?;
    }
    {
        #[cfg(feature = "tracy")]
        let _sort = tracy_client::Client::running().map(|client| {
            client.span(tracy_client::span_location!("bistro.build.blended_sort"), 0)
        });
        // A deterministic far-to-near primitive order. This is ordinary alpha
        // blending, not order-independent transparency or per-triangle sorting.
        workspace.blended.sort_unstable_by(|&a, &b| {
            camera
                .distance_squared(scene.primitives[b].center)
                .total_cmp(&camera.distance_squared(scene.primitives[a].center))
                .then(a.cmp(&b))
        });
    }
    {
        #[cfg(feature = "tracy")]
        let _blended = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("bistro.build.blended_draws"),
                0,
            )
        });
        if !workspace.blended.is_empty() {
            let store = if multisampled {
                StoreOp::Discard
            } else {
                StoreOp::Store
            };
            graph.raster(
                "Forward PBR alpha blend",
                RasterAttachments {
                    color_resolves: final_resolve.as_slice(),
                    colors: &[Attachment {
                        view: color_view,
                        load: LoadOp::Load,
                        store,
                    }],
                    depth: Some(Attachment {
                        view: depth_view,
                        load: LoadOp::Load,
                        store,
                    }),
                },
                |raster| {
                    raster.draw(
                        &options.pipeline.forward_program(true),
                        workspace.blended.iter().copied().map(draw),
                    )
                },
            )?;
        }
    }
    #[cfg(feature = "tracy")]
    let _resolve = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("bistro.build.resolve_and_exports"),
            0,
        )
    });
    let hdr = ImageDesc::rgba16_float(options.width, options.height);
    let bloom = options.bloom.add_passes(&mut graph, color)?;
    let tone = graph.image("Neutral display linear", hdr)?;
    let bloom_view = graph.image_view(bloom, 0, 0)?;
    tone_map_neutral(&mut graph, bloom_view, tone)?;
    let tone_view = graph.image_view(tone, 0, 0)?;
    write_sdr(
        &mut graph,
        if options.windowed {
            "FXAA and linear presentation source"
        } else {
            "FXAA and single sRGB encode"
        },
        tone_view,
        output,
        options.sdr_output(),
    )?;
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
            hdr: graph.readback_image("capture linear HDR", color)?,
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
    let bloom_capture = if capture && bloom != color {
        Some(graph.readback_image("capture post-Bloom HDR", bloom)?)
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
    let opaque_draws = match (capture, &packed) {
        (true, Some(packed)) => {
            packed.visible_direct_draws(&frustum, &resident.primitive_bounds, include_material)
        }
        (true, None) => Some(workspace.opaque.len()),
        (false, _) => None,
    };
    let draws =
        opaque_draws.map(|count| count + workspace.blended.len() + options.instances.draws());
    Ok(Built {
        graph,
        output,
        updates: BufferUpdates { camera: uniform },
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

    #[test]
    fn shadow_reuse_keys_use_exact_cascade_bits_and_caster_policy() {
        let matrix = [[0.0; 4]; 4];
        let original = ShadowKey::new(matrix, false);
        assert!(original == ShadowKey::new(matrix, false));
        assert!(original != ShadowKey::new(matrix, true));
        let mut changed = matrix;
        changed[3][0] = -0.0;
        assert!(original != ShadowKey::new(changed, false));
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
        let one = Budget::new(&scene, &environment, &options).unwrap();
        options.inflight = 4;
        let four = Budget::new(&scene, &environment, &options).unwrap();
        assert_eq!(one.buffers, four.buffers);
        assert_eq!(four.physical_resources - one.physical_resources, 24);
        assert_eq!(four.graph.resources, one.graph.resources);
        assert!(
            one.readback_bytes > 101 * 73 * 16,
            "odd output rows need padding beyond packed texels"
        );
        options.sample_count = SampleCount::X4;
        let multisampled = Budget::new(&scene, &environment, &options).unwrap();
        assert_eq!(multisampled.physical_resources - four.physical_resources, 4);
        for format in [ImageFormat::Rgba16Float, ImageFormat::Depth32Float] {
            assert!(multisampled.images.iter().any(|&(desc, count)| {
                desc.format == format && desc.sample_count == SampleCount::X4 && count == 4
            }));
        }
        options.sample_count = SampleCount::X1;
        options.pipeline = PipelineMode::Deferred;
        let deferred = Budget::new(&scene, &environment, &options).unwrap();
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
            Budget::new(&scene, &ibl::Environment::fixture(), &tiny_output_options())
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
        let budget = Budget::new(&scene, &environment, &options).unwrap();
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
