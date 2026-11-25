//! GPU frustum culling and indexed indirect argument generation.

#[allow(unsafe_code)]
mod program;

use sashiko_renderer_core::capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet};
use sashiko_renderer_core::graph::{
    Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacityKind, GraphError,
    IndexedIndirectArgs, IndexedIndirectTable,
};
use sashiko_renderer_core::program::ManagedProgram;

use program::VISIBILITY_MAX_WORKGROUPS;

pub use program::{
    VISIBILITY_AABB_BYTES, VISIBILITY_CAMERA_BYTES, VISIBILITY_MASK_BYTES, VISIBILITY_THREADS,
    visibility_program,
};

const MASK: &str = "visibility mask";
const CULL_PASS: &str = "cull indexed draw bounds";
const ENCODE_PASS: &str = "encode visible indexed draws";

/// World-space AABB associated with a command. Disabled bounds produce no
/// instances. Enabled bounds with uncertain clip arithmetic remain visible.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorldBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub enabled: bool,
}

/// Graph-local vertex and index buffers for one indirect command group.
#[derive(Clone, Copy, Debug)]
pub struct IndexedGeometry {
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
}

struct Group {
    table: IndexedIndirectTable,
    mask_offset: u64,
}

/// Cold command tables shared by frames with the same group and command order.
///
/// Keep this layout alive across frame retirement so its template upload bytes
/// remain anchored. The app owns mesh packing, the group order and the bounds
/// buffer. Refresh the bounds when the underlying geometry changes.
pub struct PreparedVisibility {
    groups: Vec<Group>,
    padded_slots: u32,
    template_bytes: u64,
}

/// Results in table order. Each enabled group yields an indirect argument
/// token usable by [`sashiko_renderer_core::graph::RasterPass::draw_indirect`].
pub struct VisibilityFrame<'a> {
    /// One little-endian u32 visibility mask per padded command slot.
    pub mask: BufferHandle,
    /// Disabled groups yield `None`.
    pub arguments: &'a [Option<IndexedIndirectArgs>],
}

impl PreparedVisibility {
    pub fn new(tables: &[IndexedIndirectTable]) -> Result<Self, GraphError> {
        let max_slots = VISIBILITY_MAX_WORKGROUPS * VISIBILITY_THREADS;
        if tables.is_empty() || tables.len() > VISIBILITY_MAX_WORKGROUPS as usize {
            return Err(GraphError::invalid_input(
                "visibility requires a nonempty supported group count",
            ));
        }
        let mut groups = Vec::with_capacity(tables.len());
        let mut padded_slots = 0u32;
        let mut template_bytes = 0;
        for table in tables {
            let next = padded_slots
                .checked_add(table.padded_count())
                .filter(|count| *count <= max_slots)
                .ok_or_else(|| {
                    GraphError::invalid_input("visibility dispatch capacity exceeded")
                })?;
            groups.push(Group {
                table: table.clone(),
                mask_offset: u64::from(padded_slots) * VISIBILITY_MASK_BYTES as u64,
            });
            padded_slots = next;
            // Each table is eight bytes per slot, bounded by max_slots.
            template_bytes += table.template_buffer_desc().size;
        }
        Ok(Self {
            groups,
            padded_slots,
            template_bytes,
        })
    }

