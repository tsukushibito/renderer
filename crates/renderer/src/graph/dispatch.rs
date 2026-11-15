//! Owned managed Compute operands and their shader-derived logical accesses.

use crate::program::{BindingType, BufferRange, ManagedProgram};

use super::{
    AccessPurpose, AccessRange, BufferHandle, Graph, GraphCapacityKind, GraphError, GraphErrorKind,
    Operation, PassHandle, ResourceAccess, storage::capacity_error,
};

/// A graph-local buffer descriptor, borrowed only during [`Graph::dispatch`].
/// The Program determines its use; callers do not repeat read/write declarations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferBinding {
    pub group: u32,
    pub binding: u32,
    pub buffer: BufferHandle,
    pub offset: u64,
    pub size: u64,
}

/// Full descriptor operands in manifest order, distinct from the shader's
/// smaller read/write footprint used by dataflow and hazard analysis.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "native managed dispatch is subsequent work")
)]
pub(crate) struct BufferBindingOperands {
    pub resource: usize,
    pub range: BufferRange,
}

impl Graph {
    /// Own one managed Compute dispatch and derive accesses from its reviewed
    /// shader contract. Currently supports the buffer-only uniform/storage
    /// readback assets with exactly `[1, 1, 1]` workgroups.
    ///
    /// The complete descriptor ranges must fit their buffers; only the bytes
    /// actually written by the shader become initialized. Inputs may be dropped
    /// or changed after this call. Device alignment/usage/limit checks belong to
    /// native preparation. Native execution of this operation is not yet supported.
    pub fn dispatch(
        &mut self,
        name: &str,
        program: &ManagedProgram,
        bindings: &[BufferBinding],
        workgroups: [u32; 3],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let contract = program.buffer_dispatch().ok_or_else(|| {
                GraphError::new(
                    GraphErrorKind::Unsupported,
                    "Program has no buffer dispatch contract",
                )
            })?;
            if workgroups != contract.workgroups {
                return Err(GraphError::new(
                    GraphErrorKind::Unsupported,
                    "workgroup counts differ from the reviewed Program dispatch contract",
                ));
            }
            let layout = program.bindings();
            if bindings.len() != layout.len() {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "bindings must cover the complete Program layout",
                ));
            }
            let first_binding = graph.bindings.len();
            let required = first_binding.checked_add(layout.len());
            if required.is_none_or(|required| required > graph.capacity.bindings) {
                return Err(capacity_error(
                    GraphCapacityKind::Bindings,
                    graph.capacity.bindings,
                    required,
                ));
            }
            let first_access = graph.accesses.len();
            for (slot, &bytes) in layout.iter().zip(contract.bytes) {
                let mut matches = bindings
                    .iter()
                    .filter(|input| input.group == slot.group && input.binding == slot.binding);
                let input = matches.next().ok_or_else(|| {
                    GraphError::new(GraphErrorKind::InvalidInput, "missing Program binding slot")
                        .with_binding(slot.group, slot.binding)
                })?;
                if matches.next().is_some() {
                    return Err(GraphError::new(
                        GraphErrorKind::InvalidInput,
                        "duplicate Program binding slot",
                    )
                    .with_binding(slot.group, slot.binding));
                }
                let result = (|| {
                    let resource = graph.buffer_id(input.buffer)?;
                    let descriptor =
                        graph.buffer_range(resource, input.offset, input.size, false)?;
                    if input.size < slot.min_size {
                        return Err(graph
                            .resource_error(
                                GraphErrorKind::InvalidInput,
                                "buffer descriptor is smaller than the Program ABI",
                                resource,
                            )
                            .with_range(AccessRange::Buffer(descriptor)));
                    }
                    // This private contract is reviewed together with the shader,
                    // not inferred from min_size or supplied by an API caller.
                    let end = input
                        .offset
                        .checked_add(bytes)
                        .filter(|&end| end <= descriptor.end)
                        .expect("reviewed footprint fits the Program minimum descriptor size");
                    let (read, write, purpose) = match slot.kind {
                        BindingType::Uniform => (true, false, AccessPurpose::Uniform),
                        BindingType::StorageRead => (true, false, AccessPurpose::StorageRead),
                        BindingType::StorageWrite => (false, true, AccessPurpose::StorageWrite),
                        _ => unreachable!("buffer dispatch contract contains only buffer slots"),
                    };
                    graph.append_access(ResourceAccess {
                        resource,
                        range: input.offset..end,
                        read,
                        write,
                        initializes: write,
                        discard_after: false,
                        purpose,
                    })?;
                    graph.data_mut().bindings.push(BufferBindingOperands {
                        resource,
                        range: BufferRange {
                            offset: input.offset,
                            size: input.size,
                        },
                    });
                    Ok(())
                })();
                result.map_err(|error: GraphError| error.with_binding(slot.group, slot.binding))?;
            }
            let accesses = first_access..graph.accesses.len();
            let conflict = {
                let data = graph.data_mut();
                super::accesses::conflicting_accesses_in(
                    &data.accesses[accesses.clone()],
                    |access| (access.resource, access.range.clone(), access.write),
                    &mut data.overlap,
                )
                .map_err(|_| {
                    capacity_error(
                        GraphCapacityKind::Accesses,
                        data.capacity.accesses,
                        Some(accesses.len()),
                    )
                })?
            };
            if let Some((left, _)) = conflict {
                let access = &graph.accesses[accesses.start + left];
                let slot = layout[left];
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping dispatch read/write footprints are unsupported",
                        access.resource,
                    )
                    .with_range(AccessRange::Buffer(access.range.clone()))
                    .with_binding(slot.group, slot.binding));
            }
            Ok((
                Operation::Dispatch {
                    program: *program,
                    bindings: first_binding..graph.bindings.len(),
                    workgroups,
                },
                accesses,
                false,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{BufferDesc, GraphCapacity, GraphPool};

    #[test]
    fn reviewed_buffer_footprints_cover_each_slot_and_fit_the_abi() {
        for program in [
            ManagedProgram::uniform_readback(),
            ManagedProgram::storage_readback(),
        ] {
            let contract = program.buffer_dispatch().unwrap();
            assert_eq!(contract.workgroups, [1, 1, 1]);
            assert_eq!(contract.bytes.len(), program.bindings().len());
            for (slot, &bytes) in program.bindings().iter().zip(contract.bytes) {
                assert!(bytes > 0 && bytes <= slot.min_size);
                assert!(matches!(
                    slot.kind,
                    BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite
                ));
            }
        }
    }

    #[test]
    fn plan_retains_program_and_complete_canonical_descriptors_separately_from_footprints() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let source = graph.buffer("uniform", BufferDesc { size: 64 }).unwrap();
        let output = graph.buffer("output", BufferDesc { size: 64 }).unwrap();
        graph.upload_buffer("seed", source, 16, &[0; 16]).unwrap();
        let dispatch = {
            let bindings = [
                BufferBinding {
                    group: 1,
                    binding: 0,
                    buffer: output,
                    offset: 32,
                    size: 32,
                },
                BufferBinding {
                    group: 0,
                    binding: 0,
                    buffer: source,
                    offset: 16,
                    size: 48,
                },
            ];
            graph
                .dispatch(
                    "dispatch",
                    &ManagedProgram::uniform_readback(),
                    &bindings,
                    [1; 3],
                )
                .unwrap()
        };
        graph.readback_buffer("result", output, 32, 16).unwrap();
        let plan = graph.compile().unwrap();
        let Operation::Dispatch {
            program,
            bindings,
            workgroups,
        } = &plan.passes[dispatch.index].operation
        else {
            panic!("expected the owned dispatch operation");
        };
        assert_eq!(program.bindings()[0].kind, BindingType::Uniform);
        assert_eq!(*workgroups, [1, 1, 1]);
        let operands = &plan.bindings[bindings.clone()];
        assert_eq!(operands.len(), 2);
        assert_eq!(operands[0].resource, source.index);
        assert_eq!(
            operands[0].range,
            BufferRange {
                offset: 16,
                size: 48
            }
        );
        assert_eq!(operands[1].resource, output.index);
        assert_eq!(
            operands[1].range,
            BufferRange {
                offset: 32,
                size: 32
            }
        );
        let accesses = plan.accesses(dispatch.index);
        assert_eq!(accesses[0].range, 16..32);
        assert_eq!(accesses[0].purpose, AccessPurpose::Uniform);
        assert_eq!(accesses[1].range, 32..48);
        assert_eq!(accesses[1].purpose, AccessPurpose::StorageWrite);
    }
}
