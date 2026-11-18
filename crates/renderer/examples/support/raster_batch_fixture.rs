//! Verify two color outputs and ordered pipeline switches through the public API.

use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, CompiledGraph, GraphCapacity, GraphError,
    GraphPool, ImageDesc, LoadOp, PassHandle, RasterAttachments, RasterConstants, RasterDraw,
    StoreOp,
};
use renderer::program::{
    BindingLayout, BindingType, BlendState, CompareOp, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, ShaderStages, VertexAttribute, VertexFormat,
};
use renderer::{
    D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
    D3D12ValidationMode, FrameCapacity, ImageFormat, ResourceClass, RuntimeConfig, ValidationMode,
    VulkanRuntime,
};
use std::{error::Error, time::Duration};

const WIDTH: u32 = 5;
const HEIGHT: u32 = 3;
const PIXELS: usize = (WIDTH * HEIGHT) as usize;
const WAIT: Duration = Duration::from_secs(30);
const VERTICES: BufferDesc = BufferDesc { size: 36 };
const INDICES: BufferDesc = BufferDesc { size: 12 };
const UNIFORM: BufferDesc = BufferDesc { size: 16 };
const CAPACITY: GraphCapacity = GraphCapacity {
    resources: 8,
    passes: 9,
    accesses: 32,
    draws: 4,
    raster_constant_words: Some(8),
    bindings: 2,
    attachments: 3,
    dependencies: 0,
    initial_ranges: 0,
    name_bytes: 256,
    upload_bytes: 116,
    edges: 32,
    segments: 16,
    readers: 32,
};

static INLINE_COLOR: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 12,
    vertex_attributes: &[VertexAttribute {
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x3,
    }],
    parameters: RasterParameterLayout {
        size: 16,
        stages: ShaderStages::FRAGMENT,
        dxil_register: 7,
        dxil_space: 0,
    },
    color_outputs: &[ImageFormat::Rgba16Float, ImageFormat::Rgba16Float],
    bindings: &[],
    binding_bytes: &[],
    sampling_pairs: &[],
    blend: BlendState::Alpha,
    depth: DepthState::Required {
        compare: CompareOp::LessEqual,
        write: true,
        require_load: false,
    },
    vertex_spirv: include_bytes!("../../tests/support/shaders/raster_batches.vert.spv"),
    vertex_dxil: include_bytes!("../../tests/support/shaders/dxil/raster_batches.vert.dxil"),
    fragment_spirv: include_bytes!("../../tests/support/shaders/raster_batches_inline.frag.spv"),
    fragment_dxil: include_bytes!(
        "../../tests/support/shaders/dxil/raster_batches_inline.frag.dxil"
    ),
};

static UNIFORM_COLOR: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
        dxil_register: 0,
        dxil_space: 0,
    },
    bindings: &[BindingLayout {
        group: 0,
        binding: 2,
        kind: BindingType::Uniform,
        min_size: UNIFORM.size,
        stages: ShaderStages::FRAGMENT,
    }],
    binding_bytes: &[UNIFORM.size],
    depth: DepthState::Required {
        compare: CompareOp::LessEqual,
        write: false,
        require_load: false,
    },
    fragment_spirv: include_bytes!("../../tests/support/shaders/raster_batches_uniform.frag.spv"),
    fragment_dxil: include_bytes!(
        "../../tests/support/shaders/dxil/raster_batches_uniform.frag.dxil"
    ),
    ..INLINE_COLOR
};

static OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[ImageFormat::Rgba8Unorm, ImageFormat::Rgba16Float],
    blend: BlendState::Replace,
    ..INLINE_COLOR
};

fn programs() -> [ManagedRasterProgram; 3] {
    [&INLINE_COLOR, &UNIFORM_COLOR, &OPAQUE].map(|descriptor| {
        // SAFETY: Both pinned vertex binaries read only the declared vec3 input.
        // Fragment binaries read only the four inline words or the exact 16-byte
        // uniform prefix, and write two float4 outputs without other effects.
        unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
    })
}