    /// Reserve the transient resources, Programs and Graph declarations for
    /// one invocation per frame with all groups enabled. The caller reserves
    /// the camera, bounds and geometry sources and the consuming draw passes.
    pub fn reserve(&self, preparation: &mut Preparation) -> Result<(), GraphError> {
        let groups = self.group_count();
        let count = preparation.frames_in_flight().get();
        let mut resources = Vec::with_capacity(1 + groups * 2);
        resources.push(ResourceClass::Buffer {
            desc: self.mask_desc(),
            count,
        });
        for group in &self.groups {
            for desc in [
                group.table.template_buffer_desc(),
                group.table.command_buffer_desc(),
            ] {
                resources.push(ResourceClass::Buffer { desc, count });
            }
        }
        preparation.add_requirements(
            GraphRequirements {
                resources: resources.len(),
                passes: 1 + groups * 2,
                compute_dispatches: 1 + groups,
                accesses: 3 + groups * 4,
                bindings: 3 + groups * 3,
                // Per group, include the core encoder's three internal names.
                name_bytes: MASK.len() + CULL_PASS.len() + groups * (ENCODE_PASS.len() + 96),
                ..GraphRequirements::default()
            },
            WorkingSet {
                resources: &resources,
                compute: &[
                    visibility_program(),
                    ManagedProgram::indexed_indirect_encoder(),
                ],
                ..WorkingSet::default()
            },
        )
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Buffer size for the caller's packed bounds records.
    pub fn bounds_desc(&self) -> BufferDesc {
        BufferDesc {
            size: u64::from(self.padded_slots) * VISIBILITY_AABB_BYTES as u64,
        }
    }

    pub fn mask_desc(&self) -> BufferDesc {
        BufferDesc {
            size: u64::from(self.padded_slots) * VISIBILITY_MASK_BYTES as u64,
        }
    }

    /// Template transfers per frame with all groups enabled. Use this with
    /// [`Self::template_transfer_bytes`] to budget native staging.
    pub fn template_transfer_count(&self) -> usize {
        self.group_count()
    }

    /// Total bytes read from the cold command tables by the template uploads.
    /// The Graph retains that storage directly, outside its frame upload arena.
    pub fn template_transfer_bytes(&self) -> u64 {
        self.template_bytes
    }

    /// Encode one bounds record per real command, with disabled padding at the
    /// end of each group. The destination has the exact [`Self::bounds_desc`]
    /// size. The callback receives group and command indices in table order.
    pub fn encode_bounds(
        &self,
        destination: &mut [u8],
        mut bounds: impl FnMut(usize, usize) -> WorldBounds,
    ) -> Result<(), GraphError> {
        if destination.len() as u64 != self.bounds_desc().size {
            return Err(GraphError::invalid_input(
                "visibility bounds destination size differs from the prepared layout",
            ));
        }
        let (mut records, _) = destination.as_chunks_mut::<VISIBILITY_AABB_BYTES>();
        for (group_index, group) in self.groups.iter().enumerate() {
            let (this_group, rest) = records.split_at_mut(group.table.padded_count() as usize);
            let (active, padding) = this_group.split_at_mut(group.table.command_count() as usize);
            for (index, record) in active.iter_mut().enumerate() {
                *record = bounds_record(bounds(group_index, index));
            }
            padding.fill([0; VISIBILITY_AABB_BYTES]);
            records = rest;
        }
        Ok(())
    }

    /// Update a contiguous set of commands in an already initialized packed bounds
    /// array. Returns the affected byte range to upload at the same destination offset.
    /// Other commands and padding keep their previous contents.
    pub fn encode_group_bounds(
        &self,
        destination: &mut [u8],
        group: usize,
        commands: std::ops::Range<usize>,
        mut bounds: impl FnMut(usize) -> WorldBounds,
    ) -> Result<std::ops::Range<usize>, GraphError> {
        let group = self
            .groups
            .get(group)
            .ok_or_else(|| GraphError::invalid_input("unknown visibility group"))?;
        if destination.len() as u64 != self.bounds_desc().size
            || commands.start > commands.end
            || commands.end > group.table.command_count() as usize
        {
            return Err(GraphError::invalid_input(
                "invalid packed visibility bounds range",
            ));
        }
        let base = group.mask_offset as usize / VISIBILITY_MASK_BYTES;
        let bytes = (base + commands.start) * VISIBILITY_AABB_BYTES
            ..(base + commands.end) * VISIBILITY_AABB_BYTES;
        for (index, record) in commands.zip(
            destination[bytes.clone()]
                .as_chunks_mut::<VISIBILITY_AABB_BYTES>()
                .0,
        ) {
            *record = bounds_record(bounds(index));
        }
        Ok(bytes)
    }

    /// Cull all padded slots and encode the enabled groups. The camera begins
    /// with a column-major f32 view-projection matrix. Bounds use the layout
    /// produced by [`Self::encode_bounds`]. Both are initialized in the Graph.
    ///
    /// Supply one preallocated argument slot per group. Return `None` from
    /// `geometry` to omit that group's encoder and draw. On declaration failure
    /// discard the partially built Graph.
    pub fn add_passes<'a>(
        &self,
        graph: &mut Graph,
        camera: BufferHandle,
        bounds: BufferHandle,
        arguments: &'a mut [Option<IndexedIndirectArgs>],
        mut geometry: impl FnMut(usize) -> Option<IndexedGeometry>,
    ) -> Result<VisibilityFrame<'a>, GraphError> {
        let argument_slots = arguments.len();
        let arguments = arguments.get_mut(..self.group_count()).ok_or_else(|| {
            GraphError::capacity_error(
                GraphCapacityKind::Draws,
                argument_slots,
                Some(self.group_count()),
            )
        })?;
        let camera_bytes = VISIBILITY_CAMERA_BYTES as u64;
        let bounds_bytes = self.bounds_desc().size;
        if graph.buffer_desc(camera)?.size < camera_bytes
            || graph.buffer_desc(bounds)?.size < bounds_bytes
        {
            return Err(GraphError::invalid_input(
                "visibility camera or bounds buffer is smaller than the prepared input",
            ));
        }
        let mask = graph.buffer(MASK, self.mask_desc())?;
        graph.dispatch_buffer_grid(
            CULL_PASS,
            &visibility_program(),
            &[
                binding(0, camera, camera_bytes),
                binding(1, bounds, bounds_bytes),
                binding(2, mask, self.mask_desc().size),
            ],
            [self.padded_slots / VISIBILITY_THREADS, 1],
        )?;
        for (index, (group, output)) in self.groups.iter().zip(&mut *arguments).enumerate() {
            *output = if let Some(geometry) = geometry(index) {
                Some(graph.encode_indexed_indirect(
                    ENCODE_PASS,
                    &group.table,
                    geometry.vertices,
                    geometry.indices,
                    mask,
                    group.mask_offset,
                )?)
            } else {
                None
            };
        }
        Ok(VisibilityFrame { mask, arguments })
    }
}

fn bounds_record(WorldBounds { min, max, enabled }: WorldBounds) -> [u8; VISIBILITY_AABB_BYTES] {
    let words = [
        min[0].to_bits(),
        min[1].to_bits(),
        min[2].to_bits(),
        u32::from(enabled),
        max[0].to_bits(),
        max[1].to_bits(),
        max[2].to_bits(),
        0,
    ];
    let mut record = [0; VISIBILITY_AABB_BYTES];
    record.copy_from_slice(words.map(u32::to_le_bytes).as_flattened());
    record
}

fn binding(binding: u32, buffer: BufferHandle, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size,
        },
    }
}
