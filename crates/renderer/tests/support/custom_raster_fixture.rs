//! Consumer-owned graphics assets and a changing, allocation-free composition.
//! Standard recipes, semantic post insertion and direct parts use the same inputs.

use super::compute_fixture as compute;
pub use compute::{Expected, HEIGHT, IMAGE_BYTES, PIXELS, Readback, WIDTH, hdr};
use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity,
    GraphPool, ImageDesc, ImageHandle, LoadOp, RasterAttachments, RasterConstants, RasterDraw,
    RasterParameters, StoreOp,
};
use renderer::program::{
    BindingLayout, BindingType, BlendState, CompareOp, DepthState, ImageFormat,
    ManagedRasterProgram, RasterParameterLayout, RasterProgramDescriptor, SamplerDescriptor,
    ShaderStages, VertexAttribute, VertexFormat,
};
use renderer::standard::{graphs, passes};

pub const CASES: usize = 8;
pub const READBACK_BYTES: usize = IMAGE_BYTES * 3 + PIXELS * 4;
pub const HDR_IMAGES: usize = 6;
pub const DEPTH_IMAGES: usize = 3;

static ALPHA: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 24,
    vertex_attributes: &[
        VertexAttribute {
            location: 0,
            offset: 0,
            format: VertexFormat::Float32x2,
        },
        VertexAttribute {
            location: 1,
            offset: 8,
            format: VertexFormat::Float32x4,
        },
    ],
    parameters: RasterParameterLayout {
        size: 32,
        stages: ShaderStages::VERTEX.union(ShaderStages::FRAGMENT),
        dxil_register: 7,
        dxil_space: 0,
    },
    color_outputs: &[ImageFormat::Rgba16Float],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 2,
            kind: BindingType::Uniform,
            min_size: 16,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 3,
            kind: BindingType::StorageRead,
            min_size: 16,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 2,
            binding: 4,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 2,
            binding: 5,
            kind: BindingType::Sampler,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
    ],
    binding_bytes: &[16, 16, 0, 0],
    blend: BlendState::Alpha,
    depth: DepthState::Required {
        compare: CompareOp::Less,
        write: false,
        require_load: true,
    },
    vertex_spirv: include_bytes!("shaders/custom_raster.vert.spv"),
    fragment_spirv: include_bytes!("shaders/custom_raster.frag.spv"),
    vertex_dxil: include_bytes!("shaders/dxil/custom_raster.vert.dxil"),
    fragment_dxil: include_bytes!("shaders/dxil/custom_raster.frag.dxil"),
};
static PREMULTIPLIED: RasterProgramDescriptor = RasterProgramDescriptor {
    blend: BlendState::PremultipliedAlpha,
    ..ALPHA
};
static ADDITIVE: RasterProgramDescriptor = RasterProgramDescriptor {
    blend: BlendState::Additive,
    ..ALPHA
};
static REPLACE: RasterProgramDescriptor = RasterProgramDescriptor {
    blend: BlendState::Replace,
    ..ALPHA
};
static TOO_MANY: [RasterProgramDescriptor; 65] =
    [const { RasterProgramDescriptor { ..ALPHA } }; 65];

pub fn too_many_programs() -> [ManagedRasterProgram; 65] {
    std::array::from_fn(|index| {
        // SAFETY: Every distinct immutable entry carries the identical reviewed
        // shader/ABI contract as ALPHA; only its immutable asset identity differs.
        unsafe { ManagedRasterProgram::from_trusted_descriptor(&TOO_MANY[index]) }
    })
}

pub const fn custom_program(blend: BlendState) -> ManagedRasterProgram {
    let descriptor = match blend {
        BlendState::Alpha => &ALPHA,
        BlendState::PremultipliedAlpha => &PREMULTIPLIED,
        BlendState::Additive => &ADDITIVE,
        BlendState::Replace => &REPLACE,
    };
    // SAFETY: Reviewed source and pinned generated binaries implement this exact
    // vertex/inline/resource ABI. Indexed fetch uses only the declared stream;
    // fragment reads are bounded 16-byte prefixes and a normalized, clamped LOD0
    // sample. Neither stage has storage writes, hidden resources or side effects.
    // Every permitted inline bit pattern is valid shader input; only ordinary
    // FP32 operations are used. New source/compiler/metadata requires review.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
}