#[derive(Clone, Copy)]
enum Scene {
    Opaque,
    Ordered,
}

struct Capture {
    pass: PassHandle,
    pixel: &'static [u8],
}

fn build(
    pool: &mut GraphPool,
    [inline, uniform, opaque]: [ManagedRasterProgram; 3],
    scene: Scene,
) -> Result<(CompiledGraph, [Capture; 3]), GraphError> {
    let mut graph = pool.begin()?;
    let indices = graph.buffer("indices", INDICES)?;
    graph.upload_buffer(
        "triangle indices",
        indices,
        0,
        &[0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0],
    )?;
    let front = graph.buffer("front triangle", VERTICES)?;
    let triangle = |z| [-1.0, -1.0, z, 3.0, -1.0, z, -1.0, 3.0, z].map(f32::to_le_bytes);
    graph.upload_buffer("front vertices", front, 0, triangle(0.5).as_flattened())?;

    let hdr = ImageDesc::rgba16_float(WIDTH, HEIGHT);
    let first = graph.image(
        "first target",
        match scene {
            Scene::Opaque => ImageDesc::rgba8(WIDTH, HEIGHT),
            Scene::Ordered => hdr,
        },
    )?;
    let second = graph.image("second target", hdr)?;
    let depth = graph.image("depth", ImageDesc::depth32(WIDTH, HEIGHT))?;
    let targets = RasterAttachments {
        colors: &[
            Attachment {
                view: graph.image_view(first, 0, 0)?,
                load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                store: StoreOp::Store,
            },
            Attachment {
                view: graph.image_view(second, 0, 0)?,
                load: LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
                store: StoreOp::Store,
            },
        ],
        depth: Some(Attachment {
            view: graph.image_view(depth, 0, 0)?,
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        }),
        color_resolves: &[],
    };
    let draw = RasterDraw {
        bindings: None,
        vertices: front,
        indices,
        first_index: 0,
        index_count: 3,
        instance_count: 1,
        parameters: RasterConstants::new(&[1.0_f32.to_bits(), 0, 0, 1.0_f32.to_bits()])?,
    };

    let first_pixel: &'static [u8];
    let second_pixel: &'static [u8];
    match scene {
        Scene::Opaque => {
            graph.raster("mixed-format MRT", targets, |raster| {
                raster.draw(&opaque, [draw])
            })?;
            first_pixel = &[255, 0, 0, 255];
            second_pixel = &[0, 0, 0, 0x3c, 0, 0, 0, 0x3c];
        }
        Scene::Ordered => {
            let far = graph.buffer("occluded triangle", VERTICES)?;
            graph.upload_buffer("far vertices", far, 0, triangle(0.75).as_flattened())?;
            let uniform_color = graph.buffer("uniform color", UNIFORM)?;
            graph.upload_buffer(
                "uniform bytes",
                uniform_color,
                0,
                [0.0, 1.0, 0.0, 0.5].map(f32::to_le_bytes).as_flattened(),
            )?;
            let binding = graph.raster_bindings(
                &uniform,
                &[Binding {
                    group: 0,
                    binding: 2,
                    resource: BindingResource::Buffer {
                        buffer: uniform_color,
                        offset: 0,
                        size: UNIFORM.size,
                    },
                }],
            )?;
            let blend_green = RasterDraw {
                bindings: Some(binding),
                parameters: RasterConstants::default(),
                ..draw
            };
            let alpha = 0.5_f32.to_bits();
            let one = 1.0_f32.to_bits();
            graph.raster("inline, uniform, inline", targets, |raster| {
                raster.draw(
                    &inline,
                    [RasterDraw {
                        parameters: RasterConstants::new(&[one, 0, 0, alpha])?,
                        ..draw
                    }],
                )?;
                raster.draw(
                    &uniform,
                    [
                        RasterDraw {
                            vertices: far,
                            ..blend_green
                        },
                        blend_green,
                    ],
                )?;
                raster.draw(
                    &inline,
                    [RasterDraw {
                        parameters: RasterConstants::new(&[0, 0, one, alpha])?,
                        ..draw
                    }],
                )
            })?;
            // Over black: red, green, then blue at alpha 1/2.
            first_pixel = &[0, 0x30, 0, 0x34, 0, 0x38, 0, 0x3c];
            // Over red: green, blue, then red at alpha 1/2.
            second_pixel = &[0, 0x39, 0, 0x30, 0, 0x34, 0, 0x3c];
        }
    }

    let captures = [
        Capture {
            pass: graph.readback_image("read first target", first)?,
            pixel: first_pixel,
        },
        Capture {
            pass: graph.readback_image("read second target", second)?,
            pixel: second_pixel,
        },
        Capture {
            pass: graph.readback_image("read depth", depth)?,
            pixel: &[0, 0, 0, 0x3f],
        },
    ];
    Ok((graph.compile()?, captures))
}

