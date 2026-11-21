//! Three public composition routes with changing, consumer-owned HDR Compute.
//! Shared by CPU/native acceptance and the standalone full-lifecycle probe.

use super::compute_fixture as compute;
pub use compute::{Expected, HEIGHT, IMAGE_BYTES, PIXELS, READBACKS, Readback, WIDTH, hdr};
use renderer::{passes, unlit_forward};
use renderer_core::graph::{
    Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity,
    GraphPool, ImageDesc, ImageHandle, LoadOp, RasterDraw, RasterParameters, StoreOp,
};
use renderer_core::program::{ManagedProgram, ManagedRasterProgram, SamplerDescriptor};

pub const READBACK_BYTES: usize = READBACKS * IMAGE_BYTES;
pub const BUFFER_SIZES: [u64; 4] = [84, 12, 512, 64];
/// Maximum live resource instances of each image description in one frame.
/// The intentionally unused two-stage branch is culled before native acquisition.
pub const HDR_IMAGES: usize = 11;
pub const DEPTH_IMAGES: usize = 2;
pub const CASES: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostMode {
    Disabled,
    MultiplyThenAdd,
    AddThenMultiply,
    AdditionalAdd,
}

pub fn post_mode(frame: usize) -> PostMode {
    match frame % CASES {
        0 => PostMode::Disabled,
        1 => PostMode::MultiplyThenAdd,
        2 => PostMode::AddThenMultiply,
        _ => PostMode::AdditionalAdd,
    }
}

pub fn draw_count(frame: usize) -> usize {
    [0, 1, 4, 2][frame % CASES]
}

pub const fn programs() -> [ManagedProgram; 3] {
    [
        compute::sampled_program(),
        compute::storage_program(),
        ManagedProgram::uniform_readback(),
    ]
}

pub const fn raster_programs() -> [ManagedRasterProgram; 3] {
    [
        ManagedRasterProgram::mesh_hdr(),
        ManagedRasterProgram::depth_only(),
        ManagedRasterProgram::mesh_hdr_prepassed(),
    ]
}

pub fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 24,
        passes: 32,
        accesses: 128,
        draws: 20,
        raster_constant_words: None,
        bindings: 40,
        attachments: 12,
        dependencies: 64,
        initial_ranges: 48,
        name_bytes: 2048,
        upload_bytes: 1024,
        edges: 512,
        segments: 512,
        readers: 512,
    }
}

fn tint(frame: usize, draw: usize) -> [f32; 4] {
    [
        [2.0, -0.5, 0.25, 1.0],
        [0.5, 4.0, -1.0, 1.0],
        [-0.25, 1.0, 2.0, 0.5],
        [3.0, 0.0, 1.0, 1.0],
    ][(frame / CASES + draw) % 4]
}

