use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use renderer::graph::{BufferDesc, BufferHandle, Graph, IndexedIndirectArgs, IndexedIndirectTable};

use super::{PipelineError, PipelineRequirements, Resource, ResourceDesc, frame::named};

/// An immutable indexed command table and its offset into the encoder's
/// visibility buffer, in bytes. The buffer holds one u32 per padded entry.
#[derive(Clone)]
pub struct IndirectTable {
    pub table: IndexedIndirectTable,
    pub visibility_offset: u64,
}

impl fmt::Debug for IndirectTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndirectTable")
            .field("command_count", &self.table.command_count())
            .field("visibility_offset", &self.visibility_offset)
            .finish_non_exhaustive()
    }
}

/// The ordered tables for a YAML encoder's `table_list`. Preparation retains
/// the immutable tables for subsequent frames.
#[derive(Clone, Copy, Debug)]
pub struct PrepareIndirectList<'a> {
    pub name: &'a str,
    pub tables: &'a [IndirectTable],
}

/// Geometry and material inputs for one prepared table in this Graph.
#[derive(Clone, Copy)]
pub struct FrameIndirectGroup {
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    /// Index into the frame's materials when the raster batch uses materials.
    pub material: Option<usize>,
    /// Whether to encode and draw this table in the current Graph.
    pub enabled: bool,
}

/// Graph-local inputs for a named table list, in the same order as the
/// corresponding prepared tables.
#[derive(Clone, Copy)]
pub struct FrameIndirectList<'a> {
    pub name: &'a str,
    pub groups: &'a [FrameIndirectGroup],
}

#[derive(Default)]
pub(super) struct IndirectEncoders {
    ranges: HashMap<String, Range<usize>>,
    groups: usize,
}

impl IndirectEncoders {
    pub(super) fn groups(&self) -> usize {
        self.groups
    }

    pub(super) fn range(
        &self,
        pass_id: &str,
        encoder: &str,
    ) -> Result<Range<usize>, PipelineError> {
        self.ranges.get(encoder).cloned().ok_or_else(|| {
            PipelineError::Invalid(format!(
                "pass {pass_id:?}: {encoder:?} must name an earlier indexed indirect encoder"
            ))
        })
    }

    pub(super) fn prepare(
        &mut self,
        pass_id: &str,
        visibility: usize,
        table_list: String,
        resources: &[Resource],
        lists: &[PrepareIndirectList<'_>],
    ) -> Result<PreparedIndirectEncoder, PipelineError> {
        let ResourceDesc::Buffer(BufferDesc { size }) = resources[visibility].desc else {
            return Err(PipelineError::Invalid(format!(
                "pass {pass_id:?}: visibility must name a buffer"
            )));
        };
        let list = named(lists, &table_list, "prepared indirect table list", |list| {
            list.name
        })
        .map_err(|error| PipelineError::Invalid(format!("pass {pass_id:?}: {error}")))?;
        for (index, group) in list.tables.iter().enumerate() {
            let bytes = u64::from(group.table.padded_count()) * size_of::<u32>() as u64;
            if !group
                .visibility_offset
                .is_multiple_of(size_of::<u32>() as u64)
                || group
                    .visibility_offset
                    .checked_add(bytes)
                    .is_none_or(|end| end > size)
            {
                return Err(PipelineError::Invalid(format!(
                    "pass {pass_id:?}: table {index} in {table_list:?} needs an aligned visibility range within buffer {:?}",
                    resources[visibility].id
                )));
            }
        }
        let end = self.groups.checked_add(list.tables.len()).ok_or_else(|| {
            PipelineError::Invalid("pipeline indirect group count overflow".into())
        })?;
        let range = self.groups..end;
        self.ranges.insert(pass_id.into(), range.clone());
        self.groups = end;
        Ok(PreparedIndirectEncoder {
            visibility,
            table_list,
            tables: list.tables.to_vec(),
            range,
        })
    }
}

pub(super) struct PreparedIndirectEncoder {
    pub(super) visibility: usize,
    table_list: String,
    tables: Vec<IndirectTable>,
    range: Range<usize>,
}

impl PreparedIndirectEncoder {
    pub(super) fn transient_buffers(&self) -> impl Iterator<Item = BufferDesc> + '_ {
        self.tables.iter().flat_map(|group| {
            [
                group.table.template_buffer_desc(),
                group.table.command_buffer_desc(),
            ]
        })
    }

