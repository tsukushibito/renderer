//! Verify absolute indices and two shader-visible instance IDs on both APIs.

include!("shader_binaries.rs");

use renderer_core::graph::{
    Attachment, Binding, BindingResource, BufferDesc, CompiledGraph, GraphCapacity, GraphError,
    GraphPool, ImageDesc, LoadOp, PassHandle, RasterAttachments, RasterConstants, RasterDraw,
    StoreOp,
};
use renderer_core::program::{
    BindingLayout, BindingType, BlendState, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, ShaderStages, VertexAttribute, VertexFormat,
};
use renderer_core::{
    ImageFormat,
    d3d12::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12ValidationMode,
    },
    vulkan::{FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode, VulkanRuntime},
};
use std::{error::Error, time::Duration};

const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;
const BYTES: usize = (WIDTH * HEIGHT * 4) as usize;
const WAIT: Duration = Duration::from_secs(30);
const POSITIONS: [f32; 14] = [
    2.0, -1.0, 3.0, -1.0, 2.0, 1.0, // A triangle outside the view.
    -1.0, -1.0, 0.0, -1.0, -1.0, 1.0, 0.0, 1.0, // The left half of the view.
];
const INDICES: [u32; 9] = [0, 1, 2, 3, 4, 5, 5, 4, 6];
const INSTANCE_DATA: [f32; 8] = [0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0];
const VERTICES: BufferDesc = BufferDesc {
    size: size_of_val(&POSITIONS) as u64,
};
const INDEX_BUFFER: BufferDesc = BufferDesc {
    size: size_of_val(&INDICES) as u64,
};
const INSTANCES: BufferDesc = BufferDesc {
    size: size_of_val(&INSTANCE_DATA) as u64,
};
const CAPACITY: GraphCapacity = GraphCapacity {
    resources: 4,
    passes: 5,
    accesses: 16,
    draws: 2,
    raster_constant_words: None,
    bindings: 1,
    attachments: 1,
    dependencies: 0,
    initial_ranges: 0,
    name_bytes: 256,
    upload_bytes: 128,
    edges: 16,
    segments: 16,
    readers: 16,
};

static SHADER: RasterProgramDescriptor = RasterProgramDescriptor {
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
        size: 4,
        stages: ShaderStages::VERTEX,
    },
    color_outputs: &[ImageFormat::Rgba8Unorm],
    bindings: &[BindingLayout {
        group: 0,
        binding: 2,
        kind: BindingType::StorageRead,
        min_size: INSTANCES.size,
        stages: ShaderStages::VERTEX,
    }],
    binding_bytes: &[INSTANCES.size],
    sampling_pairs: &[],
    blend: BlendState::Replace,
    depth: DepthState::Disabled,
    shaders: fixture_shaders!(
        renderer_core::program::RasterShaderStages {
            vertex: include_bytes!("../tests/support/shaders/indexed_instancing.vert.spv"),
            fragment: Some(include_bytes!(
                "../tests/support/shaders/indexed_instancing.frag.spv"
            )),
        },
        renderer_core::program::D3D12RasterShaders {
            stages: renderer_core::program::RasterShaderStages {
                vertex: include_bytes!(
                    "../tests/support/shaders/dxil/indexed_instancing.vert.dxil"
                ),
                fragment: Some(include_bytes!(
                    "../tests/support/shaders/dxil/indexed_instancing.frag.dxil"
                )),
            },
            inline_constants: Some(renderer_core::program::D3D12InlineConstantBinding {
                register: 7,
                space: 0,
            }),
        },
    ),
};

fn program() -> ManagedRasterProgram {
    // SAFETY: Both pinned vertex binaries access only the declared position,
    // inline word and two vec4 data records. The record index is clamped to 0..=1
    // regardless of draw count and inline value. The fragment binaries write
    // exactly one color output and use no descriptors or side effects.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&SHADER) }
}

#[derive(Clone, Copy, Debug)]
enum DrawMode {
    Separate,
    Instanced,
}

