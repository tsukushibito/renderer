use renderer::ImageFormat;
use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, BufferHandle, GraphCapacity,
    GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc, LoadOp, RasterAttachments,
    RasterConstants, RasterDraw, SampleCount, StoreOp,
};
use renderer::program::{
    BindingLayout, BindingType, BlendState, D3D12RasterShaders, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, RasterShaderStages, ShaderBinaries,
    ShaderStages,
};

const COLOR: BufferDesc = BufferDesc { size: 16 };
const TARGET: ImageDesc = ImageDesc {
    width: 7,
    height: 5,
    format: ImageFormat::Rgba8Unorm,
    mip_levels: 1,
    array_layers: 1,
    sample_count: SampleCount::X1,
};
const BACKGROUND: [f32; 4] = [0.25, 0.0, 0.0, 0.0];

static FULLSCREEN: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 0,
    vertex_attributes: &[],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
    },
    color_outputs: &[ImageFormat::Rgba8Unorm],
    bindings: &[BindingLayout {
        group: 0,
        binding: 0,
        kind: BindingType::Uniform,
        stages: ShaderStages::VERTEX,
        min_size: COLOR.size,
    }],
    binding_bytes: &[COLOR.size],
    sampling_pairs: &[],
    blend: BlendState::Additive,
    depth: DepthState::Disabled,
    shaders: ShaderBinaries::both(
        RasterShaderStages {
            vertex: include_bytes!("support/shaders/fullscreen_uniform.vert.spv"),
            fragment: Some(include_bytes!("../shaders/mesh.frag.spv")),
        },
        D3D12RasterShaders {
            stages: RasterShaderStages {
                vertex: include_bytes!("support/shaders/dxil/fullscreen_uniform.vert.dxil"),
                fragment: Some(include_bytes!("../shaders/dxil/mesh.frag.dxil")),
            },
            inline_constants: None,
        },
    ),
};

fn program() -> ManagedRasterProgram {
    // SAFETY: The vertex shader uses IDs 0..=2 to compute positions, reads a
    // single vec4 at uniform (0, 0), and passes that value at location 0 to
    // mesh.frag. The generated binaries agree with this interface, with no
    // other access or side effects. Color values do not affect memory accesses.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&FULLSCREEN) }
}

fn uniform(buffer: BufferHandle) -> Binding {
    Binding {
        group: 0,
        binding: 0,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size: COLOR.size,
        },
    }
}

