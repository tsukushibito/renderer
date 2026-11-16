//! Consumer-owned Compute assets and an exact, allocation-free HDR workload.
//! This module is shared by public-API acceptance and the standalone memory probe.

use renderer::ImageFormat;
use renderer::graph::{
    Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity, GraphPool, ImageDesc,
    ImageHandle, PassHandle,
};
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, SamplerDescriptor, ShaderStages,
};

pub const WIDTH: u32 = 3;
pub const HEIGHT: u32 = 7;
pub const PIXELS: usize = WIDTH as usize * HEIGHT as usize;
pub const IMAGE_BYTES: usize = PIXELS * 8;
pub const READBACKS: usize = 4;
pub const READBACK_BYTES: usize = IMAGE_BYTES * 2 + 64 + 16;

static SAMPLED: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [4, 4, 1],
    bindings: &[
        slot(0, 0, BindingType::SampledImage, 0),
        slot(0, 1, BindingType::Sampler, 0),
        slot(0, 2, BindingType::Uniform, 16),
        slot(
            1,
            0,
            BindingType::StorageImageWrite(ImageFormat::Rgba16Float),
            0,
        ),
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Image2d {
            group: 1,
            binding: 0,
        },
        bytes: &[0, 0, 16, 0],
    },
    spirv: include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/shaders/compute_hdr_sampled.comp.spv"
    )),
    dxil: include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/shaders/dxil/compute_hdr_sampled.comp.dxil"
    )),
};

static STORAGE: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [4, 4, 1],
    bindings: &[
        slot(
            0,
            0,
            BindingType::StorageImageRead(ImageFormat::Rgba16Float),
            0,
        ),
        slot(0, 1, BindingType::StorageRead, 16),
        slot(
            1,
            0,
            BindingType::StorageImageWrite(ImageFormat::Rgba16Float),
            0,
        ),
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Image2d {
            group: 1,
            binding: 0,
        },
        bytes: &[0, 16, 0],
    },
    spirv: include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/shaders/compute_hdr_storage.comp.spv"
    )),
    dxil: include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/shaders/dxil/compute_hdr_storage.comp.dxil"
    )),
};

const fn slot(group: u32, binding: u32, kind: BindingType, min_size: u64) -> BindingLayout {
    BindingLayout {
        group,
        binding,
        kind,
        stages: ShaderStages::COMPUTE,
        min_size,
    }
}

pub const fn sampled_program() -> ManagedProgram {
    // SAFETY: The checked-in, generated shader and ABI are reviewed together.
    // One guarded invocation writes every output texel exactly once; normalized
    // nearest/clamp sampling is bounded for every accepted source extent, and
    // the sole buffer read is the declared vec4 prefix. No inter-invocation sharing.
    unsafe { ManagedProgram::from_trusted_descriptor(&SAMPLED) }
}

pub const fn storage_program() -> ManagedProgram {
    // SAFETY: The reviewed shader bounds-checks fringe invocations and copies
    // one corresponding input texel plus the declared vec4 prefix into every
    // output texel once. Equal image extents and distinct read/write Views are
    // required by the descriptor/Graph contract. No extra memory or side effects.
    unsafe { ManagedProgram::from_trusted_descriptor(&STORAGE) }
}

pub const fn programs() -> [ManagedProgram; 5] {
    [
        sampled_program(),
        storage_program(),
        ManagedProgram::uniform_readback(),
        ManagedProgram::storage_readback(),
        ManagedProgram::sampled_float_readback(),
    ]
}

pub fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 16,
        passes: 24,
        accesses: 64,
        draws: 0,
        bindings: 32,
        attachments: 0,
        dependencies: 32,
        initial_ranges: 32,
        name_bytes: 1024,
        upload_bytes: 1024,
        edges: 256,
        segments: 256,
        readers: 256,
    }
}

pub fn hdr() -> ImageDesc {
    ImageDesc {
        format: ImageFormat::Rgba16Float,
        ..ImageDesc::rgba8(WIDTH, HEIGHT)
    }
}

