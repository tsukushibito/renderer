//! Indexed command templates and typed GPU-generated draw arguments.

use super::{
    Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError, GraphErrorKind,
};
use crate::program::{
    INDIRECT_COMMAND_BYTES, INDIRECT_GROUP_SIZE, INDIRECT_MASK_BYTES, INDIRECT_TEMPLATE_BYTES,
    MAX_INDIRECT_GROUPS, ManagedProgram,
};
use std::sync::Arc;

/// One immutable triangle-list index range in an indirect draw table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedIndirectTemplate {
    pub first_index: u32,
    pub index_count: u32,
}

/// Immutable command ranges prepared outside the frame loop. Keep the table
/// alive while regularly submitting graphs built from it so the shared upload
/// bytes remain anchored when a plan is retired or its slot is reused.
#[derive(Clone)]
pub struct IndexedIndirectTable {
    templates: Arc<[u8]>,
    command_count: u32,
    groups: u32,
    max_index_end: u32,
}

impl IndexedIndirectTable {
    pub fn new(commands: &[IndexedIndirectTemplate]) -> Result<Self, GraphError> {
        let command_count = u32::try_from(commands.len())
            .ok()
            .filter(|count| (1..=INDIRECT_GROUP_SIZE * MAX_INDIRECT_GROUPS).contains(count))
            .ok_or_else(|| {
                GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "indexed indirect table needs a nonempty supported command count",
                )
            })?;
        let mut max_index_end = 0;
        for command in commands {
            let end = command
                .first_index
                .checked_add(command.index_count)
                .filter(|_| command.index_count != 0 && command.index_count.is_multiple_of(3))
                .ok_or_else(|| {
                    GraphError::new(
                        GraphErrorKind::InvalidInput,
                        "indexed indirect templates require a representable nonempty triangle index range",
                    )
                })?;
            max_index_end = max_index_end.max(end);
        }
        let groups = command_count.div_ceil(INDIRECT_GROUP_SIZE);
        // The checked group count limits the addressable template bytes to
        // roughly 32 MiB on both supported host and shader ABIs.
        let bytes =
            (u64::from(groups) * u64::from(INDIRECT_GROUP_SIZE) * INDIRECT_TEMPLATE_BYTES) as usize;
        let mut templates = Vec::with_capacity(bytes);
        for command in commands {
            templates.extend_from_slice(&command.first_index.to_le_bytes());
            templates.extend_from_slice(&command.index_count.to_le_bytes());
        }
        templates.resize(bytes, 0);
        Ok(Self {
            templates: templates.into(),
            command_count,
            groups,
            max_index_end,
        })
    }

    /// Number of commands read by native indexed indirect execution.
    pub const fn command_count(&self) -> u32 {
        self.command_count
    }

    /// Number of 32-bit visibility entries consumed when encoding the table.
    /// The padding must also be initialized. Its values do not affect drawing.
    pub const fn padded_count(&self) -> u32 {
        self.groups * INDIRECT_GROUP_SIZE
    }

    /// Description of the transient upload buffer to include in preparation.
    pub fn template_buffer_desc(&self) -> BufferDesc {
        BufferDesc {
            size: self.templates.len() as u64,
        }
    }

    /// Description of the transient GPU command buffer to include in preparation.
    pub fn command_buffer_desc(&self) -> BufferDesc {
        BufferDesc {
            size: u64::from(self.padded_count()) * INDIRECT_COMMAND_BYTES,
        }
    }
}

/// Opaque arguments from a single Graph checkout. The encoded buffer has no
/// public handle, so later user declarations cannot alter its command words.
#[derive(Clone, Copy, Debug)]
pub struct IndexedIndirectArgs {
    pub(super) buffer: BufferHandle,
    pub(super) vertices: BufferHandle,
    pub(super) indices: BufferHandle,
    pub(super) command_count: u32,
    pub(super) max_index_end: u32,
}