#[test]
fn fullscreen_owns_raw_bindings_and_returns_capacity_after_failed_passes() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 1,
            draws: 1,
            attachments: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let color = graph.buffer("scene color", COLOR).unwrap();
    graph
        .upload_buffer(
            "color producer",
            color,
            0,
            BACKGROUND.map(f32::to_le_bytes).as_flattened(),
        )
        .unwrap();
    let target = graph.image("fullscreen target", TARGET).unwrap();
    let attachments = RasterAttachments {
        colors: &[Attachment {
            view: graph.image_view(target, 0, 0).unwrap(),
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        }],
        color_resolves: &[],
        depth: None,
    };
    let fullscreen = program();
    let constants = RasterConstants::default();
    for error in [
        graph.raster("indexed frame program", attachments, |raster| {
            raster.draw_fullscreen(&ManagedRasterProgram::mesh(), &[], constants)
        }),
        graph.raster("fullscreen indexed program", attachments, |raster| {
            raster.draw(&fullscreen, std::iter::empty::<RasterDraw>())
        }),
    ] {
        assert_eq!(error.unwrap_err().kind(), GraphErrorKind::InvalidInput);
    }
    for _ in 0..3 {
        let error = graph
            .raster("aborted fullscreen", attachments, |raster| {
                raster.draw_fullscreen(&fullscreen, &[uniform(color)], constants)?;
                raster.draw_fullscreen(&fullscreen, &[uniform(color)], constants)
            })
            .unwrap_err();
        assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Draws);
    }
    graph
        .raster("fullscreen", attachments, |raster| {
            raster.draw_fullscreen(&fullscreen, &[uniform(color)], constants)
        })
        .unwrap();
    graph.readback_image("pixels", target).unwrap();
    let plan = graph.compile().unwrap();
    assert!(
        plan.ordered_pass_names()
            .eq(["color producer", "fullscreen", "pixels"])
    );
    drop(plan);
    assert!(pool.is_idle());
}

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
mod native {
    use super::*;
    use renderer::graph::{CompiledGraph, PassHandle, RasterParameters};
    use renderer::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode,
        VulkanRuntime,
    };
    use std::path::Path;
    use std::time::Duration;

    const VERTICES: BufferDesc = BufferDesc { size: 84 };
    const INDICES: BufferDesc = BufferDesc { size: 12 };
    const OVERLAY: [f32; 4] = [0.0, 0.0, 0.25, 1.0];
    const PIXEL_BYTES: usize = TARGET.width as usize * TARGET.height as usize * 4;
    const WAIT: Duration = Duration::from_secs(30);

    fn load_fullscreen(vulkan: bool) -> ManagedRasterProgram {
        let load = |path: &str, reviewed: &[u8]| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
            let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
            assert_eq!(bytes.as_slice(), reviewed, "unreviewed fixture: {path:?}");
            bytes
        };
        let shaders = if vulkan {
            ShaderBinaries::vulkan(RasterShaderStages {
                vertex: load(
                    "tests/support/shaders/fullscreen_uniform.vert.spv",
                    include_bytes!("support/shaders/fullscreen_uniform.vert.spv"),
                ),
                fragment: Some(load(
                    "shaders/mesh.frag.spv",
                    include_bytes!("../shaders/mesh.frag.spv"),
                )),
            })
        } else {
            ShaderBinaries::d3d12(D3D12RasterShaders {
                stages: RasterShaderStages {
                    vertex: load(
                        "tests/support/shaders/dxil/fullscreen_uniform.vert.dxil",
                        include_bytes!("support/shaders/dxil/fullscreen_uniform.vert.dxil"),
                    ),
                    fragment: Some(load(
                        "shaders/dxil/mesh.frag.dxil",
                        include_bytes!("../shaders/dxil/mesh.frag.dxil"),
                    )),
                },
                inline_constants: None,
            })
        };
        let entry = FULLSCREEN.entry.to_string();
        let bindings = FULLSCREEN.bindings.to_vec();
        // SAFETY: The loaded files were compared with the reviewed fullscreen
        // fixture bytecode. The descriptor supplies that same resource and stage
        // contract, including the vertex-stage vec4 and no other accesses.
        unsafe {
            ManagedRasterProgram::from_trusted_owned_descriptor(RasterProgramDescriptor {
                entry: &entry,
                vertex_stride: FULLSCREEN.vertex_stride,
                vertex_attributes: FULLSCREEN.vertex_attributes,
                parameters: FULLSCREEN.parameters,
                color_outputs: FULLSCREEN.color_outputs,
                bindings: &bindings,
                binding_bytes: FULLSCREEN.binding_bytes,
                sampling_pairs: FULLSCREEN.sampling_pairs,
                blend: FULLSCREEN.blend,
                depth: FULLSCREEN.depth,
                shaders,
            })
        }
        .unwrap()
    }

    fn build(pool: &mut GraphPool, program: ManagedRasterProgram) -> (CompiledGraph, PassHandle) {
        let mut graph = pool.begin().unwrap();
        let colors = [BACKGROUND, OVERLAY].map(|value| {
            let buffer = graph.buffer("generated triangle color", COLOR).unwrap();
            graph
                .upload_buffer(
                    "upload generated triangle color",
                    buffer,
                    0,
                    value.map(f32::to_le_bytes).as_flattened(),
                )
                .unwrap();
            uniform(buffer)
        });
        let vertices = graph.buffer("indexed vertices", VERTICES).unwrap();
        let indices = graph.buffer("indexed indices", INDICES).unwrap();
        let vertex_bytes = [[-1.0f32, -1.0], [0.0, -1.0], [-1.0, 0.0]]
            .map(|[x, y]| [x, y, 0.5, 0.0, 1.0, 0.0, 1.0].map(f32::to_le_bytes));
        graph
            .upload_buffer(
                "upload indexed vertices",
                vertices,
                0,
                vertex_bytes.as_flattened().as_flattened(),
            )
            .unwrap();
        graph
            .upload_buffer(
                "upload indexed indices",
                indices,
                0,
                [0u32, 1, 2].map(u32::to_le_bytes).as_flattened(),
            )
            .unwrap();
        let target = graph.image("mixed Raster target", TARGET).unwrap();
        let view = graph.image_view(target, 0, 0).unwrap();
        graph
            .raster(
                "generated, indexed, generated",
                RasterAttachments {
                    colors: &[Attachment {
                        view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: None,
                },
                |raster| {
                    let constants = RasterConstants::default();
                    raster.draw_fullscreen(&program, &colors[..1], constants)?;
                    raster.draw(
                        &ManagedRasterProgram::mesh(),
                        [RasterDraw {
                            bindings: None,
                            vertices,
                            indices,
                            first_index: 0,
                            index_count: 3,
                            instance_count: 1,
                            parameters: RasterParameters::default().into(),
                        }],
                    )?;
                    raster.draw_fullscreen(&program, &colors[1..], constants)
                },
            )
            .unwrap();
        let readback = graph.readback_image("mixed Raster pixels", target).unwrap();
        drop(program);
        (graph.compile().unwrap(), readback)
    }

    #[test]
    #[ignore = "requires one physical adapter available to Vulkan and D3D12"]
    fn mixed_fullscreen_and_indexed_order() {
        run_mixed_fullscreen_and_indexed_order(program(), None);
    }

    #[test]
    #[ignore = "requires one physical adapter available to Vulkan and D3D12"]
    fn runtime_loaded_raster_preserves_mixed_draw_order() {
        run_mixed_fullscreen_and_indexed_order(load_fullscreen(true), Some(load_fullscreen(false)));
    }

    fn run_mixed_fullscreen_and_indexed_order(
        vk_program: ManagedRasterProgram,
        dx_program: Option<ManagedRasterProgram>,
    ) {
        let strict = cfg!(feature = "development-validation");
        let capacity = GraphCapacity::default();
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
        )
        .unwrap();
        vk.prepare_working_set(
            FrameCapacity {
                graph: capacity,
                work_slots: 1,
                ..Default::default()
            },
            &[
                ResourceClass::Buffer {
                    desc: COLOR,
                    count: 2,
                },
                ResourceClass::Buffer {
                    desc: VERTICES,
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: INDICES,
                    count: 1,
                },
                ResourceClass::Image {
                    desc: TARGET,
                    count: 1,
                },
            ],
        )
        .unwrap();
        vk.prepare_raster_programs(&[vk_program.clone(), ManagedRasterProgram::mesh()])
            .unwrap();

        let mut dx = D3D12Runtime::new_with_working_set(
            D3D12RuntimeConfig {
                validation: if strict {
                    D3D12ValidationMode::Required
                } else {
                    D3D12ValidationMode::Disabled
                },
                capacity: D3D12FrameCapacity {
                    executions: 1,
                    results: 1,
                    resources: 8,
                    resource_capacity: 8,
                    operations: 16,
                    buffers: 4,
                    buffer_bytes: 256,
                    upload_bytes: 1024,
                    readback_bytes: 4096,
                    ..Default::default()
                },
            },
            &[
                D3D12BufferClass {
                    size: COLOR.size,
                    count: 2,
                },
                D3D12BufferClass {
                    size: VERTICES.size,
                    count: 1,
                },
                D3D12BufferClass {
                    size: INDICES.size,
                    count: 1,
                },
            ],
            &[D3D12ImageClass {
                desc: TARGET,
                count: 1,
            }],
            &[
                dx_program.as_ref().unwrap_or(&vk_program).clone().into(),
                ManagedRasterProgram::mesh().into(),
            ],
            &[],
        )
        .unwrap();
        let adapter = vk.device_info();
        assert_ne!(adapter.device_type, "CPU");
        assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
        let vk_monitor = vk.validation_monitor();
        let mut pool = GraphPool::new(capacity, 2).unwrap();
        let (vk_plan, vk_readback) = build(&mut pool, vk_program);
        let dx_recorded = dx_program.map(|program| build(&mut pool, program));
        let (dx_plan, dx_readback) = dx_recorded
            .as_ref()
            .map_or((&vk_plan, vk_readback), |(plan, pass)| (plan, *pass));
        let mut vk_pixels = [0; PIXEL_BYTES];
        let mut dx_pixels = [0; PIXEL_BYTES];
        {
            let mut pending = vk.submit(&vk_plan).unwrap();
            let mut result = pending.wait(WAIT).unwrap().expect("Vulkan completion");
            result.readback_into(vk_readback, &mut vk_pixels).unwrap();
        }
        {
            let mut pending = dx.submit(dx_plan).unwrap();
            let mut result = pending.wait(WAIT).unwrap().expect("D3D12 completion");
            result.readback_into(dx_readback, &mut dx_pixels).unwrap();
        }
        assert_eq!(vk_pixels, dx_pixels);
        for y in 0..TARGET.height {
            for x in 0..TARGET.width {
                // The indexed triangle lies between clip x/y -1 and 0. With
                // 7×5 odd dimensions no pixel centers lie on the diagonal.
                let indexed = (2 * x + 1) * TARGET.height + (2 * y + 1) * TARGET.width
                    < TARGET.width * TARGET.height;
                let expected = if indexed {
                    [0, 255, 64, 255]
                } else {
                    [64, 0, 64, 255]
                };
                let i = ((y * TARGET.width + x) * 4) as usize;
                assert_eq!(vk_pixels[i..i + 4], expected, "pixel ({x}, {y})");
            }
        }
        drop((vk_plan, dx_recorded));
        assert!(pool.is_idle());
        drop(vk);
        let vk_report = vk_monitor.report();
        let dx_report = dx.shutdown().unwrap();
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
    }
}