fn build(
    pool: &mut GraphPool,
    program: &ManagedRasterProgram,
    mode: DrawMode,
) -> Result<(CompiledGraph, PassHandle), GraphError> {
    let mut graph = pool.begin()?;
    let vertices = graph.buffer("shared vertices", VERTICES)?;
    let indices = graph.buffer("shared indices", INDEX_BUFFER)?;
    let records = graph.buffer("instance records", INSTANCES)?;
    graph.upload_buffer(
        "positions",
        vertices,
        0,
        POSITIONS.map(f32::to_le_bytes).as_flattened(),
    )?;
    graph.upload_buffer(
        "indices",
        indices,
        0,
        INDICES.map(u32::to_le_bytes).as_flattened(),
    )?;
    graph.upload_buffer(
        "records",
        records,
        0,
        INSTANCE_DATA.map(f32::to_le_bytes).as_flattened(),
    )?;

    let bindings = graph.raster_bindings(
        program,
        &[Binding {
            group: 0,
            binding: 2,
            resource: BindingResource::Buffer {
                buffer: records,
                offset: 0,
                size: INSTANCES.size,
            },
        }],
    )?;
    let color = graph.image("output", ImageDesc::rgba8(WIDTH, HEIGHT))?;
    let targets = RasterAttachments {
        colors: &[Attachment {
            view: graph.image_view(color, 0, 0)?,
            load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
            store: StoreOp::Store,
        }],
        depth: None,
        color_resolves: &[],
    };
    let draw = RasterDraw {
        secondary_vertices: None,
        vertices,
        indices,
        first_index: 3,
        index_count: 6,
        instance_count: 1,
        parameters: RasterConstants::new(&[0])?,
        bindings: Some(bindings),
    };
    graph.raster("ranged indexed draws", targets, |raster| match mode {
        DrawMode::Separate => raster.draw(
            program,
            [
                draw,
                RasterDraw {
                    parameters: RasterConstants::new(&[1])?,
                    ..draw
                },
            ],
        ),
        DrawMode::Instanced => raster.draw(
            program,
            [RasterDraw {
                instance_count: 2,
                ..draw
            }],
        ),
    })?;
    let readback = graph.readback_image("pixels", color)?;
    Ok((graph.compile()?, readback))
}

fn verify(bytes: &[u8; BYTES], mode: DrawMode, api: &str) {
    for (index, pixel) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let x = index % WIDTH as usize;
        let expected = if x < WIDTH as usize / 2 {
            [255, 0, 0, 255]
        } else {
            [0, 255, 0, 255]
        };
        assert_eq!(*pixel, expected, "{api} {mode:?}, pixel {index}");
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let strict = cfg!(feature = "development-validation");
    let shader = program();
    let buffers = [VERTICES, INDEX_BUFFER, INSTANCES];
    let color = ImageDesc::rgba8(WIDTH, HEIGHT);
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 1,
            resource_capacity: 4,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )?;
    vk.prepare_working_set(
        FrameCapacity {
            graph: CAPACITY,
            work_slots: 1,
            ..Default::default()
        },
        &[
            ResourceClass::Buffer {
                desc: VERTICES,
                count: 1,
            },
            ResourceClass::Buffer {
                desc: INDEX_BUFFER,
                count: 1,
            },
            ResourceClass::Buffer {
                desc: INSTANCES,
                count: 1,
            },
            ResourceClass::Image {
                desc: color,
                count: 1,
            },
        ],
    )?;
    vk.prepare_raster_programs(std::slice::from_ref(&shader))?;
    let mut dx = D3D12Runtime::new_with_working_set(
        D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                resources: CAPACITY.resources,
                operations: 16,
                buffers: 3,
                upload_bytes: 4096,
                readback_bytes: 4096,
                ..Default::default()
            },
        },
        &buffers.map(|desc| D3D12BufferClass {
            size: desc.size,
            count: 1,
        }),
        &[D3D12ImageClass {
            desc: color,
            count: 1,
        }],
        &[shader.clone().into()],
        &[],
    )?;
    let adapter = vk.device_info();
    assert_ne!(adapter.device_type, "CPU");
    assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
    let vk_monitor = vk.validation_monitor();
    let dx_monitor = dx.validation_monitor();
    let mut pool = GraphPool::new(CAPACITY, 1)?;
    for mode in [DrawMode::Separate, DrawMode::Instanced] {
        let (plan, output) = build(&mut pool, &shader, mode)?;
        let mut bytes = [0; BYTES];
        let mut vk_pending = vk.submit(&plan)?;
        let mut vk_result = vk_pending.wait(WAIT)?.expect("Vulkan completion");
        vk_result.readback_into(output, &mut bytes)?;
        verify(&bytes, mode, "Vulkan");
        drop((vk_result, vk_pending));

        let mut dx_pending = dx.submit(&plan)?;
        drop(plan);
        let mut dx_result = dx_pending.wait(WAIT)?.expect("D3D12 completion");
        dx_result.readback_into(output, &mut bytes)?;
        verify(&bytes, mode, "D3D12");
        drop((dx_result, dx_pending));
    }
    assert!(pool.is_idle());
    drop(vk);
    dx.shutdown()?;
    let vk_report = vk_monitor.report();
    let dx_report = dx_monitor.snapshot();
    assert!(vk_report.is_clean(), "{vk_report:?}");
    assert!(dx_report.is_clean(), "{dx_report:?}");
    if strict {
        assert_eq!(vk_report.core_validation, Some(true));
        assert_eq!(vk_report.synchronization_validation, Some(true));
        assert!(dx_report.debug_layer_enabled && dx_report.gpu_based_validation_enabled);
        assert!(dx_report.teardown_checked);
    } else {
        assert_eq!(vk_report.requested, ValidationMode::Disabled);
        assert!(!dx_report.debug_layer_enabled && !dx_report.gpu_based_validation_enabled);
    }
    println!(
        "Indexed instancing verified on {} (strict={strict})",
        adapter.name
    );
    Ok(())
}