#[derive(Clone, Copy)]
pub struct Readback {
    pub pass: PassHandle,
    pub name: &'static str,
    pub bytes: [u8; IMAGE_BYTES],
    pub len: usize,
}

#[derive(Clone, Copy)]
pub struct Expected {
    pub readbacks: [Readback; READBACKS],
}

fn half_exact(value: f32) -> u16 {
    // All fixture arithmetic uses exactly representable binary16 normals or 0.
    // Reject any accidental rounding rather than duplicating shader conversion.
    let bits = value.to_bits();
    let sign = (bits >> 16) as u16 & 0x8000;
    if value == 0.0 {
        return sign;
    }
    let exponent = (bits >> 23) & 0xff;
    assert!((113..143).contains(&exponent));
    assert_eq!(bits & 0x1fff, 0);
    sign | ((exponent - 112) << 10) as u16 | ((bits >> 13) & 0x3ff) as u16
}

pub fn image_bytes(pixels: &[[f32; 4]; PIXELS]) -> [u8; IMAGE_BYTES] {
    let mut bytes = [0; IMAGE_BYTES];
    for (output, pixel) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(pixels) {
        for (component, value) in output.as_chunks_mut::<2>().0.iter_mut().zip(pixel) {
            component.copy_from_slice(&half_exact(*value).to_le_bytes());
        }
    }
    bytes
}