fn draws(graph: &mut Graph, frame: usize) -> [RasterDraw; 4] {
    let vertices = graph
        .buffer("standard triangle vertices", BufferDesc { size: 84 })
        .unwrap();
    let indices = graph
        .buffer("standard triangle indices", BufferDesc { size: 12 })
        .unwrap();
    // The clipped oversized triangle covers every pixel with wide margins from
    // its diagonal; all vertices use exactly white so each fragment is its tint.
    let attributes: [f32; 21] = [
        -1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0, 3.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0, -1.0, 3.0, 0.5,
        1.0, 1.0, 1.0, 1.0,
    ];
    let mut bytes = [0; 84];
    for (destination, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(attributes) {
        destination.copy_from_slice(&value.to_le_bytes());
    }
    graph
        .upload_buffer("standard vertex upload", vertices, 0, &bytes)
        .unwrap();
    let mut bytes = [0; 12];
    for (destination, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip([0_u32, 1, 2]) {
        destination.copy_from_slice(&value.to_le_bytes());
    }
    graph
        .upload_buffer("standard index upload", indices, 0, &bytes)
        .unwrap();
    std::array::from_fn(|draw| {
        let mut transform = *RasterParameters::default().transform();
        transform[2][2] = 0.0;
        transform[3][2] = 0.75 - draw as f32 * 0.125;
        transform[3][0] = if frame.is_multiple_of(2) {
            -0.125
        } else {
            0.125
        };
        RasterDraw {
            vertices,
            indices,
            first_index: 0,
            index_count: 3,
            instance_count: 1,
            bindings: None,
            parameters: RasterParameters::new(transform, tint(frame, draw))
                .unwrap()
                .into(),
        }
    })
}

#[derive(Clone, Copy)]
struct PostInputs {
    uniform: BufferHandle,
    offsets: BufferHandle,
    factor_offset: u64,
}

fn post_parameters(frame: usize) -> ([f32; 4], [f32; 4]) {
    let factor = if frame.is_multiple_of(2) {
        [2.0, 0.5, 1.0, 1.0]
    } else {
        [0.5, 2.0, 1.0, 1.0]
    };
    let offset = if (frame / 2).is_multiple_of(2) {
        [0.25, 0.5, 1.0, 0.0]
    } else {
        [0.5, 0.25, -0.5, 0.0]
    };
    (factor, offset)
}

fn post_inputs(graph: &mut Graph, frame: usize) -> PostInputs {
    let uniform = graph
        .buffer("standard post constants", BufferDesc { size: 512 })
        .unwrap();
    let offsets = graph
        .buffer("standard GPU offsets", BufferDesc { size: 64 })
        .unwrap();
    let factor_offset = if frame.is_multiple_of(2) { 0 } else { 256 };
    let offset_offset = 256 - factor_offset;
    let (factor, offset) = post_parameters(frame);
    let mut bytes = [0; 512];
    bytes[factor_offset as usize..factor_offset as usize + 16]
        .copy_from_slice(&compute::vec4_bytes(factor));
    bytes[offset_offset as usize..offset_offset as usize + 16]
        .copy_from_slice(&compute::vec4_bytes(offset));
    graph
        .upload_buffer("standard post parameters", uniform, 0, &bytes)
        .unwrap();
    graph
        .dispatch(
            "standard GPU offset producer",
            &ManagedProgram::uniform_readback(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: uniform,
                        offset: offset_offset,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: offsets,
                        offset: 0,
                        size: 32,
                    },
                },
            ],
        )
        .unwrap();
    PostInputs {
        uniform,
        offsets,
        factor_offset,
    }
}

fn multiply(graph: &mut Graph, source: ImageHandle, inputs: PostInputs) -> ImageHandle {
    let output = graph.image("consumer multiply output", hdr()).unwrap();
    graph
        .dispatch(
            "consumer sampled multiply",
            &compute::sampled_program(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(source, 0, 0).unwrap()),
                },
                Binding {
                    group: 0,
                    binding: 1,
                    resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                },
                Binding {
                    group: 0,
                    binding: 2,
                    resource: BindingResource::Buffer {
                        buffer: inputs.uniform,
                        offset: inputs.factor_offset,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(output, 0, 0).unwrap()),
                },
            ],
        )
        .unwrap();
    output
}

fn add(graph: &mut Graph, source: ImageHandle, inputs: PostInputs) -> ImageHandle {
    let output = graph.image("consumer add output", hdr()).unwrap();
    graph
        .dispatch(
            "consumer storage add",
            &compute::storage_program(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(source, 0, 0).unwrap()),
                },
                Binding {
                    group: 0,
                    binding: 1,
                    resource: BindingResource::Buffer {
                        buffer: inputs.offsets,
                        offset: 0,
                        size: 32,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(output, 0, 0).unwrap()),
                },
            ],
        )
        .unwrap();
    output
}

fn post(graph: &mut Graph, source: ImageHandle, inputs: PostInputs, mode: PostMode) -> ImageHandle {
    match mode {
        PostMode::Disabled => source,
        PostMode::MultiplyThenAdd => {
            let multiplied = multiply(graph, source, inputs);
            add(graph, multiplied, inputs)
        }
        PostMode::AddThenMultiply => {
            let added = add(graph, source, inputs);
            multiply(graph, added, inputs)
        }
        PostMode::AdditionalAdd => {
            let multiplied = multiply(graph, source, inputs);
            let added = add(graph, multiplied, inputs);
            add(graph, added, inputs)
        }
    }
}

