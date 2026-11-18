use renderer::{
    D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
    D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode,
    VulkanRuntime,
    graph::{
        Attachment, BufferDesc, CompiledGraph, GraphCapacity, GraphPool, ImageDesc, LoadOp,
        PassHandle, RasterAttachments, RasterDraw, RasterParameters, SampleCount, StoreOp,
    },
    program::{ManagedRasterProgram, RasterPipelineRequest},
};
use std::time::Duration;

const SIDE: u32 = 8;
const READBACK_BYTES: usize = SIDE as usize * SIDE as usize * 8;
const WAIT: Duration = Duration::from_secs(30);
const VERTICES: BufferDesc = BufferDesc { size: 84 };
const INDICES: BufferDesc = BufferDesc { size: 12 };
const CLEAR: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn triangle(depth: f32, color: [f32; 4]) -> [u8; 84] {
    let mut bytes = [0; 84];
    // The diagonal of an 8×8 box traverses the interior of eight pixels.
    // Both triangles use the same footprint, so the farther one must never
    // contribute green, even at the partially covered edge.
    for (vertex, [x, y]) in
        bytes
            .as_chunks_mut::<28>()
            .0
            .iter_mut()
            .zip([[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0]])
    {
        for (word, value) in vertex
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip([x, y, depth, color[0], color[1], color[2], color[3]])
        {
            *word = value.to_le_bytes();
        }
    }
    bytes
}

fn frame(
    pool: &mut GraphPool,
    samples: SampleCount,
    draw_meshes: bool,
) -> (CompiledGraph, PassHandle) {
    let mut graph = pool.begin().unwrap();
    let color_desc = ImageDesc::rgba16_float(SIDE, SIDE);
    let color = graph
        .image("raster color", color_desc.with_sample_count(samples))
        .unwrap();
    let depth = graph
        .image(
            "raster depth",
            ImageDesc::depth32(SIDE, SIDE).with_sample_count(samples),
        )
        .unwrap();
    let target = if samples == SampleCount::X4 {
        graph
            .image("resolved single-sample HDR", color_desc)
            .unwrap()
    } else {
        color
    };
    let resolve =
        (samples == SampleCount::X4).then(|| Some(graph.image_view(target, 0, 0).unwrap()));
    let colors = [Attachment {
        view: graph.image_view(color, 0, 0).unwrap(),
        load: LoadOp::Clear(CLEAR),
        store: if resolve.is_some() {
            StoreOp::Discard
        } else {
            StoreOp::Store
        },
    }];
    let attachments = RasterAttachments {
        colors: &colors,
        color_resolves: resolve.as_slice(),
        depth: Some(Attachment {
            view: graph.image_view(depth, 0, 0).unwrap(),
            load: LoadOp::Clear(1.0),
            store: StoreOp::Discard,
        }),
    };
    if draw_meshes {
        let indices = graph.buffer("triangle indices", INDICES).unwrap();
        let index_bytes = [0u32, 1, 2].map(u32::to_le_bytes);
        graph
            .upload_buffer(
                "upload triangle indices",
                indices,
                0,
                index_bytes.as_flattened(),
            )
            .unwrap();
        let draws =
            [(0.25, [1.0, 0.0, 0.0, 1.0]), (0.75, [0.0, 1.0, 0.0, 1.0])].map(|(z, color)| {
                let vertices = graph.buffer("triangle vertices", VERTICES).unwrap();
                graph
                    .upload_buffer("upload triangle vertices", vertices, 0, &triangle(z, color))
                    .unwrap();
                RasterDraw {
                    bindings: None,
                    vertices,
                    indices,
                    index_count: 3,
                    parameters: RasterParameters::default().into(),
                }
            });
        graph
            .raster(
                "depth-tested triangles",
                &ManagedRasterProgram::mesh_hdr(),
                draws,
                attachments,
            )
            .unwrap();
    } else {
        graph
            .attachment_pass("clear and resolve", attachments)
            .unwrap();
    }
    let readback = graph.readback_image("read back 1× HDR", target).unwrap();
    (graph.compile().unwrap(), readback)
}