pub fn vec4_bytes(value: [f32; 4]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (component, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(value) {
        component.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn buffer(buffer: BufferHandle, offset: u64, size: u64) -> BindingResource {
    BindingResource::Buffer {
        buffer,
        offset,
        size,
    }
}

fn image(graph: &Graph, image: ImageHandle) -> BindingResource {
    BindingResource::ImageView(graph.image_view(image, 0, 0).unwrap())
}

fn binding(group: u32, binding: u32, resource: BindingResource) -> Binding {
    Binding {
        group,
        binding,
        resource,
    }
}

fn expected_image(pass: PassHandle, name: &'static str, pixels: &[[f32; 4]; PIXELS]) -> Readback {
    Readback {
        pass,
        name,
        bytes: image_bytes(pixels),
        len: IMAGE_BYTES,
    }
}

pub fn build(pool: &mut GraphPool, frame: usize) -> (Graph, Expected) {
    let mut graph = pool.begin().unwrap();
    let source = graph.image("HDR input", hdr()).unwrap();
    let first = graph.image("HDR first", hdr()).unwrap();
    let second = graph.image("HDR second", hdr()).unwrap();
    let copied = graph.image("HDR copy", hdr()).unwrap();
    let uniform = graph.buffer("constants", BufferDesc { size: 512 }).unwrap();
    let offsets = graph
        .buffer("GPU offsets", BufferDesc { size: 64 })
        .unwrap();
    let storage = graph
        .buffer("storage copy", BufferDesc { size: 64 })
        .unwrap();
    let sampled = graph
        .buffer("FP32 samples", BufferDesc { size: 64 })
        .unwrap();

    let palette = [
        [2.0, -0.5, 0.25, 1.0],
        [0.5, 4.0, -1.0, 1.0],
        [3.0, 0.0, 1.0, 0.5],
        [-0.25, 1.0, 2.0, 1.0],
    ];
    let input = std::array::from_fn(|pixel| palette[(pixel + frame / 3) % palette.len()]);
    graph
        .upload_image("upload HDR", source, &image_bytes(&input))
        .unwrap();
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
    let factor_offset = if frame.is_multiple_of(2) { 0 } else { 256 };
    let offset_offset = 256 - factor_offset;
    let mut constants = [0; 512];
    constants[factor_offset as usize..factor_offset as usize + 16]
        .copy_from_slice(&vec4_bytes(factor));
    constants[offset_offset as usize..offset_offset as usize + 16]
        .copy_from_slice(&vec4_bytes(offset));
    graph
        .upload_buffer("upload constants", uniform, 0, &constants)
        .unwrap();
    graph
        .dispatch(
            "uniform to GPU offsets",
            &ManagedProgram::uniform_readback(),
            &[
                binding(0, 0, buffer(uniform, offset_offset, 16)),
                binding(1, 0, buffer(offsets, 0, 32)),
            ],
            [1; 3],
        )
        .unwrap();

    graph
        .dispatch(
            "sample HDR with per-frame uniform",
            &sampled_program(),
            &[
                binding(0, 0, image(&graph, source)),
                binding(
                    0,
                    1,
                    BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                ),
                binding(0, 2, buffer(uniform, factor_offset, 16)),
                binding(1, 0, image(&graph, first)),
            ],
            [1, 2, 1],
        )
        .unwrap();
    let mut pixels: [[f32; 4]; PIXELS] = std::array::from_fn(|pixel| {
        std::array::from_fn(|component| input[pixel][component] * factor[component])
    });
    let initial_readback = graph
        .readback_image("sampled HDR intermediate", first)
        .unwrap();
    let initial = expected_image(initial_readback, "sampled HDR intermediate", &pixels);

    let storage_passes = frame % 3 + 1;
    let mut current = first;
    for pass in 0..storage_passes {
        let destination = if pass.is_multiple_of(2) {
            second
        } else {
            first
        };
        graph
            .dispatch(
                "storage HDR with GPU-produced offset",
                &storage_program(),
                &[
                    binding(0, 0, image(&graph, current)),
                    binding(0, 1, buffer(offsets, 0, 32)),
                    binding(1, 0, image(&graph, destination)),
                ],
                [1, 2, 1],
            )
            .unwrap();
        current = destination;
        for pixel in &mut pixels {
            for (component, offset) in pixel.iter_mut().zip(offset) {
                *component += offset;
            }
        }
    }
    graph.copy_image("copy final HDR", current, copied).unwrap();
    let copied_readback = graph.readback_image("copied HDR", copied).unwrap();
    let final_image = expected_image(copied_readback, "copied HDR", &pixels);

    graph
        .dispatch(
            "sample copied HDR into FP32 buffer",
            &ManagedProgram::sampled_float_readback(),
            &[
                binding(0, 0, image(&graph, copied)),
                binding(
                    0,
                    1,
                    BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                ),
                binding(1, 0, buffer(sampled, 0, 64)),
            ],
            [1; 3],
        )
        .unwrap();
    let sampled_readback = graph
        .readback_buffer("FP32 sample result", sampled, 0, 64)
        .unwrap();
    let mut sample_bytes = [0; IMAGE_BYTES];
    // Independent nearest/clamp coordinates from the reviewed four-sample asset.
    // Height seven keeps its 0.2/0.6 coordinates away from texel boundaries.
    // Height five would place them exactly on rows one/three, making the oracle
    // sensitive to permitted coordinate precision rather than Graph correctness.
    for (destination, pixel) in sample_bytes[..64].as_chunks_mut::<16>().0.iter_mut().zip([
        0,
        WIDTH as usize + 1,
        WIDTH as usize * 4,
        PIXELS - 1,
    ]) {
        destination.copy_from_slice(&vec4_bytes(pixels[pixel]));
    }

    graph
        .dispatch(
            "readonly storage to storage buffer",
            &ManagedProgram::storage_readback(),
            &[
                binding(0, 0, buffer(offsets, 0, 32)),
                binding(1, 0, buffer(storage, 16, 32)),
            ],
            [1; 3],
        )
        .unwrap();
    graph
        .copy_buffer("copy GPU buffer result", storage, 16, offsets, 32, 16)
        .unwrap();
    let buffer_readback = graph
        .readback_buffer("copied GPU buffer", offsets, 32, 16)
        .unwrap();
    let mut buffer_bytes = [0; IMAGE_BYTES];
    buffer_bytes[..16].copy_from_slice(&vec4_bytes(offset));

    (
        graph,
        Expected {
            readbacks: [
                initial,
                final_image,
                Readback {
                    pass: sampled_readback,
                    name: "FP32 sample result",
                    bytes: sample_bytes,
                    len: 64,
                },
                Readback {
                    pass: buffer_readback,
                    name: "copied GPU buffer",
                    bytes: buffer_bytes,
                    len: 16,
                },
            ],
        },
    )
}