impl Graph {
    /// Encode ordered indexed draw arguments from a cold table and an
    /// initialized range of 32-bit visibility entries. Every nonzero entry
    /// produces one instance and every zero entry produces none. Vertex and
    /// index buffers stay associated with the opaque result.
    ///
    /// The caller supplies the visibility producer and must reserve the two
    /// temporary buffer classes and prepare the portable encoder. Native
    /// storage-buffer alignment is verified while preparing the frame.
    pub fn encode_indexed_indirect(
        &mut self,
        name: &str,
        table: &IndexedIndirectTable,
        vertices: BufferHandle,
        indices: BufferHandle,
        visibility: BufferHandle,
        visibility_offset: u64,
    ) -> Result<IndexedIndirectArgs, GraphError> {
        self.buffer_id(vertices)?;
        self.buffer_range(
            self.buffer_id(indices)?,
            0,
            u64::from(table.max_index_end) * 4,
            true,
        )?;
        let visibility_size = u64::from(table.padded_count()) * INDIRECT_MASK_BYTES;
        self.buffer_range(
            self.buffer_id(visibility)?,
            visibility_offset,
            visibility_size,
            true,
        )?;

        // Only transient buffers and two passes are appended before publishing
        // the token. Restore the whole group if a later reservation fails.
        let resources = self.resources.len();
        let passes = self.passes.len();
        let accesses = self.accesses.len();
        let names = self.names.len();
        let result = (|| {
            let template_desc = table.template_buffer_desc();
            let command_desc = table.command_buffer_desc();
            let template = self.buffer("indirect templates", template_desc)?;
            let buffer = self.buffer("indirect arguments", command_desc)?;
            self.upload_buffer_slice(
                "upload indirect templates",
                template,
                0,
                Arc::clone(&table.templates),
                0..table.templates.len(),
            )?;
            self.dispatch_buffer_grid(
                name,
                &ManagedProgram::indexed_indirect_encoder(),
                &[
                    indirect_binding(0, template, 0, template_desc.size),
                    indirect_binding(1, visibility, visibility_offset, visibility_size),
                    indirect_binding(2, buffer, 0, command_desc.size),
                ],
                [table.groups, 1],
            )?;
            Ok(IndexedIndirectArgs {
                buffer,
                vertices,
                indices,
                command_count: table.command_count,
                max_index_end: table.max_index_end,
            })
        })();
        if result.is_err() {
            // Individual passes already rolled back their own storage. A
            // successful upload adds no bindings or extra upload arena bytes.
            let data = self.data_mut();
            data.resources.truncate(resources);
            data.exported.truncate(resources);
            data.passes.truncate(passes);
            data.accesses.truncate(accesses);
            data.names.truncate(names);
        }
        result
    }
}

