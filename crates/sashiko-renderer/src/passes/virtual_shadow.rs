//! Directional virtual shadow page requests from the current camera depth.

mod atlas;
pub use atlas::{ATLAS_DESC, draw_atlas, reserve_atlas};
mod caster;
pub use caster::{CasterDraw, bind_caster};
mod camera_depth;
pub use camera_depth::{bind_receiver, camera_matrix, draw_camera_depth, reserve_camera_depth};

use crate::pbr::{Bounds, ShadowSettings, Sun, ViewInput};
use sashiko_renderer_core::{
    ImageFormat,
    capacity::{GraphRequirements, Preparation},
    graph::{Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError, ImageView},
    program::{
        BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
        ManagedProgram, ShaderStages,
    },
};

pub const LEVELS: usize = 8;
pub const PAGES_PER_AXIS: usize = 128;
pub const PAGE_TEXELS: usize = 128;
pub const REQUEST_BYTES: usize = LEVELS * PAGES_PER_AXIS * PAGES_PER_AXIS / 8;
pub const PHYSICAL_PAGES: usize = 1024;
pub const TABLE_BYTES: usize = LEVELS * PAGES_PER_AXIS * PAGES_PER_AXIS * 4;
const BLOCK_BYTES: usize = 64 * 4;
const CAMERA_BYTES: usize = 112 + LEVELS * 16 + 32;
pub const SLOT_BYTES: usize = PHYSICAL_PAGES * 16;
const SLOTS: &str = "virtual shadow physical slots";
const EMPTY: &str = "virtual shadow empty history";
const RESET: &str = "reset virtual shadow history";
const TAG: &str = "identify dirty shadow pages";

/// Demand mask and the snapped clipmap origins used to produce it.
#[derive(Clone, Copy)]
pub struct PageRequests {
    pub mask: BufferHandle,
    pub camera: BufferHandle,
    axes: [[f64; 3]; 2],
    origins: [[i32; 2]; LEVELS],
}

/// All buffer contents describe this frame. Export the slot buffer together
/// with its populated atlas after recording all required caster draws.
pub struct AssignedPages {
    pub table: BufferHandle,
    pub slots: BufferHandle,
}

const CAMERA: &str = "virtual shadow camera";
const REQUESTS: &str = "virtual shadow requests";
const CLEAR: &str = "clear virtual shadow requests";
const MARK: &str = "mark virtual shadow receivers";
const UPLOAD: &str = "upload virtual shadow camera";
const COUNTS: &str = "virtual shadow block counts";
const TABLE: &str = "virtual shadow page table";
const COUNT: &str = "count requested shadow pages";
const ASSIGN: &str = "assign physical shadow pages";

macro_rules! native_shader {
    ($name:literal) => {
        sashiko_renderer_core::program::embedded_shader_binaries!(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/shaders/virtual_shadow/",
                $name,
                ".comp.spv"
            )),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/shaders/virtual_shadow/dxil/",
                $name,
                ".comp.dxil"
            )),
        )
    };
}

mod receiver;
pub use receiver::{reserve_visibility, resolve_visibility};

const fn layout(binding: u32, kind: BindingType, min_size: usize) -> BindingLayout {
    BindingLayout {
        group: 0,
        binding,
        kind,
        min_size: min_size as u64,
        stages: ShaderStages::COMPUTE,
    }
}

#[allow(unsafe_code)]
fn program() -> ManagedProgram {
    static DESC: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [8, 8, 1],
        bindings: &[
            layout(0, BindingType::Uniform, CAMERA_BYTES),
            layout(1, BindingType::SampledImage, 0),
            layout(2, BindingType::StorageReadWrite, REQUEST_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Fixed([32, 32, 1]),
            bytes: &[CAMERA_BYTES as u64, 0, REQUEST_BYTES as u64],
        },
        shaders: native_shader!("requests"),
    };
    // SAFETY: loads stay within GetDimensions at mip zero. The only writes are
    // atomic ORs to an initialized table. Integer coordinates are bounded before
    // indexing, independently of input bits. No shared memory or subgroup ops.
    unsafe { ManagedProgram::from_trusted_descriptor(&DESC) }
}

