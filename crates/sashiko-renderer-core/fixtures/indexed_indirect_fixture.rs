//! Compare typed indirect commands with direct draws and analytic pixel colors.

use sashiko_renderer_core::graph::{
    Attachment, BufferDesc, CompiledGraph, GraphCapacity, GraphError, GraphPool, ImageDesc,
    IndexedIndirectTable, IndexedIndirectTemplate, LoadOp, PassHandle, RasterAttachments,
    RasterDraw, RasterParameters, StoreOp,
};
use sashiko_renderer_core::program::{
    BlendState, ManagedProgram, ManagedRasterProgram, RasterProgramDescriptor,
};
use sashiko_renderer_core::{
    d3d12::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12ValidationMode,
    },
    vulkan::{FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode, VulkanRuntime},
};
use std::{error::Error, time::Duration};

const WIDTH: u32 = 12;
const HEIGHT: u32 = 4;
const PIXEL_BYTES: usize = (WIDTH * HEIGHT * 4) as usize;
const WAIT: Duration = Duration::from_secs(30);
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const STRIPE_CHANNEL: u8 = 32;
const INDICES: [u32; 22] = [
    0, 1, 2, 0, // Sentinel before the first complete draw range.
    3, 4, 5, 5, 4, 6, 7, 8, 9, 9, 8, 10, 11, 12, 13, 13, 12, 14,
];
const TEMPLATES: [IndexedIndirectTemplate; 3] = [
    IndexedIndirectTemplate {
        first_index: 4,
        index_count: 6,
    },
    IndexedIndirectTemplate {
        first_index: 10,
        index_count: 6,
    },
    IndexedIndirectTemplate {
        first_index: 16,
        index_count: 6,
    },
];
const VERTICES: BufferDesc = BufferDesc { size: 15 * 28 };
const INDEX_BUFFER: BufferDesc = BufferDesc {
    size: size_of_val(&INDICES) as u64,
};
// Start at a nonzero byte offset that also meets D3D12 and Vulkan storage
// buffer alignment. Two workgroups' worth of mask entries follow the prefix.
const MASK_OFFSET: usize = 64;
const MASK_WORDS: usize = MASK_OFFSET + 128;
const MASK: BufferDesc = BufferDesc {
    size: (MASK_WORDS * size_of::<u32>()) as u64,
};
fn vertices() -> [[f32; 7]; 15] {
    let mut vertices = [[0.0; 7]; 15];
    vertices[..3].copy_from_slice(&[
        [-1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0],
        [1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0],
        [-1.0, 1.0, 0.5, 1.0, 1.0, 1.0, 1.0],
    ]);
    let edges = [-1.0, -1.0 / 3.0, 1.0 / 3.0, 1.0];
    for (stripe, quad) in vertices[3..].as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let [left, right] = [edges[stripe], edges[stripe + 1]];
        for (vertex, [x, y]) in
            quad.iter_mut()
                .zip([[left, -1.0], [right, -1.0], [left, 1.0], [right, 1.0]])
        {
            *vertex = [x, y, 0.5, 0.0, 0.0, 0.0, 0.0];
            vertex[stripe + 3] = f32::from(STRIPE_CHANNEL) / 255.0;
        }
    }
    vertices
}

#[derive(Clone, Copy, Debug)]
enum Visibility {
    All,
    SecondGroup,
    None,
    Mixed,
}

impl Visibility {
    fn mask(self) -> [u32; MASK_WORDS] {
        let mut words = [0; MASK_WORDS];
        words[..MASK_OFFSET].fill(7);
        match self {
            // Nonzero values with low bits clear still produce one instance.
            Self::All => words[MASK_OFFSET..MASK_OFFSET + 3].copy_from_slice(&[1, 2, 7]),
            Self::SecondGroup => words[MASK_OFFSET + 64] = 4,
            Self::None => {}
            Self::Mixed => words[MASK_OFFSET + 1] = 3,
        }
        words
    }