    pub(super) fn measure(
        &self,
        pass_id: &str,
        requirements: &mut PipelineRequirements,
    ) -> Result<(), PipelineError> {
        let count = self.tables.len();
        // Every group appends a template upload and an encoder dispatch. Allow
        // for the two hidden buffer names and the template upload name.
        const INTERNAL_NAMES: usize = 64;
        for (total, per_group) in [
            (&mut requirements.resources, 2),
            (&mut requirements.passes, 2),
            (&mut requirements.compute_dispatches, 1),
            (&mut requirements.bindings, 3),
            (&mut requirements.accesses, 1),
            (&mut requirements.name_bytes, pass_id.len() + INTERNAL_NAMES),
        ] {
            *total = count
                .checked_mul(per_group)
                .and_then(|value| total.checked_add(value))
                .ok_or_else(|| {
                    PipelineError::Invalid("pipeline Graph declaration limits overflow".into())
                })?;
        }
        Ok(())
    }

    pub(super) fn encode(
        &self,
        graph: &mut Graph,
        pass_id: &str,
        visibility: BufferHandle,
        inputs: &[FrameIndirectList<'_>],
        encoded: &mut Vec<Option<EncodedGroup>>,
    ) -> Result<(), PipelineError> {
        let input = named(
            inputs,
            &self.table_list,
            "frame indirect table list",
            |list| list.name,
        )
        .map_err(|error| PipelineError::Invalid(format!("pass {pass_id:?}: {error}")))?;
        if input.groups.len() != self.tables.len() {
            return Err(PipelineError::Invalid(format!(
                "pass {pass_id:?}: {} frame indirect groups for {} prepared tables",
                input.groups.len(),
                self.tables.len()
            )));
        }
        debug_assert_eq!(encoded.len(), self.range.start);
        for (table, group) in self.tables.iter().zip(input.groups) {
            if !group.enabled {
                encoded.push(None);
                continue;
            }
            let args = graph.encode_indexed_indirect(
                pass_id,
                &table.table,
                group.vertices,
                group.indices,
                visibility,
                table.visibility_offset,
            )?;
            encoded.push(Some(EncodedGroup {
                args,
                material: group.material,
            }));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct EncodedGroup {
    pub(super) args: IndexedIndirectArgs,
    pub(super) material: Option<usize>,
}

#[cfg(test)]
mod tests {
    use renderer::graph::{GraphCapacity, GraphPool, IndexedIndirectTemplate};
    use renderer::program::ManagedRasterProgram;

    use super::*;
    use crate::yaml::{
        FrameInputs, FrameMaterial, FrameResource, PrepareInputs, PrepareOptions, PreparedPipeline,
        Program,
    };

    const DEFINITION: &str = "
version: 1
resources:
  - {id: target, kind: image, width: 2, height: 2, format: rgba8_unorm}
  - {id: mask, kind: buffer, source: external, bytes: 512}
raster_batches:
  mesh:
    - indirect: encode
      shader_template: example
      material_types: [opaque]
      constants: [1065353216, 0, 0, 0, 0, 1065353216, 0, 0, 0, 0, 1065353216, 0, 0, 0, 0, 1065353216, 1065353216, 1065353216, 1065353216, 1065353216]
    - indirect: encode
      shader: fixed
      constants: [1065353216, 0, 0, 0, 0, 1065353216, 0, 0, 0, 0, 1065353216, 0, 0, 0, 0, 1065353216, 1065353216, 1065353216, 1065353216, 1065353216]
passes:
  - {id: encode, kind: encode_indexed_indirect, visibility: mask, table_list: mesh_tables}
  - id: draw
    kind: raster
    batches: mesh
    colors: [{image: target, load: clear, clear: [0, 0, 0, 1]}]
output: target
";

    fn prepare(definition: &str, offsets: [u64; 2]) -> Result<PreparedPipeline, PipelineError> {
        let table = IndexedIndirectTable::new(&[IndexedIndirectTemplate {
            first_index: 0,
            index_count: 3,
        }])
        .unwrap();
        let tables = offsets.map(|visibility_offset| IndirectTable {
            table: table.clone(),
            visibility_offset,
        });
        PreparedPipeline::from_yaml_fragments(
            &[definition],
            PrepareOptions {
                frame_extent: None,
                frames_in_flight: 2,
                max_draws_per_list: 0,
                max_materials: 1,
            },
            PrepareInputs {
                indirect_lists: &[PrepareIndirectList {
                    name: "mesh_tables",
                    tables: &tables,
                }],
                ..PrepareInputs::default()
            },
            |_| Ok(Program::Raster(ManagedRasterProgram::mesh())),
        )
    }

    #[test]
    fn rejects_invalid_encoder_link_and_visibility_range_during_preparation() {
        for offsets in [[0, 1], [0, 260], [0, u64::MAX - 3]] {
            assert!(
                prepare(DEFINITION, offsets)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("aligned visibility range")
            );
        }
        let definition = DEFINITION.replace("indirect: encode", "indirect: draw");
        assert!(
            prepare(&definition, [0, 256])
                .err()
                .unwrap()
                .to_string()
                .contains("earlier indexed indirect encoder")
        );
    }

    #[test]
    fn disabled_groups_skip_declarations_and_later_frames_reuse_scratch() {
        let mut pipeline = prepare(DEFINITION, [0, 256]).unwrap();
        let requirements = pipeline.requirements();
        let working_set = pipeline.working_set();
        assert_eq!(requirements.resources, 5);
        assert_eq!(requirements.passes, 5);
        assert_eq!(requirements.draws, 4);
        assert_eq!(working_set.compute.len(), 1);
        assert_eq!(
            working_set
                .resources
                .iter()
                .filter_map(|class| match class {
                    renderer::capacity::ResourceClass::Buffer { count, .. } => Some(count),
                    _ => None,
                })
                .sum::<usize>(),
            8
        );
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: requirements.resources + 3,
                passes: requirements.passes + 3,
                draws: requirements.draws,
                accesses: requirements.accesses + 3,
                bindings: requirements.bindings,
                attachments: requirements.attachments,
                raster_constant_words: Some(requirements.raster_constant_words),
                name_bytes: requirements.name_bytes + 32,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        for enabled in [false, true] {
            let mut graph = pool.begin().unwrap();
            let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
            let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
            let mask = graph.buffer("mask", BufferDesc { size: 512 }).unwrap();
            graph.upload_buffer("v", vertices, 0, &[0; 84]).unwrap();
            graph.upload_buffer("i", indices, 0, &[0; 12]).unwrap();
            graph.upload_buffer("m", mask, 0, &[0; 512]).unwrap();
            let groups = [
                FrameIndirectGroup {
                    vertices,
                    indices,
                    material: Some(0),
                    enabled: true,
                },
                FrameIndirectGroup {
                    vertices,
                    indices,
                    material: Some(0),
                    enabled,
                },
            ];
            let (mut graph, output) = pipeline
                .build(
                    graph,
                    FrameInputs {
                        resources: &[FrameResource {
                            name: "mask",
                            resource: mask.into(),
                        }],
                        materials: &[FrameMaterial {
                            material_type: "opaque",
                            bindings: 0..0,
                        }],
                        indirect_lists: &[FrameIndirectList {
                            name: "mesh_tables",
                            groups: &groups,
                        }],
                        ..FrameInputs::default()
                    },
                )
                .unwrap();
            graph.export_image(output.image()).unwrap();
            let plan = graph.compile().unwrap();
            assert_eq!(
                plan.ordered_pass_names()
                    .filter(|pass| *pass == "encode")
                    .count(),
                1 + usize::from(enabled)
            );
            assert_eq!(plan.ordered_pass_names().last(), Some("draw"));
        }
    }
}