#[allow(unsafe_code)]
fn assignment_programs() -> [ManagedProgram; 2] {
    use BindingType::{StorageRead, StorageWrite};
    static COUNT_PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [64, 1, 1],
        bindings: &[
            layout(0, StorageRead, REQUEST_BYTES),
            layout(1, StorageWrite, BLOCK_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Fixed([1, 1, 1]),
            bytes: &[REQUEST_BYTES as u64, BLOCK_BYTES as u64],
        },
        shaders: native_shader!("count"),
    };
    static ASSIGN_PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [64, 1, 1],
        bindings: &[
            layout(0, StorageRead, REQUEST_BYTES),
            layout(1, StorageRead, BLOCK_BYTES),
            layout(2, StorageWrite, TABLE_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Fixed([64, 1, 1]),
            bytes: &[REQUEST_BYTES as u64, BLOCK_BYTES as u64, TABLE_BYTES as u64],
        },
        shaders: native_shader!("assign"),
    };
    // SAFETY: fixed launch sizes are the complete output domains. Count lanes read
    // a permuted 64-word block each. Assignment lanes read at most 63 previous
    // counts and 64 request words in their block, writing 32 disjoint entries.
    // Neither shader derives a memory address from buffer contents.
    unsafe {
        [
            ManagedProgram::from_trusted_descriptor(&COUNT_PROGRAM),
            ManagedProgram::from_trusted_descriptor(&ASSIGN_PROGRAM),
        ]
    }
}

#[allow(unsafe_code)]
fn slot_program() -> ManagedProgram {
    static DESC: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [64, 1, 1],
        bindings: &[
            layout(0, BindingType::Uniform, CAMERA_BYTES),
            layout(1, BindingType::StorageRead, REQUEST_BYTES),
            layout(2, BindingType::StorageRead, BLOCK_BYTES),
            layout(3, BindingType::StorageRead, SLOT_BYTES),
            layout(4, BindingType::StorageWrite, SLOT_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Fixed([16, 1, 1]),
            bytes: &[
                CAMERA_BYTES as u64,
                REQUEST_BYTES as u64,
                BLOCK_BYTES as u64,
                SLOT_BYTES as u64,
                SLOT_BYTES as u64,
            ],
        },
        shaders: native_shader!("page_state"),
    };
    // SAFETY: the fixed dispatch maps injectively onto all 1024 records. All
    // search indices depend only on bounded loop counters. A rank smaller than
    // a word's population count selects one of its set bits.
    unsafe { ManagedProgram::from_trusted_descriptor(&DESC) }
}

/// Reserve the 128-by-128 page table per level and per-slot ownership records.
pub fn reserve_table(preparation: &mut Preparation) -> Result<(), GraphError> {
    let [count, assign] = assignment_programs();
    preparation.reserve_buffer(
        BufferDesc {
            size: BLOCK_BYTES as u64,
        },
        1,
    )?;
    preparation.reserve_buffer(
        BufferDesc {
            size: TABLE_BYTES as u64,
        },
        1,
    )?;
    preparation.reserve_buffer(
        BufferDesc {
            size: SLOT_BYTES as u64,
        },
        2,
    )?;
    preparation.reserve_dispatch(TAG, &slot_program())?;
    preparation.reserve_dispatch(COUNT, &count)?;
    preparation.reserve_dispatch(ASSIGN, &assign)?;
    preparation.add_graph_requirements(GraphRequirements {
        resources: 4,
        passes: 1,
        accesses: 1,
        upload_bytes: SLOT_BYTES,
        name_bytes: COUNTS.len() + TABLE.len() + SLOTS.len() + EMPTY.len() + RESET.len(),
        ..Default::default()
    })
}

