//! Public Graph composition and explicit working-set budgets for this consumer.

use super::{Error, Options, asset::Scene, backend::Backend, camera::Camera, ibl, program, shadow};
use renderer::{
    ImageFormat,
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity,
        GraphPool, ImageDesc, ImageHandle, ImportBuffer, ImportImage, LoadOp, PassHandle,
        RasterAttachments, RasterBindings, RasterConstants, RasterDraw, StoreOp,
    },
    program::SamplerDescriptor,
};

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
    pub fn new(
        scene: &Scene,
        environment: &ibl::Environment,
        options: &Options,
    ) -> Result<Self, Error> {
        let buffer_count = scene
            .primitives
            .len()
            .checked_mul(2)
            .and_then(|v| v.checked_add(scene.materials.len() + 3))
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
        add(24);
        add(12);
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
        // coexist until completion. Resolve uses the same paired-stream path.
        let resolve_bytes =
            size_of_val(&program::RESOLVE_VERTICES) + size_of_val(&program::RESOLVE_INDICES);
        let geometry_upload_bytes =
            scene
                .primitives
                .iter()
                .try_fold(resolve_bytes, |maximum, primitive| {
                    primitive
                        .vertices
                        .len()
                        .checked_add(primitive.indices.len())
                        .map(|bytes| maximum.max(bytes))
                        .ok_or("geometry upload byte overflow")
                })?;
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
        add_image(
            ImageDesc::rgba16_float(options.width, options.height),
            options.inflight * 2,
        );
        add_image(
            ImageDesc::depth32(options.width, options.height),
            options.inflight,
        );
        add_image(
            ImageDesc::rgba8(options.width, options.height),
            options.inflight,
        );
        add_image(
            ImageDesc::depth32(
                shadow::Parameters::default().resolution,
                shadow::Parameters::default().resolution,
            ),
            options.inflight * 4,
        );
        let resources = buffer_count + scene.images.len() + 11;
        let physical_resources = buffer_count
            + scene.images.len()
            + 3
            + options.inflight * 8
            + check_images
            + usize::from(options.resource_checks);
        let accesses = scene.primitives.len() * 64 + 256;
        let graph = GraphCapacity {
            resources,
            passes: 24,
            accesses,
            draws: scene.primitives.len() * 5 + 2,
            bindings: scene.materials.len() * 20 + 4,
            attachments: 24,
            dependencies: 32,
            initial_ranges: resources * 2,
            name_bytes: resources * 32 + 2048,
            upload_bytes: program::CAMERA_BYTES,
            edges: accesses * 4,
            segments: accesses * 2,
            readers: accesses * 4,
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
            .max(max_buffer_bytes as usize)
            .max(4096)
            .checked_add(4096)
            .ok_or("upload byte overflow")?;
        let mut frame_readback_bytes = ((u64::from(options.width) * 8).next_multiple_of(256)
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
            buffer_count,
            physical_resources,
            max_buffer_bytes,
            upload_bytes,
            readback_bytes,
            ranges: resources * (options.inflight + 3) + 32,
        })
    }
    pub fn upload_pool(&self) -> Result<GraphPool, Error> {
        Ok(GraphPool::new(
            GraphCapacity {
                resources: 1,
                passes: 1,
                accesses: 32,
                draws: 0,
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
    pub shadow_bounds: shadow::Bounds,
    pub shadow_parameters: shadow::Parameters,
    pub camera: ImportBuffer,
    pub resolve_vertices: ImportBuffer,
    pub resolve_indices: ImportBuffer,
    // Only small general Uniform uploads retain byte provenance. Geometry uses
    // typed immutable bounds and releases all upload CPU streams before frames.
    pub upload_owners: Vec<GraphPool>,
}

fn upload_geometry<B: Backend>(
    backend: &mut B,
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
    let mut pending = backend.submit(&plan)?;
    let imports = (
        B::export_buffer(&pending, vertex)?,
        B::export_buffer(&pending, index)?,
    );
    drop(B::wait(&mut pending, super::WAIT)?);
    drop((pending, plan, geometry));
    // Native canonical provenance carries scalar index bounds, not CPU bytes.
    // Release this slot's final asset Arc at the explicit load boundary.
    pool.release_idle();
    Ok(imports)
}

pub(super) fn upload_buffer<B: Backend>(
    backend: &mut B,
    owners: &mut Vec<GraphPool>,
    bytes: &[u8],
) -> Result<ImportBuffer, Error> {
    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 1,
            passes: 1,
            accesses: 1,
            draws: 0,
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
    let mut pending = backend.submit(&plan)?;
    let imported = B::export_buffer(&pending, buffer)?;
    drop(B::wait(&mut pending, super::WAIT)?);
    drop((pending, plan));
    owners.push(pool);
    Ok(imported)
}

impl Resident {
    pub fn upload<B: Backend>(
        backend: &mut B,
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
        for (index, primitive) in scene.primitives.iter().enumerate() {
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
                &scene.vertices[primitive.vertices.clone()],
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
        let resolve_vertices = program::RESOLVE_VERTICES
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        let (resolve_vertices, resolve_indices) = upload_geometry(
            backend,
            &mut geometry_pool,
            8,
            &resolve_vertices,
            &program::RESOLVE_INDICES,
        )?;
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
            let mut pending = backend.submit(&plan)?;
            images.push(B::export_image(&pending, target)?);
            drop(B::wait(&mut pending, super::WAIT)?);
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
            let mut pending = backend.submit(&plan)?;
            let imported = B::export_image(&pending, target)?;
            drop(B::wait(&mut pending, super::WAIT)?);
            drop((pending, plan));
            Ok(imported)
        };
        let environment_images = [
            upload_environment(&environment.diffuse)?,
            upload_environment(&environment.specular)?,
            upload_environment(&environment.brdf)?,
        ];
        let mut minimum = [f32::INFINITY; 3];
        let mut maximum = [f32::NEG_INFINITY; 3];
        if scene.vertices.is_empty() {
            // An empty scene is a valid clear-only control, with no casters.
            minimum = [-1.0; 3];
            maximum = [1.0; 3];
        }
        for vertex in scene.vertices.as_chunks::<{ program::VERTEX_BYTES }>().0 {
            for axis in 0..3 {
                let value = f32::from_le_bytes(vertex[axis * 4..axis * 4 + 4].try_into().unwrap());
                minimum[axis] = minimum[axis].min(value);
                maximum[axis] = maximum[axis].max(value);
            }
        }
        let shadow_bounds = shadow::Bounds::new(minimum, maximum)?;
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
            shadow_bounds,
            shadow_parameters: shadow::Parameters::default(),
            camera,
            resolve_vertices,
            resolve_indices,
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
    blended: Vec<usize>,
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
            blended: Vec::with_capacity(scene.primitives.len()),
        }
    }
}
pub(super) struct Built {
    pub graph: Graph,
    pub output: ImageHandle,
    pub camera: BufferHandle,
    pub captures: Option<[PassHandle; 3]>,
    pub shadow_captures: Option<[PassHandle; 4]>,
    pub draws: usize,
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
        resident.shadow_bounds,
        resident.shadow_parameters,
    )?;
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
    let (uniform, resolve_vertices, resolve_indices) = {
        #[cfg(feature = "tracy")]
        let _imports = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("bistro.build.imports"), 0));
        workspace.vertices.clear();
        workspace.indices.clear();
        workspace.materials.clear();
        workspace.images.clear();
        workspace.bindings.clear();
        workspace.shadow_bindings.clear();
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
        let resolve_vertices =
            graph.import_buffer("resolve vertices", &resident.resolve_vertices)?;
        let resolve_indices = graph.import_buffer("resolve indices", &resident.resolve_indices)?;
        (uniform, resolve_vertices, resolve_indices)
    };
    let environment = [
        graph.import_image("diffuse environment", &resident.environment[0])?,
        graph.import_image("specular environment", &resident.environment[1])?,
        graph.import_image("BRDF integration", &resident.environment[2])?,
    ];
    let mut make_shadow = || {
        graph.image(
            "sun cascade",
            ImageDesc::depth32(
                resident.shadow_parameters.resolution,
                resident.shadow_parameters.resolution,
            ),
        )
    };
    let shadows = [
        make_shadow()?,
        make_shadow()?,
        make_shadow()?,
        make_shadow()?,
    ];
    let shadow_views = [
        graph.image_view(shadows[0], 0, 0)?,
        graph.image_view(shadows[1], 0, 0)?,
        graph.image_view(shadows[2], 0, 0)?,
        graph.image_view(shadows[3], 0, 0)?,
    ];
    let shadow_program = program::shadow_program();
    for (index, material) in scene.materials.iter().enumerate() {
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
        let parameters = RasterConstants::new(&[cascade as u32])?;
        let draws = (0..scene.primitives.len())
            .filter(|&index| {
                let material = &scene.materials[scene.primitives[index].material];
                options.shadows && !material.blend && (!options.alpha_only || material.alpha_zero)
            })
            .map(|index| {
                let primitive = &scene.primitives[index];
                RasterDraw {
                    bindings: Some(workspace.shadow_bindings[primitive.material]),
                    vertices: workspace.vertices[index],
                    indices: workspace.indices[index],
                    index_count: primitive.index_count,
                    parameters,
                }
            });
        graph.raster(
            "sun shadow depth",
            &shadow_program,
            draws,
            RasterAttachments {
                colors: &[],
                depth: Some(Attachment {
                    view,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )?;
    }
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
            let program = if material.blend {
                program::blend_program()
            } else {
                program::opaque_program()
            };
            workspace
                .bindings
                .push(graph.raster_bindings(&program, &bindings)?);
        }
    }
    let (color, depth, output, color_view, depth_view) = {
        #[cfg(feature = "tracy")]
        let _targets = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("bistro.build.targets"), 0));
        let color = graph.image(
            "scene linear HDR",
            ImageDesc::rgba16_float(options.width, options.height),
        )?;
        let depth = graph.image(
            "scene depth",
            ImageDesc::depth32(options.width, options.height),
        )?;
        let output = graph.image(
            "B2 sRGB output",
            ImageDesc::rgba8(options.width, options.height),
        )?;
        let color_view = graph.image_view(color, 0, 0)?;
        let depth_view = graph.image_view(depth, 0, 0)?;
        graph.attachment_pass(
            "scene clears",
            RasterAttachments {
                colors: &[Attachment {
                    view: color_view,
                    load: LoadOp::Clear([background[0], background[1], background[2], 1.0]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: depth_view,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )?;
        (color, depth, output, color_view, depth_view)
    };
    let eligible = |index: usize| {
        !options.alpha_only || scene.materials[scene.primitives[index].material].alpha_zero
    };
    let draw = |index: usize| {
        let primitive = &scene.primitives[index];
        RasterDraw {
            bindings: Some(workspace.bindings[primitive.material]),
            vertices: workspace.vertices[index],
            indices: workspace.indices[index],
            index_count: primitive.index_count,
            parameters: RasterConstants::default(),
        }
    };
    {
        #[cfg(feature = "tracy")]
        let _opaque = tracy_client::Client::running().map(|client| {
            client.span(tracy_client::span_location!("bistro.build.opaque_draws"), 0)
        });
        let opaque = (0..scene.primitives.len()).filter(|&index| {
            eligible(index) && !scene.materials[scene.primitives[index].material].blend
        });
        graph.raster(
            "Forward PBR opaque and mask",
            &program::opaque_program(),
            opaque.map(draw),
            RasterAttachments {
                colors: &[Attachment {
                    view: color_view,
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: depth_view,
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }),
            },
        )?;
    }
    {
        #[cfg(feature = "tracy")]
        let _sort = tracy_client::Client::running().map(|client| {
            client.span(tracy_client::span_location!("bistro.build.blended_sort"), 0)
        });
        for index in 0..scene.primitives.len() {
            if eligible(index) && scene.materials[scene.primitives[index].material].blend {
                workspace.blended.push(index);
            }
        }
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
            graph.raster(
                "Forward PBR alpha blend",
                &program::blend_program(),
                workspace.blended.iter().copied().map(draw),
                RasterAttachments {
                    colors: &[Attachment {
                        view: color_view,
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    }],
                    depth: Some(Attachment {
                        view: depth_view,
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    }),
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
    let tone = graph.image(
        "Neutral display linear",
        ImageDesc::rgba16_float(options.width, options.height),
    )?;
    let tone_program = program::tone_program();
    let tone_bindings = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(color_view),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ];
    let tone_bindings = graph.raster_bindings(&tone_program, &tone_bindings)?;
    graph.raster(
        "fixed exposure and PBR Neutral",
        &tone_program,
        [RasterDraw {
            bindings: Some(tone_bindings),
            vertices: resolve_vertices,
            indices: resolve_indices,
            index_count: 3,
            parameters: RasterConstants::default(),
        }],
        RasterAttachments {
            colors: &[Attachment {
                view: graph.image_view(tone, 0, 0)?,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
        },
    )?;
    let resolve_bindings = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(graph.image_view(tone, 0, 0)?),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(if options.aa {
                SamplerDescriptor::LinearClamp
            } else {
                SamplerDescriptor::NearestClamp
            }),
        },
    ];
    let resolve_program = if options.aa {
        program::resolve_aa_program()
    } else {
        program::resolve_program()
    };
    let resolve_bindings = graph.raster_bindings(&resolve_program, &resolve_bindings)?;
    graph.raster(
        "FXAA and single sRGB encode",
        &resolve_program,
        [RasterDraw {
            bindings: Some(resolve_bindings),
            vertices: resolve_vertices,
            indices: resolve_indices,
            index_count: 3,
            parameters: RasterConstants::default(),
        }],
        RasterAttachments {
            colors: &[Attachment {
                view: graph.image_view(output, 0, 0)?,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
        },
    )?;
    graph.export_image(output)?;
    let captures = if capture {
        Some([
            graph.readback_image("capture linear HDR", color)?,
            graph.readback_image("capture sRGB", output)?,
            graph.readback_image("capture depth", depth)?,
        ])
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
    let draws = (0..scene.primitives.len())
        .filter(|&index| eligible(index))
        .count();
    Ok(Built {
        graph,
        output,
        camera: uniform,
        captures,
        shadow_captures,
        draws,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let options = tiny_output_options();
        let environment = ibl::Environment::fixture();
        let budget = Budget::new(&scene, &environment, &options).unwrap();
        let mut backend = super::super::backend::D3d12::create(&options, &budget).unwrap();
        let resident =
            Resident::upload(&mut backend, &scene, &environment, &budget, &[0; 128]).unwrap();
        drop(resident);
        assert_eq!(backend.close().unwrap()["clean"], true);
    }
}