pub const fn raster_programs() -> [ManagedRasterProgram; 5] {
    [
        ManagedRasterProgram::mesh_hdr(),
        custom_program(BlendState::Alpha),
        custom_program(BlendState::PremultipliedAlpha),
        custom_program(BlendState::Additive),
        custom_program(BlendState::Replace),
    ]
}

pub fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 20,
        passes: 24,
        accesses: 128,
        draws: 24,
        bindings: 32,
        attachments: 20,
        dependencies: 64,
        initial_ranges: 40,
        name_bytes: 2048,
        upload_bytes: 1024,
        edges: 512,
        segments: 512,
        readers: 512,
    }
}

pub fn buffer_classes() -> &'static [(u64, usize)] {
    &[(84, 1), (12, 1), (72, 1), (512, 1), (32, 1)]
}

pub fn image_classes() -> Vec<(ImageDesc, usize)> {
    vec![
        (hdr(), HDR_IMAGES),
        (ImageDesc::depth32(WIDTH, HEIGHT), DEPTH_IMAGES),
    ]
}

pub fn draw_count(frame: usize) -> usize {
    [0, 1, 4, 2, 1, 4, 2, 0][frame % CASES]
}

pub fn overlay_count(frame: usize) -> usize {
    [0, 1, 3, 2, 2, 2, 2, 0][frame % CASES]
}

pub fn blend(frame: usize) -> BlendState {
    match frame % CASES {
        3 => BlendState::PremultipliedAlpha,
        4 => BlendState::Additive,
        5 => BlendState::Replace,
        _ => BlendState::Alpha,
    }
}

pub fn base_color(frame: usize) -> [f32; 4] {
    if draw_count(frame) == 0 {
        [0.25, 0.5, 0.75, 1.0]
    } else if (frame / CASES).is_multiple_of(2) {
        [0.5, 0.25, 0.0, 1.0]
    } else {
        [0.25, 0.5, 1.0, 1.0]
    }
}

fn words(values: &[f32]) -> RasterConstants {
    let mut output = [0; 8];
    for (destination, value) in output.iter_mut().zip(values) {
        *destination = value.to_bits();
    }
    RasterConstants::new(&output).unwrap()
}

fn bytes<const N: usize>(values: &[f32]) -> [u8; N] {
    let mut output = [0; N];
    for (destination, value) in output.as_chunks_mut::<4>().0.iter_mut().zip(values) {
        destination.copy_from_slice(&value.to_le_bytes());
    }
    output
}