fn coverage(bytes: &[u8; READBACK_BYTES], samples: SampleCount, draw_meshes: bool) -> usize {
    // Binary16 representations of red/blue quarter-sample coverage.
    let fractions = [0, 0x3400, 0x3800, 0x3a00, 0x3c00];
    let mut histogram = [0; 5];
    for pixel in bytes.as_chunks::<8>().0 {
        let [r, g, b, a] = pixel.as_chunks::<2>().0 else {
            unreachable!()
        };
        let covered = fractions
            .iter()
            .position(|value| *value == u16::from_le_bytes(*r))
            .expect("red channel must be a quarter-coverage value");
        assert_eq!(u16::from_le_bytes(*g), 0, "far triangle leaked");
        assert_eq!(u16::from_le_bytes(*b), fractions[4 - covered]);
        assert_eq!(u16::from_le_bytes(*a), 0x3c00);
        histogram[covered] += 1;
    }
    let partial = histogram[1..4].iter().sum::<usize>();
    if draw_meshes {
        assert!(histogram[0] >= 28 && histogram[4] >= 28, "{histogram:?}");
        match samples {
            SampleCount::X1 => assert_eq!(partial, 0),
            // Require every diagonal pixel to contain a mix, without prescribing
            // which subpixel sample positions the device uses.
            SampleCount::X4 => assert_eq!(partial, SIDE as usize),
        }
    } else {
        assert_eq!(histogram[0], SIDE as usize * SIDE as usize);
    }
    partial
}

pub fn run() {
    let strict = cfg!(feature = "development-validation");
    let graph_capacity = GraphCapacity::default();
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 1,
            resource_capacity: 12,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .unwrap();
    let program = ManagedRasterProgram::mesh_hdr();
    let requests = [
        program.into(),
        RasterPipelineRequest {
            program,
            sample_count: SampleCount::X4,
        },
    ];
    let color = ImageDesc::rgba16_float(SIDE, SIDE);
    let depth = ImageDesc::depth32(SIDE, SIDE);
    let image_descs = [
        color,
        depth,
        color.with_sample_count(SampleCount::X4),
        depth.with_sample_count(SampleCount::X4),
    ];
    let mut resources = image_descs
        .map(|desc| ResourceClass::Image { desc, count: 1 })
        .to_vec();
    resources.extend([
        ResourceClass::Buffer {
            desc: VERTICES,
            count: 2,
        },
        ResourceClass::Buffer {
            desc: INDICES,
            count: 1,
        },
    ]);
    vk.prepare_working_set(
        FrameCapacity {
            graph: graph_capacity,
            work_slots: 1,
            ..Default::default()
        },
        &resources,
    )
    .unwrap();
    vk.prepare_raster_programs(&requests).unwrap();
    let mut dx = D3D12Runtime::new_with_working_set(
        D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                resources: 12,
                operations: 24,
                upload_bytes: 4096,
                readback_bytes: 4096,
                ..Default::default()
            },
        },
        &[
            D3D12BufferClass {
                size: VERTICES.size,
                count: 2,
            },
            D3D12BufferClass {
                size: INDICES.size,
                count: 1,
            },
        ],
        &image_descs.map(|desc| D3D12ImageClass { desc, count: 1 }),
        &requests,
        &[],
    )
    .unwrap();
    let adapter = vk.device_info();
    assert_ne!(adapter.device_type, "CPU");
    assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
    let vk_monitor = vk.validation_monitor();
    let dx_monitor = dx.validation_monitor();
    let mut pool = GraphPool::new(graph_capacity, 1).unwrap();
    for (samples, draw_meshes) in [
        (SampleCount::X1, true),
        (SampleCount::X4, true),
        (SampleCount::X4, false),
    ] {
        let (plan, readback) = frame(&mut pool, samples, draw_meshes);
        let mut vk_bytes = [0; READBACK_BYTES];
        let mut dx_bytes = [0; READBACK_BYTES];
        let mut pending = vk.submit(&plan).unwrap();
        let mut result = pending.wait(WAIT).unwrap().expect("Vulkan completion");
        result.readback_into(readback, &mut vk_bytes).unwrap();
        drop((result, pending));
        let mut pending = dx
            .submit(&plan)
            .unwrap_or_else(|error| panic!("{error}: {:#?}", dx_monitor.snapshot()));
        let mut result = pending.wait(WAIT).unwrap().expect("D3D12 completion");
        result.readback_into(readback, &mut dx_bytes).unwrap();
        drop((result, pending, plan));
        let vk_partial = coverage(&vk_bytes, samples, draw_meshes);
        let dx_partial = coverage(&dx_bytes, samples, draw_meshes);
        eprintln!(
            "{}× draws={draw_meshes}: partial pixels Vulkan={vk_partial} D3D12={dx_partial}",
            samples.as_u32()
        );
    }
    assert!(pool.is_idle());
    drop(vk);
    let dx_report = dx.shutdown().unwrap();
    let vk_report = vk_monitor.report();
    assert!(vk_report.is_clean(), "{vk_report:?}");
    assert!(dx_report.is_clean(), "{dx_report:?}");
    if strict {
        assert_eq!(vk_report.core_validation, Some(true));
        assert_eq!(vk_report.synchronization_validation, Some(true));
        assert!(dx_report.debug_layer_enabled && dx_report.gpu_based_validation_enabled);
        assert!(dx_report.teardown_checked);
    }
    eprintln!("MSAA resolve checked on {}, strict={strict}", adapter.name);
}