fn indirect_binding(binding: u32, buffer: BufferHandle, offset: u64, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset,
            size,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        AccessPurpose, Attachment, GraphCapacity, GraphCapacityKind, GraphPool, ImageDesc, LoadOp,
        Operation, RasterAttachments, RasterParameters, StoreOp,
    };
    use crate::program::ManagedRasterProgram;

    fn template(first_index: u32, index_count: u32) -> IndexedIndirectTemplate {
        IndexedIndirectTemplate {
            first_index,
            index_count,
        }
    }

    #[test]
    fn tables_validate_ranges_and_pad_the_encoding_to_complete_workgroups() {
        assert!(IndexedIndirectTable::new(&[]).is_err());
        for invalid in [template(0, 0), template(0, 4), template(u32::MAX, 3)] {
            assert!(IndexedIndirectTable::new(&[invalid]).is_err());
        }
        let table = IndexedIndirectTable::new(&[template(7, 3), template(2, 6)]).unwrap();
        assert_eq!(table.command_count(), 2);
        assert_eq!(table.padded_count(), 64);
        assert_eq!(table.max_index_end, 10);
        assert_eq!(
            &table.templates[..16],
            &[7, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 6, 0, 0, 0]
        );
        assert!(table.templates[16..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn indirect_draw_retains_encoder_and_exact_mask_range_with_only_real_commands_read() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                raster_constant_words: Some(20),
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let table = IndexedIndirectTable::new(&[template(1, 3); 65]).unwrap();
        let mut graph = pool.begin().unwrap();
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 16 }).unwrap();
        let visibility = graph
            .buffer("visibility", BufferDesc { size: 1024 })
            .unwrap();
        graph
            .upload_buffer("vertices", vertices, 0, &[0; 84])
            .unwrap();
        graph
            .upload_buffer("indices", indices, 0, &[0; 16])
            .unwrap();
        graph
            .upload_buffer("visibility", visibility, 256, &[0; 512])
            .unwrap();
        let args = graph
            .encode_indexed_indirect("encode", &table, vertices, indices, visibility, 256)
            .unwrap();
        let output = graph.image("color", ImageDesc::rgba8(2, 2)).unwrap();
        let view = graph.image_view(output, 0, 0).unwrap();
        let constants = RasterParameters::default().into();
        let pass = graph
            .raster(
                "draw",
                RasterAttachments {
                    colors: &[Attachment {
                        view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: None,
                },
                |raster| raster.draw_indirect(&ManagedRasterProgram::mesh(), args, None, constants),
            )
            .unwrap();
        graph.readback_image("color pixels", output).unwrap();
        let plan = graph.compile().unwrap();
        assert!(plan.ordered_pass_names().eq([
            "vertices",
            "indices",
            "visibility",
            "upload indirect templates",
            "encode",
            "draw",
            "color pixels",
        ]));
        let Operation::Raster { batches, .. } = &plan.passes[pass.index].operation else {
            panic!("indirect Raster operation");
        };
        let [batch] = plan.raster_batches(batches) else {
            panic!("one indirect batch");
        };
        let indirect = batch.indirect.unwrap();
        assert_eq!(indirect.command_count, 65);
        assert_eq!(indirect.buffer, args.buffer.index);
        assert_eq!(
            plan.raster_draws(&batch.draws)[0]
                .indexed
                .unwrap()
                .index_count,
            4
        );
        assert!(plan.raster_constants(batch).eq([constants.words()]));
        assert!(plan.accesses(pass.index).iter().any(|access| {
            access.purpose == AccessPurpose::Indirect
                && access.resource == indirect.buffer
                && access.range == (0..1300)
        }));
        assert!(plan.accesses(pass.index - 1).iter().any(|access| {
            access.purpose == AccessPurpose::StorageRead
                && access.resource == visibility.index
                && access.range == (256..768)
        }));
    }

    #[test]
    fn failed_encoder_reservation_rolls_back_the_template_pass_and_hidden_buffers() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: 5,
                passes: 1,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let table = IndexedIndirectTable::new(&[template(0, 3)]).unwrap();
        let mut graph = pool.begin().unwrap();
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        let visibility = graph
            .buffer("visibility", BufferDesc { size: 256 })
            .unwrap();
        let export_capacity = graph.exported.capacity();
        for _ in 0..64 {
            let err = graph
                .encode_indexed_indirect("encode", &table, vertices, indices, visibility, 0)
                .unwrap_err();
            assert_eq!(err.capacity().unwrap().kind, GraphCapacityKind::Passes);
            assert_eq!(graph.exported.len(), graph.resources.len());
        }
        let result = graph
            .buffer("remaining buffer", BufferDesc { size: 4 })
            .unwrap();
        graph
            .buffer("remaining slot", BufferDesc { size: 4 })
            .unwrap();
        graph
            .upload_buffer("retained upload", result, 0, &[0; 4])
            .unwrap();
        graph.export_buffer(result).unwrap();
        assert_eq!(graph.exported.capacity(), export_capacity);
        assert!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .eq(["retained upload"])
        );
    }

    #[test]
    fn arguments_are_tied_to_one_graph_checkout() {
        let table = IndexedIndirectTable::new(&[template(0, 3)]).unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let args = {
            let mut graph = pool.begin().unwrap();
            let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
            let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
            let visibility = graph
                .buffer("visibility", BufferDesc { size: 256 })
                .unwrap();
            graph
                .encode_indexed_indirect("encode", &table, vertices, indices, visibility, 0)
                .unwrap()
        };
        let mut next = pool.begin().unwrap();
        let target = next.image("color", ImageDesc::rgba8(1, 1)).unwrap();
        let view = next.image_view(target, 0, 0).unwrap();
        let error = next
            .raster(
                "foreign arguments",
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
                    raster.draw_indirect(
                        &ManagedRasterProgram::mesh(),
                        args,
                        None,
                        RasterParameters::default().into(),
                    )
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    }
}