pub struct Inputs {
    pub opaque: [RasterDraw<'static>; 4],
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    pub bindings: [Binding; 4],
}

pub fn inputs(graph: &mut Graph, frame: usize) -> Inputs {
    let standard_vertices = graph
        .buffer("standard raster vertices", BufferDesc { size: 84 })
        .unwrap();
    let indices = graph
        .buffer("shared raster indices", BufferDesc { size: 12 })
        .unwrap();
    let vertices = graph
        .buffer("consumer vec2 vertices", BufferDesc { size: 72 })
        .unwrap();
    graph
        .upload_buffer(
            "standard vertex bytes",
            standard_vertices,
            0,
            &bytes::<84>(&[
                -1.0, -1.0, 0.75, 1.0, 1.0, 1.0, 1.0, 3.0, -1.0, 0.75, 1.0, 1.0, 1.0, 1.0, -1.0,
                3.0, 0.75, 1.0, 1.0, 1.0, 1.0,
            ]),
        )
        .unwrap();
    graph
        .upload_buffer(
            "consumer vertex bytes",
            vertices,
            0,
            &bytes::<72>(&[
                -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 3.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 3.0, 1.0, 1.0,
                1.0, 1.0,
            ]),
        )
        .unwrap();
    graph
        .upload_buffer(
            "index bytes",
            indices,
            0,
            &[0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0],
        )
        .unwrap();
    let uniform = graph
        .buffer("consumer uniform", BufferDesc { size: 512 })
        .unwrap();
    let storage = graph
        .buffer("consumer readonly storage", BufferDesc { size: 32 })
        .unwrap();
    let uniform_offset = if frame.is_multiple_of(2) { 0 } else { 256 };
    graph
        .upload_buffer(
            "uniform contents",
            uniform,
            uniform_offset,
            &compute::vec4_bytes([1.0, 0.5, 2.0, 1.0]),
        )
        .unwrap();
    graph
        .upload_buffer(
            "storage contents",
            storage,
            16,
            &compute::vec4_bytes([0.25, 0.0, 0.0, 0.0]),
        )
        .unwrap();
    let image = graph.image("consumer sampled texture", hdr()).unwrap();
    graph
        .upload_image(
            "sampled contents",
            image,
            &compute::image_bytes(&[[0.5, 1.0, 0.5, 1.0]; PIXELS]),
        )
        .unwrap();
    // Deliberately reverse manifest order. Captured bindings must survive this
    // stack array and must resolve sparse slots over three groups correctly.
    let bindings = [
        Binding {
            group: 2,
            binding: 5,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
        Binding {
            group: 2,
            binding: 4,
            resource: BindingResource::ImageView(graph.image_view(image, 0, 0).unwrap()),
        },
        Binding {
            group: 1,
            binding: 3,
            resource: BindingResource::Buffer {
                buffer: storage,
                offset: 16,
                size: 16,
            },
        },
        Binding {
            group: 0,
            binding: 2,
            resource: BindingResource::Buffer {
                buffer: uniform,
                offset: uniform_offset,
                size: 16,
            },
        },
    ];
    let opaque = RasterDraw {
        vertices: standard_vertices,
        indices,
        index_count: 3,
        parameters: RasterParameters::new(
            *RasterParameters::default().transform(),
            base_color(frame),
        )
        .unwrap()
        .into(),
        bindings: &[],
    };
    Inputs {
        opaque: [opaque; 4],
        vertices,
        indices,
        bindings,
    }
}

fn color_attachment(
    graph: &Graph,
    image: ImageHandle,
    load: LoadOp<[f32; 4]>,
) -> Attachment<[f32; 4]> {
    Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    }
}

fn depth_attachment(graph: &Graph, image: ImageHandle, load: LoadOp<f32>) -> Attachment<f32> {
    Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    }
}

fn overlay_values(frame: usize, draw: usize) -> (f32, [f32; 4]) {
    let ordered = if frame % CASES == 6 {
        [1, 0, 2]
    } else {
        [0, 1, 2]
    };
    let color = ordered[draw];
    let mut tint = [
        [0.0, 0.0, 1.0, 0.5],
        [1.0, 0.0, 0.0, 0.5],
        [0.0, 1.0, 0.0, 0.5],
    ][color];
    if blend(frame) == BlendState::PremultipliedAlpha {
        // RGB reaching the blend unit is premultiplied for the half-alpha
        // source. Include the red storage contribution in that conversion.
        tint[0] = (tint[0] * 0.5) - 0.125;
        tint[1] *= 0.5;
        tint[2] *= 0.5;
    }
    ([0.5, 0.25, 0.875][draw], tint)
}

pub fn overlay_draws<'a>(inputs: &'a Inputs, frame: usize) -> [RasterDraw<'a>; 3] {
    std::array::from_fn(|draw| {
        let (depth, tint) = overlay_values(frame, draw);
        RasterDraw {
            bindings: &inputs.bindings,
            vertices: inputs.vertices,
            indices: inputs.indices,
            index_count: 3,
            parameters: words(&[0.0, 0.0, depth, 0.0, tint[0], tint[1], tint[2], tint[3]]),
        }
    })
}

pub fn overlay(
    graph: &mut Graph,
    scene: graphs::SceneColor,
    inputs: &Inputs,
    frame: usize,
    name: &str,
) -> ImageHandle {
    let draws = overlay_draws(inputs, frame);
    graph
        .raster(
            name,
            &custom_program(blend(frame)),
            &draws[..overlay_count(frame)],
            RasterAttachments {
                colors: &[color_attachment(graph, scene.color, LoadOp::Load)],
                depth: Some(depth_attachment(graph, scene.depth.unwrap(), LoadOp::Load)),
            },
        )
        .unwrap();
    scene.color
}

