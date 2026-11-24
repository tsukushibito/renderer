//! Cross-stage sampling must wait for the image layout transition.

#![cfg(feature = "vulkan")]

use std::time::Duration;

use renderer_core::graph::{
    Attachment, Binding, BindingResource, BufferDesc, CompiledGraph, Graph, GraphCapacity,
    GraphPool, ImageDesc, ImageHandle, LoadOp, PassHandle, RasterAttachments, RasterConstants,
    RasterDraw, SampleCount, StoreOp,
};
use renderer_core::program::{
    BindingLayout, BindingType, BlendState, CompareOp, DepthState, ManagedProgram,
    ManagedRasterProgram, RasterParameterLayout, RasterProgramDescriptor, RasterShaderStages,
    SampledImageSamplerPair, SamplerDescriptor, ShaderBinaries, ShaderStages, VertexAttribute,
    VertexFormat,
};
use renderer_core::{
    ImageFormat,
    vulkan::{
        FrameCapacity, ResourceClass, RuntimeConfig, Submission, ValidationMode, VulkanRuntime,
    },
};

const WAIT: Duration = Duration::from_secs(10);
const TEXTURE: ImageDesc = ImageDesc {
    width: 2,
    height: 2,
    format: ImageFormat::Rgba8Unorm,
    mip_levels: 1,
    array_layers: 1,
    sample_count: SampleCount::X1,
};
const DEPTH: ImageDesc = ImageDesc {
    width: 4,
    height: 4,
    format: ImageFormat::Depth32Float,
    ..TEXTURE
};
const VERTICES: BufferDesc = BufferDesc { size: 24 };
const INDICES: BufferDesc = BufferDesc { size: 12 };
const SAMPLES: BufferDesc = BufferDesc { size: 16 };
const TEXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
];

static ALPHA_MASK: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 8,
    secondary_vertex_stride: 0,
    vertex_attributes: &[VertexAttribute {
        stream: 0,
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x2,
    }],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
    },
    color_outputs: &[],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            stages: ShaderStages::FRAGMENT,
            min_size: 0,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            stages: ShaderStages::FRAGMENT,
            min_size: 0,
        },
    ],
    binding_bytes: &[0, 0],
    sampling_pairs: &[SampledImageSamplerPair {
        image_group: 0,
        image_binding: 0,
        sampler_group: 0,
        sampler_binding: 1,
    }],
    blend: BlendState::Replace,
    depth: DepthState::Required {
        compare: CompareOp::Less,
        write: true,
        require_load: false,
    },
    shaders: ShaderBinaries::vulkan(RasterShaderStages {
        vertex: include_bytes!("support/shaders/depth_sampling.vert.spv"),
        fragment: Some(include_bytes!(
            "support/shaders/depth_sampling_mask.frag.spv"
        )),
    }),
};

fn sample_bindings(graph: &Graph, texture: ImageHandle) -> [Binding; 2] {
    [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(graph.image_view(texture, 0, 0).unwrap()),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ]
}

fn fragment_sample(
    graph: &mut Graph,
    texture: ImageHandle,
    program: &ManagedRasterProgram,
) -> (PassHandle, PassHandle) {
    let vertices = graph.buffer("vertices", VERTICES).unwrap();
    let indices = graph.buffer("indices", INDICES).unwrap();
    let vertex_bytes = [-1.0_f32, -1.0, 3.0, -1.0, -1.0, 3.0].map(f32::to_le_bytes);
    let index_bytes = [0_u32, 1, 2].map(u32::to_le_bytes);
    graph
        .upload_buffer(
            "triangle vertices",
            vertices,
            0,
            vertex_bytes.as_flattened(),
        )
        .unwrap();
    graph
        .upload_buffer("triangle indices", indices, 0, index_bytes.as_flattened())
        .unwrap();
    let depth = graph.image("masked depth", DEPTH).unwrap();
    let bindings = graph
        .raster_bindings(program, &sample_bindings(graph, texture))
        .unwrap();
    let pass = graph
        .raster(
            "sample in fragment",
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
            |raster| {
                raster.draw(
                    program,
                    [RasterDraw {
                        bindings: Some(bindings),
                        secondary_vertices: None,
                        vertices,
                        indices,
                        first_index: 0,
                        index_count: 3,
                        instance_count: 1,
                        parameters: RasterConstants::new(&[]).unwrap(),
                    }],
                )
            },
        )
        .unwrap();
    let readback = graph.readback_image("fragment result", depth).unwrap();
    (pass, readback)
}