    fn visible_stripes(self) -> [bool; 3] {
        match self {
            Self::All => [true, true, true],
            Self::SecondGroup => [false, false, true],
            Self::None => [false; 3],
            Self::Mixed => [false, true, false],
        }
    }
}

fn build(
    pool: &mut GraphPool,
    table: &IndexedIndirectTable,
    visibility: Visibility,
    program: &ManagedRasterProgram,
) -> Result<(CompiledGraph, [PassHandle; 2]), GraphError> {
    let mut graph = pool.begin()?;
    let vertices_buffer = graph.buffer("packed vertices", VERTICES)?;
    let indices = graph.buffer("packed indices", INDEX_BUFFER)?;
    let mask = graph.buffer("visibility with nonzero offset", MASK)?;
    graph.upload_buffer(
        "upload packed vertices",
        vertices_buffer,
        0,
        vertices()
            .map(|vertex| vertex.map(f32::to_le_bytes))
            .as_flattened()
            .as_flattened(),
    )?;
    graph.upload_buffer(
        "upload packed indices",
        indices,
        0,
        INDICES.map(u32::to_le_bytes).as_flattened(),
    )?;
    graph.upload_buffer(
        "upload visibility",
        mask,
        0,
        visibility.mask().map(u32::to_le_bytes).as_flattened(),
    )?;
    let args = graph.encode_indexed_indirect(
        "encode indexed arguments",
        table,
        vertices_buffer,
        indices,
        mask,
        (MASK_OFFSET * size_of::<u32>()) as u64,
    )?;

    let color = ImageDesc::rgba8(WIDTH, HEIGHT);
    let direct = graph.image("direct image", color)?;
    let indirect = graph.image("indirect image", color)?;
    let direct_view = graph.image_view(direct, 0, 0)?;
    let indirect_view = graph.image_view(indirect, 0, 0)?;
    let parameters = RasterParameters::default().into();
    let reference = TEMPLATES
        .into_iter()
        .zip(visibility.visible_stripes())
        .filter(|(_, visible)| *visible)
        .map(|(template, _)| RasterDraw {
            secondary_vertices: None,
            vertices: vertices_buffer,
            indices,
            first_index: template.first_index,
            index_count: template.index_count,
            instance_count: 1,
            parameters,
            bindings: None,
        });
    graph.raster(
        "direct reference",
        RasterAttachments {
            colors: &[Attachment {
                view: direct_view,
                load: LoadOp::Clear(CLEAR),
                store: StoreOp::Store,
            }],
            color_resolves: &[],
            depth: None,
        },
        |raster| raster.draw(program, reference),
    )?;
    graph.raster(
        "indirect draws",
        RasterAttachments {
            colors: &[Attachment {
                view: indirect_view,
                load: LoadOp::Clear(CLEAR),
                store: StoreOp::Store,
            }],
            color_resolves: &[],
            depth: None,
        },
        |raster| raster.draw_indirect(program, args, None, parameters),
    )?;
    let outputs = [
        graph.readback_image("direct pixels", direct)?,
        graph.readback_image("indirect pixels", indirect)?,
    ];
    Ok((graph.compile()?, outputs))
}