/// Independent arithmetic oracle. All values and every intermediate blend
/// result are exactly binary16-representable; no raster edge or interpolation
/// precision is part of the expected result.
pub fn final_color(frame: usize) -> [f32; 4] {
    let mut destination = base_color(frame);
    for draw in 0..overlay_count(frame) {
        let (depth, tint) = overlay_values(frame, draw);
        let stored_depth = if draw_count(frame) == 0 { 1.0 } else { 0.75 };
        if depth >= stored_depth {
            continue;
        }
        let source = [(tint[0] + 0.25) * 0.5, tint[1] * 0.5, tint[2], tint[3]];
        for component in 0..4 {
            destination[component] = match blend(frame) {
                BlendState::Replace => source[component],
                BlendState::Additive => source[component] + destination[component],
                BlendState::PremultipliedAlpha => {
                    source[component] + destination[component] * (1.0 - source[3])
                }
                BlendState::Alpha => {
                    source[component] * if component == 3 { 1.0 } else { source[3] }
                        + destination[component] * (1.0 - source[3])
                }
            };
        }
    }
    destination
}

fn expected_image(
    graph: &mut Graph,
    image: ImageHandle,
    name: &'static str,
    color: [f32; 4],
) -> Readback {
    Readback {
        pass: graph.readback_image(name, image).unwrap(),
        name,
        bytes: compute::image_bytes(&[color; PIXELS]),
        len: IMAGE_BYTES,
    }
}

pub fn build(pool: &mut GraphPool, frame: usize) -> (Graph, Expected) {
    let mut graph = pool.begin().unwrap();
    let expected = append(&mut graph, frame);
    (graph, expected)
}

pub fn append(graph: &mut Graph, frame: usize) -> Expected {
    let inputs = inputs(graph, frame);
    let opaque = &inputs.opaque[..draw_count(frame)];
    let config = graphs::ForwardConfig {
        color: hdr(),
        clear_color: [0.25, 0.5, 0.75, 1.0],
        depth: true,
    };
    let direct = graphs::forward(graph, "standard direct", opaque, config).unwrap();
    let selected = graphs::forward_with_post(
        graph,
        "standard post base",
        opaque,
        config,
        |graph, scene| {
            Ok(overlay(
                graph,
                scene,
                &inputs,
                frame,
                "consumer post raster",
            ))
        },
    )
    .unwrap();
    let color = graph.image("independent color", hdr()).unwrap();
    let depth = graph
        .image("independent depth", ImageDesc::depth32(WIDTH, HEIGHT))
        .unwrap();
    passes::opaque(
        graph,
        "independent standard opaque",
        opaque,
        color_attachment(graph, color, LoadOp::Clear(config.clear_color)),
        Some(depth_attachment(graph, depth, LoadOp::Clear(1.0))),
        passes::OpaqueDepth::Write,
    )
    .unwrap();
    let composed = overlay(
        graph,
        graphs::SceneColor {
            color,
            depth: Some(depth),
        },
        &inputs,
        frame,
        "consumer direct raster",
    );
    let copied = graph.image("standard output target", hdr()).unwrap();
    passes::output(graph, "standard output copy", composed, copied).unwrap();
    let mut depth_bytes = [0; IMAGE_BYTES];
    for component in depth_bytes[..PIXELS * 4].as_chunks_mut::<4>().0 {
        component.copy_from_slice(
            &if draw_count(frame) == 0 {
                1.0_f32
            } else {
                0.75_f32
            }
            .to_le_bytes(),
        );
    }
    Expected {
        readbacks: [
            expected_image(
                graph,
                direct.color,
                "standard direct pixels",
                base_color(frame),
            ),
            expected_image(
                graph,
                selected.color,
                "custom post pixels",
                final_color(frame),
            ),
            expected_image(
                graph,
                copied,
                "custom independent pixels",
                final_color(frame),
            ),
            Readback {
                pass: graph
                    .readback_image("unchanged depth pixels", depth)
                    .unwrap(),
                name: "unchanged depth pixels",
                bytes: depth_bytes,
                len: PIXELS * 4,
            },
        ],
    }
}