pub fn build(pool: &mut GraphPool, frame: usize) -> (Graph, Expected) {
    let mut graph = pool.begin().unwrap();
    let expected = append(&mut graph, frame);
    (graph, expected)
}

pub fn append(graph: &mut Graph, frame: usize) -> Expected {
    let draws = draws(graph, frame);
    let draws = &draws[..draw_count(frame)];
    let settings = unlit_forward::ForwardConfig {
        color: hdr(),
        clear_color: [-0.25, 0.5, 1.0, 1.0],
        depth: false,
    };
    let inputs = post_inputs(graph, frame);
    let direct = unlit_forward::forward(graph, "standard direct route", draws, settings).unwrap();
    let identity =
        unlit_forward::forward(graph, "standard identity route", draws, settings).unwrap();
    let identity_color = post(graph, identity.color, inputs, PostMode::Disabled);
    assert_eq!(identity_color, identity.color);
    let mode = post_mode(frame);
    let scene = unlit_forward::forward(
        graph,
        "standard consumer post route",
        draws,
        unlit_forward::ForwardConfig {
            depth: frame.is_multiple_of(2),
            ..settings
        },
    )
    .unwrap();
    let selected = post(graph, scene.color, inputs, mode);

    // The same frame input and independently assembled pass structure. A depth
    // prepass also checks that equality shading and preserved depth compose with
    // the ordinary consumer Compute module rather than a special standard path.
    let color = graph.image("direct parts color", hdr()).unwrap();
    let depth = graph
        .image("direct parts depth", ImageDesc::depth32(WIDTH, HEIGHT))
        .unwrap();
    let depth_view = graph.image_view(depth, 0, 0).unwrap();
    passes::depth_prepass(
        graph,
        "direct parts prepass",
        draws,
        Attachment {
            view: depth_view,
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        },
    )
    .unwrap();
    passes::opaque(
        graph,
        "direct parts opaque",
        draws,
        Attachment {
            view: graph.image_view(color, 0, 0).unwrap(),
            load: LoadOp::Clear(settings.clear_color),
            store: StoreOp::Store,
        },
        Some(Attachment {
            view: depth_view,
            load: LoadOp::Load,
            store: StoreOp::Store,
        }),
        passes::OpaqueDepth::Prepassed,
    )
    .unwrap();
    let parts = post(graph, color, inputs, mode);
    let output = graph.image("direct parts output", hdr()).unwrap();
    graph
        .copy_image("direct parts output copy", parts, output)
        .unwrap();

    // A valid but unconsumed module must not stay alive merely because the
    // observed post shares its Program and constant-buffer inputs.
    let _unused = post(graph, direct.color, inputs, PostMode::MultiplyThenAdd);

    let base = if draws.is_empty() {
        settings.clear_color
    } else {
        tint(frame, draws.len() - 1)
    };
    let (factor, offset) = post_parameters(frame);
    let processed = std::array::from_fn(|component| match mode {
        PostMode::Disabled => base[component],
        PostMode::MultiplyThenAdd => base[component] * factor[component] + offset[component],
        PostMode::AddThenMultiply => (base[component] + offset[component]) * factor[component],
        PostMode::AdditionalAdd => base[component] * factor[component] + 2.0 * offset[component],
    });
    let names = [
        [
            "direct disabled case",
            "identity disabled case",
            "consumer disabled case",
            "parts disabled case",
        ],
        [
            "direct multiply-add case",
            "identity multiply-add case",
            "consumer multiply-add case",
            "parts multiply-add case",
        ],
        [
            "direct add-multiply case",
            "identity add-multiply case",
            "consumer add-multiply case",
            "parts add-multiply case",
        ],
        [
            "direct additional-add case",
            "identity additional-add case",
            "consumer additional-add case",
            "parts additional-add case",
        ],
    ][frame % CASES];
    let values = [
        (direct.color, base),
        (identity_color, base),
        (selected, processed),
        (output, processed),
    ];
    Expected {
        readbacks: std::array::from_fn(|index| {
            let (image, pixel) = values[index];
            Readback {
                pass: graph.readback_image(names[index], image).unwrap(),
                name: names[index],
                bytes: compute::image_bytes(&[pixel; PIXELS]),
                len: IMAGE_BYTES,
            }
        }),
    }
}
