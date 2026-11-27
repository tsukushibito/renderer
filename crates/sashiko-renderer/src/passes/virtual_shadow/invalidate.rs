use super::{AssignedPages, CAMERA_BYTES, PageRequests, REQUEST_BYTES, SLOT_BYTES, layout};
use crate::pbr::Bounds;
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation},
    graph::{Binding, BindingResource, BufferDesc, Graph, GraphError},
    program::{
        BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups, ManagedProgram,
    },
};
const NAME: &str = "changed virtual shadow pages";
const PASS: &str = "invalidate changed caster depths";
#[allow(unsafe_code)]
fn program() -> ManagedProgram {
    static DESC: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [64, 1, 1],
        bindings: &[
            layout(0, BindingType::Uniform, CAMERA_BYTES),
            layout(1, BindingType::StorageRead, REQUEST_BYTES),
            layout(2, BindingType::StorageReadWrite, SLOT_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Fixed([16, 1, 1]),
            bytes: &[CAMERA_BYTES as u64, REQUEST_BYTES as u64, SLOT_BYTES as u64],
        },
        shaders: native_shader!("invalidate"),
    };
    // SAFETY: one guarded invocation per slot. Check the encoded level
    // and both page coordinates before indexing the initialized change mask.
    unsafe { ManagedProgram::from_trusted_descriptor(&DESC) }
}
/// Prepare GPU marking of resident tiles touched by changed caster bounds.
pub fn reserve_invalidation(prep: &mut Preparation) -> Result<(), GraphError> {
    prep.reserve_buffer(
        BufferDesc {
            size: REQUEST_BYTES as u64,
        },
        1,
    )?;
    prep.reserve_dispatch(PASS, &program())?;
    prep.add_graph_requirements(GraphRequirements {
        resources: 1,
        passes: 1,
        accesses: 1,
        upload_bytes: REQUEST_BYTES,
        name_bytes: NAME.len() * 2,
        ..Default::default()
    })
}
/// Invalidate pages whose interior or filter gutter intersects an old or new
/// world-space caster bound. An unknown bound invalidates all resident pages.
pub fn invalidate_casters(
    graph: &mut Graph,
    requests: &PageRequests,
    pages: &AssignedPages,
    changed: &[Option<Bounds>],
) -> Result<(), GraphError> {
    if changed.is_empty() {
        return Ok(());
    }
    let mut mask = [0u32; REQUEST_BYTES / 4];
    for &world in changed {
        for (level, rect) in super::caster::rectangles(requests, world)
            .into_iter()
            .enumerate()
        {
            let Some([x, y, w, h]) = rect else {
                continue;
            };
            let end = x + w - 1;
            for row in y..y + h {
                for word in x / 32..=end / 32 {
                    let left = x.saturating_sub(word * 32);
                    let right = (end - word * 32).min(31);
                    mask[(level * 128 + row as usize) * 4 + word as usize] |=
                        (u32::MAX << left) & (u32::MAX >> (31 - right));
                }
            }
        }
    }
    let buffer = graph.buffer(
        NAME,
        BufferDesc {
            size: REQUEST_BYTES as u64,
        },
    )?;
    graph.upload_buffer(NAME, buffer, 0, mask.map(u32::to_le_bytes).as_flattened())?;
    let resources = [
        (requests.camera, CAMERA_BYTES),
        (buffer, REQUEST_BYTES),
        (pages.slots, SLOT_BYTES),
    ];
    graph.dispatch(
        PASS,
        &program(),
        &std::array::from_fn::<_, 3, _>(|i| Binding {
            group: 0,
            binding: i as u32,
            resource: BindingResource::Buffer {
                buffer: resources[i].0,
                offset: 0,
                size: resources[i].1 as u64,
            },
        }),
    )?;
    Ok(())
}