fn compare(captures: &[Capture], mut read: impl FnMut(PassHandle, &mut [u8])) {
    for Capture { pass, pixel } in captures {
        let mut output = [0; PIXELS * 8];
        let bytes = &mut output[..PIXELS * pixel.len()];
        read(*pass, bytes);
        for (index, actual) in bytes.chunks_exact(pixel.len()).enumerate() {
            assert_eq!(actual, *pixel, "capture {pass:?}, pixel {index}");
        }
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let strict = cfg!(feature = "development-validation");
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 1,
            resource_capacity: 8,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )?;
    let buffers = [(INDICES, 1), (VERTICES, 2), (UNIFORM, 1)];
    let images = [
        (ImageDesc::rgba8(WIDTH, HEIGHT), 1),
        (ImageDesc::rgba16_float(WIDTH, HEIGHT), 2),
        (ImageDesc::depth32(WIDTH, HEIGHT), 1),
    ];
    let programs = programs();
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
                operations: 32,
                buffers: 4,
                upload_bytes: 4096,
                readback_bytes: 8192,
                ..Default::default()
            },
        },
        &buffers.map(|(desc, count)| D3D12BufferClass {
            size: desc.size,
            count,
        }),
        &images.map(|(desc, count)| D3D12ImageClass { desc, count }),
        &programs.map(Into::into),
        &[],
    )?;
    let adapter = vk.device_info();
    assert_ne!(adapter.device_type, "CPU");
    assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
    let vk_monitor = vk.validation_monitor();
    let dx_monitor = dx.validation_monitor();
    let resources: Vec<_> = buffers
        .into_iter()
        .map(|(desc, count)| ResourceClass::Buffer { desc, count })
        .chain(
            images
                .into_iter()
                .map(|(desc, count)| ResourceClass::Image { desc, count }),
        )
        .collect();
    vk.prepare_working_set(
        FrameCapacity {
            graph: CAPACITY,
            work_slots: 1,
            ..Default::default()
        },
        &resources,
    )?;
    vk.prepare_raster_programs(&programs)?;
    let mut pool = GraphPool::new(CAPACITY, 1)?;
    for scene in [Scene::Opaque, Scene::Ordered] {
        let (plan, captures) = build(&mut pool, programs, scene)?;
        let raster_name = match scene {
            Scene::Opaque => "mixed-format MRT",
            Scene::Ordered => "inline, uniform, inline",
        };
        assert_eq!(
            plan.ordered_pass_names()
                .filter(|name| *name == raster_name)
                .count(),
            1
        );
        let mut vk_pending = vk.submit(&plan)?;
        let mut vk_result = vk_pending.wait(WAIT)?.expect("Vulkan completion");
        compare(&captures, |pass, bytes| {
            vk_result.readback_into(pass, bytes).unwrap()
        });
        drop((vk_result, vk_pending));
        let mut dx_pending = dx.submit(&plan)?;
        drop(plan);
        let mut dx_result = dx_pending.wait(WAIT)?.expect("D3D12 completion");
        compare(&captures, |pass, bytes| {
            dx_result.readback_into(pass, bytes).unwrap()
        });
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
        "MRT and ordered Raster programs verified on {} (strict={strict})",
        adapter.name
    );
    Ok(())
}