/// Assign up to 1,024 physical slots. Table entries encode slot + 1, or zero
/// when unallocated. Requested coarsest pages are assigned before finer levels.
/// Pass the previous accepted slot buffer only when the same atlas, light
/// basis, depth range and caster contents are available. None invalidates all
/// active slots. Each slot holds four u32 words: level+1 (zero when unused),
/// signed world page X/Y, and a flag indicating that its depth needs redrawing.
pub fn assign_pages(
    graph: &mut Graph,
    requests: PageRequests,
    previous: Option<BufferHandle>,
) -> Result<AssignedPages, GraphError> {
    let [count, assign] = assignment_programs();
    let counts = graph.buffer(
        COUNTS,
        BufferDesc {
            size: BLOCK_BYTES as u64,
        },
    )?;
    let table = graph.buffer(
        TABLE,
        BufferDesc {
            size: TABLE_BYTES as u64,
        },
    )?;
    let slot = |binding, buffer, bytes: usize| Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size: bytes as u64,
        },
    };
    graph.dispatch(
        COUNT,
        &count,
        &[
            slot(0, requests.mask, REQUEST_BYTES),
            slot(1, counts, BLOCK_BYTES),
        ],
    )?;
    graph.dispatch(
        ASSIGN,
        &assign,
        &[
            slot(0, requests.mask, REQUEST_BYTES),
            slot(1, counts, BLOCK_BYTES),
            slot(2, table, TABLE_BYTES),
        ],
    )?;
    let slots = graph.buffer(
        SLOTS,
        BufferDesc {
            size: SLOT_BYTES as u64,
        },
    )?;
    let previous = match previous {
        Some(previous) => previous,
        None => {
            let empty = graph.buffer(
                EMPTY,
                BufferDesc {
                    size: SLOT_BYTES as u64,
                },
            )?;
            graph.upload_buffer(RESET, empty, 0, &[0; SLOT_BYTES])?;
            empty
        }
    };
    graph.dispatch(
        TAG,
        &slot_program(),
        &[
            slot(0, requests.camera, CAMERA_BYTES),
            slot(1, requests.mask, REQUEST_BYTES),
            slot(2, counts, BLOCK_BYTES),
            slot(3, previous, SLOT_BYTES),
            slot(4, slots, SLOT_BYTES),
        ],
    )?;
    Ok(AssignedPages { table, slots })
}

/// Reserve the two temporary buffers, copies and dispatch for one depth view per frame.
pub fn reserve_requests(preparation: &mut Preparation) -> Result<(), GraphError> {
    for bytes in [REQUEST_BYTES, CAMERA_BYTES] {
        preparation.reserve_buffer(BufferDesc { size: bytes as u64 }, 1)?;
    }
    preparation.reserve_dispatch(MARK, &program())?;
    preparation.add_graph_requirements(GraphRequirements {
        resources: 2,
        passes: 2,
        accesses: 2,
        upload_bytes: REQUEST_BYTES + CAMERA_BYTES,
        name_bytes: CAMERA.len() + REQUESTS.len() + CLEAR.len() + UPLOAD.len(),
        ..Default::default()
    })
}