fn compute_sample(graph: &mut Graph, texture: ImageHandle) -> (PassHandle, PassHandle) {
    let samples = graph.buffer("sampled texels", SAMPLES).unwrap();
    let [image, sampler] = sample_bindings(graph, texture);
    let pass = graph
        .dispatch(
            "sample in compute",
            &ManagedProgram::sampled_readback(),
            &[
                image,
                sampler,
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: samples,
                        offset: 0,
                        size: SAMPLES.size,
                    },
                },
            ],
        )
        .unwrap();
    let readback = graph
        .readback_buffer("compute result", samples, 0, SAMPLES.size)
        .unwrap();
    (pass, readback)
}

fn submit_compute(runtime: &mut VulkanRuntime, graph: &CompiledGraph) -> Submission {
    #[cfg(feature = "graph-diagnostics")]
    {
        let (submission, lowering) = runtime.submit_with_diagnostics(graph).unwrap();
        let source = lowering
            .lines()
            .find(|line| line.contains("dst_stage=COMPUTE_SHADER dst_access=SHADER_SAMPLED_READ"))
            .unwrap()
            .split_once(" src_access=TRANSFER_WRITE ")
            .unwrap()
            .0
            .split_once(" src_stage=")
            .unwrap()
            .1;
        assert!(source.contains("COPY") && source.contains("FRAGMENT_SHADER"));
        submission
    }
    #[cfg(not(feature = "graph-diagnostics"))]
    runtime.submit(graph).unwrap()
}

#[test]
#[ignore = "explicit real GPU sampling across fragment/compute stages; run strict and release shipping separately"]
fn vulkan_layout_transition_orders_different_sampled_stages() {
    let strict = cfg!(feature = "vulkan-diagnostics");
    let mut runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 5,
            staging_bytes: 4096,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .unwrap();
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_samplers(&[SamplerDescriptor::NearestClamp])
        .unwrap();
    let graph_capacity = GraphCapacity::default();
    let mut graphs = GraphPool::new(graph_capacity, 2).unwrap();
    runtime
        .prepare_working_set(
            FrameCapacity {
                graph: graph_capacity,
                work_slots: 2,
                result_slots: 2,
                ..Default::default()
            },
            &[
                ResourceClass::Image {
                    desc: TEXTURE,
                    count: 1,
                },
                ResourceClass::Image {
                    desc: DEPTH,
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: VERTICES,
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: INDICES,
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: SAMPLES,
                    count: 1,
                },
            ],
        )
        .unwrap();
    // SAFETY: These reviewed shaders use the declared vertex stream, image and
    // sampler only. The vertex shader produces constant depth. The fragment
    // shader samples normalized coordinates at LOD zero and may discard.
    let mask = unsafe { ManagedRasterProgram::from_trusted_descriptor(&ALPHA_MASK) };
    runtime
        .prepare_raster_programs(std::slice::from_ref(&mask))
        .unwrap();
    runtime
        .prepare_compute_programs(&[ManagedProgram::sampled_readback()])
        .unwrap();

    for split_submit in [false, true] {
        let mut graph = graphs.begin().unwrap();
        let texture = graph.image("uploaded texture", TEXTURE).unwrap();
        graph
            .upload_image("upload texture", texture, &TEXELS)
            .unwrap();
        let (fragment, depth_read) = fragment_sample(&mut graph, texture, &mask);
        let mut depths = [0; 64];
        let mut texels = [0; 16];
        if split_submit {
            graph.export_image(texture).unwrap();
            let mut producer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let exported = producer.export_image(texture).unwrap();
            let mut graph = graphs.begin().unwrap();
            let texture = graph
                .import_image("fragment-sampled texture", &exported)
                .unwrap();
            let (_, compute_read) = compute_sample(&mut graph, texture);
            let mut consumer = submit_compute(&mut runtime, &graph.compile().unwrap());
            // Submit the consumer before any host completion observation.
            consumer
                .wait(WAIT)
                .unwrap()
                .unwrap()
                .readback_into(compute_read, &mut texels)
                .unwrap();
            producer
                .wait(WAIT)
                .unwrap()
                .unwrap()
                .readback_into(depth_read, &mut depths)
                .unwrap();
            runtime.delete(exported.key()).unwrap();
        } else {
            let (compute, compute_read) = compute_sample(&mut graph, texture);
            graph.depends_on(compute, fragment).unwrap();
            let mut submission = submit_compute(&mut runtime, &graph.compile().unwrap());
            let mut result = submission.wait(WAIT).unwrap().unwrap();
            result.readback_into(compute_read, &mut texels).unwrap();
            result.readback_into(depth_read, &mut depths).unwrap();
        }
        assert_eq!(texels, TEXELS, "split submit: {split_submit}");
        assert_eq!(&depths, [0.25_f32.to_le_bytes(); 16].as_flattened());
    }

    drop(runtime);
    let report = monitor.report();
    assert!(
        report.is_clean(),
        "diagnostics through destruction: {report:#?}"
    );
    if strict {
        assert_eq!(report.core_validation, Some(true));
        assert_eq!(report.synchronization_validation, Some(true));
    }
}