fn verify(pixels: &[[u8; PIXEL_BYTES]; 2], visibility: Visibility, api: &str) {
    let [direct, indirect] = pixels;
    assert_eq!(direct, indirect, "{api} {visibility:?}");
    let visible_stripes = visibility.visible_stripes();
    for (index, &pixel) in indirect.as_chunks::<4>().0.iter().enumerate() {
        let stripe = (index % WIDTH as usize) / (WIDTH as usize / 3);
        let mut expected = [0, 0, 0, 255];
        if visible_stripes[stripe] {
            expected[stripe] = STRIPE_CHANNEL;
        }
        assert_eq!(pixel, expected, "{api} {visibility:?}, pixel {index}");
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let strict = cfg!(feature = "development-validation");
    let small_table = IndexedIndirectTable::new(&TEMPLATES)?;
    let mut crossing_templates = [TEMPLATES[0]; 65];
    crossing_templates[64] = TEMPLATES[2];
    let crossing_table = IndexedIndirectTable::new(&crossing_templates)?;
    let buffers = [
        VERTICES,
        INDEX_BUFFER,
        MASK,
        small_table.template_buffer_desc(),
        small_table.command_buffer_desc(),
        crossing_table.template_buffer_desc(),
        crossing_table.command_buffer_desc(),
    ];
    let capacity = GraphCapacity {
        resources: 7,
        passes: 9,
        draws: 4,
        upload_bytes: 4096,
        ..GraphCapacity::default()
    };
    let mesh = ManagedRasterProgram::mesh();
    // SAFETY: The built-in mesh shader binaries and their ABI are unchanged.
    // Additive blending makes extra instances observable in the readback.
    let program = unsafe {
        ManagedRasterProgram::from_trusted_owned_descriptor(RasterProgramDescriptor {
            blend: BlendState::Additive,
            ..mesh.descriptor()
        })
    }?;
    let encoder = ManagedProgram::indexed_indirect_encoder();
    let color = ImageDesc::rgba8(WIDTH, HEIGHT);
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 1,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )?;
    let mut classes = buffers
        .map(|desc| ResourceClass::Buffer { desc, count: 1 })
        .to_vec();
    classes.push(ResourceClass::Image {
        desc: color,
        count: 2,
    });
    vk.prepare_working_set(
        FrameCapacity {
            graph: capacity,
            work_slots: 1,
            ..Default::default()
        },
        &classes,
    )?;
    vk.prepare_raster_programs(std::slice::from_ref(&program))?;
    vk.prepare_compute_programs(std::slice::from_ref(&encoder))?;
    let dx_config = D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: 1,
            resources: capacity.resources,
            operations: 16,
            buffers: buffers.len(),
            upload_bytes: 4096,
            readback_bytes: 4096,
            ..Default::default()
        },
    };
    let mut dx = D3D12Runtime::new_with_working_set(
        dx_config,
        &buffers.map(|desc| D3D12BufferClass {
            size: desc.size,
            count: 1,
        }),
        &[D3D12ImageClass {
            desc: color,
            count: 2,
        }],
        &[program.clone().into()],
        &[encoder],
    )?;
    let adapter = vk.device_info();
    assert_ne!(adapter.device_type, "CPU");
    assert_eq!(adapter.device_luid, Some(dx.device_info().luid));
    let vk_monitor = vk.validation_monitor();
    let dx_monitor = dx.validation_monitor();
    let mut pool = GraphPool::new(capacity, 1)?;
    for (visibility, table) in [
        (Visibility::All, &small_table),
        (Visibility::SecondGroup, &crossing_table),
        (Visibility::None, &small_table),
        (Visibility::Mixed, &small_table),
    ] {
        let (plan, outputs) = build(&mut pool, table, visibility, &program)?;
        let mut pixels = [[0; PIXEL_BYTES]; 2];
        let mut vk_pending = vk
            .submit(&plan)
            .unwrap_or_else(|error| panic!("{error}: {:#?}", vk_monitor.report()));
        let mut vk_result = vk_pending.wait(WAIT)?.expect("Vulkan completion");
        for (output, bytes) in outputs.into_iter().zip(&mut pixels) {
            vk_result.readback_into(output, bytes)?;
        }
        verify(&pixels, visibility, "Vulkan");
        drop((vk_result, vk_pending));

        let mut dx_pending = dx
            .submit(&plan)
            .unwrap_or_else(|error| panic!("{error}: {:#?}", dx_monitor.snapshot()));
        drop(plan);
        let mut dx_result = dx_pending.wait(WAIT)?.expect("D3D12 completion");
        for (output, bytes) in outputs.into_iter().zip(&mut pixels) {
            dx_result.readback_into(output, bytes)?;
        }
        verify(&pixels, visibility, "D3D12");
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
        "Indexed indirect masks verified against direct draws on {} (strict={strict})",
        adapter.name
    );
    Ok(())
}
