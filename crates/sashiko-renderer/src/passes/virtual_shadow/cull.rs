use super::{AssignedPages, CAMERA_BYTES, LEVELS, PageRequests, SLOT_BYTES, layout};
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation},
    graph::{Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError},
    program::{
        BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups, ManagedProgram,
    },
};

pub(super) const LIST_BYTES: usize = 4096;
pub(super) const RECT_BYTES: usize = LEVELS * 16;
const LANES: usize = 64;
// Keep each descriptor within 32 MiB and its group count within Vulkan's portable limit.
const CHUNK: usize = 8192;
const BOUNDS: &str = "virtual shadow caster bounds";
const LISTS: &str = "dirty shadow pages per caster";
const COUNTS: &str = "virtual shadow caster counts";
const CULL: &str = "compact dirty caster pages";
const GATHER: &str = "gather shadow instance counts";
pub(super) const ENCODE: &str = "encode virtual shadow draws";

#[allow(unsafe_code)]
fn programs() -> [ManagedProgram; 2] {
    use BindingType::{StorageRead, StorageWrite, Uniform};
    static CULL_PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [64, 1, 1],
        bindings: &[
            layout(0, Uniform, CAMERA_BYTES),
            layout(1, StorageRead, SLOT_BYTES),
            layout(2, StorageRead, RECT_BYTES),
            layout(3, StorageWrite, LIST_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::BufferGrid2d {
                group: 0,
                binding: 3,
                read_bindings: &[(0, 2)],
                max_workgroups: [CHUNK as u32, 1],
            },
            bytes: &[
                CAMERA_BYTES as u64,
                SLOT_BYTES as u64,
                RECT_BYTES as u64,
                LIST_BYTES as u64,
            ],
        },
        shaders: native_shader!("cull_casters"),
    };
    static COUNTS_PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [64, 1, 1],
        bindings: &[
            layout(0, StorageRead, LIST_BYTES * LANES),
            layout(1, StorageWrite, 4 * LANES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::BufferGrid2d {
                group: 0,
                binding: 1,
                read_bindings: &[(0, 0)],
                max_workgroups: [(CHUNK / LANES) as u32, 1],
            },
            bytes: &[(LIST_BYTES * LANES) as u64, (4 * LANES) as u64],
        },
        shaders: native_shader!("caster_counts"),
    };
    // SAFETY: Each group writes one full list. Its lanes partition the slots,
    // reserve disjoint output prefixes and initialize all remaining words.
    // Owner levels are checked before reading the eight input rectangles.
    // The gather reads the first word of each list and writes disjoint counts.
    unsafe {
        [
            ManagedProgram::from_trusted_descriptor(&CULL_PROGRAM),
            ManagedProgram::from_trusted_descriptor(&COUNTS_PROGRAM),
        ]
    }
}

pub(super) fn reserve(prep: &mut Preparation, capacity: usize) -> Result<(), GraphError> {
    if capacity == 0 {
        return Ok(());
    }
    let padded = capacity
        .checked_next_multiple_of(LANES)
        .ok_or_else(|| GraphError::invalid_input("virtual caster capacity overflow"))?;
    let list_bytes = padded
        .checked_mul(LIST_BYTES)
        .ok_or_else(|| GraphError::invalid_input("virtual caster list size overflow"))?;
    let [cull, gather] = programs();
    for _ in 0..padded.div_ceil(CHUNK) {
        prep.reserve_dispatch(CULL, &cull)?;
        prep.reserve_dispatch(GATHER, &gather)?;
    }
    prep.reserve_dispatch(ENCODE, &ManagedProgram::indexed_indirect_encoder())?;
    let [templates, commands] =
        sashiko_renderer_core::graph::IndexedInstanceBatch::required_buffers(capacity)?;
    for size in [
        padded * RECT_BYTES,
        list_bytes,
        padded * 4,
        templates.size as usize,
        commands.size as usize,
    ] {
        prep.reserve_buffer(BufferDesc { size: size as u64 }, 1)?;
    }
    prep.add_graph_requirements(GraphRequirements {
        resources: 5,
        passes: 2,
        accesses: 2,
        upload_bytes: padded * RECT_BYTES + templates.size as usize,
        name_bytes: BOUNDS.len() * 2
            + LISTS.len()
            + COUNTS.len()
            + "indirect templates".len()
            + "indirect arguments".len()
            + "upload indirect templates".len(),
        ..Default::default()
    })
}

fn buffer(binding: u32, buffer: BufferHandle, offset: usize, size: usize) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: offset as u64,
            size: size as u64,
        },
    }
}

pub(super) fn record(
    graph: &mut Graph,
    requests: &PageRequests,
    pages: &AssignedPages,
    rectangles: &[u8],
) -> Result<(BufferHandle, BufferHandle), GraphError> {
    let padded = rectangles.len() / RECT_BYTES;
    let bounds = graph.buffer(
        BOUNDS,
        BufferDesc {
            size: rectangles.len() as u64,
        },
    )?;
    graph.upload_buffer(BOUNDS, bounds, 0, rectangles)?;
    let lists = graph.buffer(
        LISTS,
        BufferDesc {
            size: (padded * LIST_BYTES) as u64,
        },
    )?;
    let counts = graph.buffer(
        COUNTS,
        BufferDesc {
            size: (padded * 4) as u64,
        },
    )?;
    let [cull, gather] = programs();
    for start in (0..padded).step_by(CHUNK) {
        let n = (padded - start).min(CHUNK);
        graph.dispatch_buffer_grid(
            CULL,
            &cull,
            &[
                buffer(0, requests.camera, 0, CAMERA_BYTES),
                buffer(1, pages.slots, 0, SLOT_BYTES),
                buffer(2, bounds, start * RECT_BYTES, n * RECT_BYTES),
                buffer(3, lists, start * LIST_BYTES, n * LIST_BYTES),
            ],
            [n as u32, 1],
        )?;
        graph.dispatch_buffer_grid(
            GATHER,
            &gather,
            &[
                buffer(0, lists, start * LIST_BYTES, n * LIST_BYTES),
                buffer(1, counts, start * 4, n * 4),
            ],
            [(n / LANES) as u32, 1],
        )?;
    }
    Ok((lists, counts))
}