/// Bit n in the result belongs to page (x,y) where n = (level * 128 + y) * 128 + x.
/// At each level the lower-left page origin is floor(projected_eye/page_width)-64.
/// Requires single-sample normal-Z D32 depth with sky cleared to one.
pub fn request_pages(
    graph: &mut Graph,
    depth: ImageView,
    view: ViewInput,
    sun: Sun,
    bounds: Bounds,
    settings: ShadowSettings,
) -> Result<PageRequests, GraphError> {
    settings.validate()?;
    let desc = graph.image_desc(depth.image())?;
    if desc.format != ImageFormat::Depth32Float
        || desc.sample_count != sashiko_renderer_core::graph::SampleCount::X1
        || depth.mip_level_count() != 1
    {
        return Err(GraphError::invalid_input(
            "invalid virtual shadow request inputs",
        ));
    }
    let mut camera = [0u8; CAMERA_BYTES];
    camera[..64].copy_from_slice(&crate::pbr_deferred::passes::inverse_bytes(
        view.clip_from_world(),
    )?);
    let [x, y, z] = sun.light_basis()?;
    let [ex, ey, ez] = view.eye();
    let axes = [
        ex,
        ey,
        ez,
        settings.distance,
        x[0] as f32,
        x[1] as f32,
        x[2] as f32,
        0.,
        y[0] as f32,
        y[1] as f32,
        y[2] as f32,
        0.,
    ];
    for (slot, value) in camera[64..].as_chunks_mut::<4>().0.iter_mut().zip(axes) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
    let mut origins = [[0i32; 2]; LEVELS];
    for (level, origin) in camera[112..112 + LEVELS * 16]
        .as_chunks_mut::<16>()
        .0
        .iter_mut()
        .enumerate()
    {
        for ((slot, target), axis) in origin
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&mut origins[level])
            .zip([x, y])
        {
            let projected: f64 = view
                .eye()
                .into_iter()
                .zip(axis)
                .map(|(v, a)| f64::from(v) * a)
                .sum();
            let page = (projected * (25.0 / (1u32 << level) as f64)).floor() - 64.0;
            if !(f64::from(i32::MIN)..=f64::from(i32::MAX - 127)).contains(&page) {
                return Err(GraphError::invalid_input(
                    "virtual shadow page origin is out of range",
                ));
            }
            *target = page as i32;
            slot.copy_from_slice(&target.to_le_bytes());
        }
    }
    // Fixed while the light, padding and scene bounds are unchanged, including
    // when the camera moves. Casters nearer the sun write smaller D32 values.
    let mut near = f64::from(settings.depth_padding);
    let mut far = -near;
    for (axis, (low, high)) in z
        .into_iter()
        .zip(bounds.min().into_iter().zip(bounds.max()))
    {
        let (a, b) = (axis * f64::from(low), axis * f64::from(high));
        near += a.max(b);
        far += a.min(b);
    }
    let scale = (near - far).recip();
    let plane = [
        (-z[0] * scale) as f32,
        (-z[1] * scale) as f32,
        (-z[2] * scale) as f32,
        (near * scale) as f32,
    ];
    if !plane.into_iter().all(f32::is_finite) || !(scale as f32).is_finite() {
        return Err(GraphError::invalid_input(
            "virtual shadow depth range is out of range",
        ));
    }
    let bias = [
        settings.depth_bias_texels,
        settings.slope_bias_texels,
        settings.normal_offset_texels,
        scale as f32,
    ];
    for (word, value) in camera[112 + LEVELS * 16..]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(plane.into_iter().chain(bias))
    {
        word.copy_from_slice(&value.to_le_bytes());
    }
    let uniform = graph.buffer(
        CAMERA,
        BufferDesc {
            size: CAMERA_BYTES as u64,
        },
    )?;
    graph.upload_buffer(UPLOAD, uniform, 0, &camera)?;
    let mask = graph.buffer(
        REQUESTS,
        BufferDesc {
            size: REQUEST_BYTES as u64,
        },
    )?;
    graph.upload_buffer(CLEAR, mask, 0, &[0; REQUEST_BYTES])?;
    let buffer = |buffer, size| BindingResource::Buffer {
        buffer,
        offset: 0,
        size,
    };
    let bindings = [
        buffer(uniform, CAMERA_BYTES as u64),
        BindingResource::ImageView(depth),
        buffer(mask, REQUEST_BYTES as u64),
    ];
    graph.dispatch(
        MARK,
        &program(),
        &std::array::from_fn::<_, 3, _>(|i| Binding {
            group: 0,
            binding: i as u32,
            resource: bindings[i],
        }),
    )?;
    Ok(PageRequests {
        mask,
        camera: uniform,
        axes: [x, y],
        origins,
    })
}
