//! Owned managed shader bindings and their reviewed logical accesses.

use super::{
    AccessPurpose, BufferHandle, Graph, GraphCapacityKind, GraphError, GraphErrorKind, ImageView,
    Operation, PassHandle, ResourceAccess, storage::capacity_error,
};
use crate::program::{
    BindingLayout, BindingType, BufferRange, DispatchWorkgroups, ManagedProgram, SamplerDescriptor,
};

/// A graph-local descriptor borrowed only during pass construction. Programs
/// derive resource uses; callers do not repeat a read/write or stage declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    pub group: u32,
    pub binding: u32,
    pub resource: BindingResource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingResource {
    Buffer {
        buffer: BufferHandle,
        offset: u64,
        size: u64,
    },
    ImageView(ImageView),
    /// Immutable sampler settings are copied into the owned plan. This is a
    /// logical sampler, not a native object or a separate content resource.
    Sampler(SamplerDescriptor),
}

/// Complete canonical descriptor operands, distinct from shader footprints.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(any(test, feature = "vulkan", all(feature = "d3d12", windows))),
    allow(
        dead_code,
        reason = "native operands are retained but not consumed by the CPU-only library"
    )
)]
pub(crate) enum BindingOperands {
    Buffer { resource: usize, range: BufferRange },
    ImageView(ImageView),
    Sampler(SamplerDescriptor),
}

impl Graph {
    /// Own a managed Compute dispatch and derive accesses from its reviewed
    /// shader contract. Buffer footprints are reviewed prefixes; images access
    /// the whole selected mip/layer. Extra descriptor bytes stay undefined.
    /// Image-domain dispatches must cover their output exactly after ceil
    /// division by the local size, with guarded fringe invocations. Native
    /// preparation establishes Device alignment, usage and limit guarantees.
    pub fn dispatch(
        &mut self,
        name: &str,
        program: &ManagedProgram,
        bindings: &[Binding],
        workgroups: [u32; 3],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let first_access = graph.accesses.len();
            let owned = graph.append_bindings(
                program.bindings(),
                program.dispatch_contract().bytes,
                bindings,
            )?;
            graph.validate_dispatch_domain(program, owned.clone(), workgroups)?;
            let accesses = first_access..graph.accesses.len();
            if let Some(left) = graph.conflicting_pass_access(accesses.clone())? {
                let access = &graph.accesses[accesses.start + left];
                // Samplers do not append a content access, so resolve its slot
                // from the reviewed non-sampler ordering rather than arena index.
                let slot = program
                    .bindings()
                    .iter()
                    .filter(|slot| slot.kind != BindingType::Sampler)
                    .nth(left)
                    .unwrap();
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping dispatch read/write footprints are unsupported",
                        access.resource,
                    )
                    .with_range(graph.access_range(access.resource, access.range.clone()))
                    .with_binding(slot.group, slot.binding));
            }
            Ok((
                Operation::Dispatch {
                    program: *program,
                    bindings: owned,
                    workgroups,
                },
                accesses,
                false,
            ))
        })
    }

    fn validate_dispatch_domain(
        &self,
        program: &ManagedProgram,
        owned: std::ops::Range<usize>,
        workgroups: [u32; 3],
    ) -> Result<(), GraphError> {
        self.validate_dispatch_image_layouts(program.bindings(), owned.clone())?;
        self.validate_dispatch_workgroups(program, owned, workgroups)
    }

    fn validate_dispatch_image_layouts(
        &self,
        layout: &[BindingLayout],
        owned: std::ops::Range<usize>,
    ) -> Result<(), GraphError> {
        // A sampled descriptor uses a read-only native image layout, while a
        // storage descriptor uses GENERAL. Even two reads cannot give the same
        // View those two layouts within one dispatch. Multiple storage reads
        // remain valid; their descriptors share the GENERAL layout.
        for (index, slot) in layout.iter().enumerate() {
            if slot.kind != BindingType::SampledImage {
                continue;
            }
            let BindingOperands::ImageView(sampled) = self.bindings[owned.start + index] else {
                unreachable!("canonical sampled image operand");
            };
            for (other_index, other_slot) in layout.iter().enumerate() {
                if matches!(
                    other_slot.kind,
                    BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_)
                ) && matches!(self.bindings[owned.start + other_index], BindingOperands::ImageView(view) if view == sampled)
                {
                    return Err(self
                        .resource_error(
                            GraphErrorKind::InvalidInput,
                            "one dispatch cannot sample and use storage for the same image View",
                            sampled.image.index,
                        )
                        .with_binding(other_slot.group, other_slot.binding));
                }
            }
        }
        Ok(())
    }

    fn validate_dispatch_workgroups(
        &self,
        program: &ManagedProgram,
        owned: std::ops::Range<usize>,
        workgroups: [u32; 3],
    ) -> Result<(), GraphError> {
        let expected = match program.dispatch_contract().workgroups {
            DispatchWorkgroups::Fixed(expected) => expected,
            DispatchWorkgroups::Image2d { group, binding } => {
                // The trusted descriptor identifies one unique write-only image
                // and a nonzero local size. Resolve already canonical operands,
                // rather than searching the caller's binding list again.
                let target = program
                    .bindings()
                    .iter()
                    .position(|slot| slot.group == group && slot.binding == binding)
                    .expect("trusted image dispatch target is a declared slot");
                let BindingOperands::ImageView(view) = self.bindings[owned.start + target] else {
                    unreachable!("trusted image dispatch target is a storage image");
                };
                let extent = self
                    .image_desc_at(view.image.index)?
                    .mip_extent(view.mip)
                    .expect("immutable View has a valid mip extent");
                for (slot, operand) in program.bindings().iter().zip(&self.bindings[owned]) {
                    if matches!(
                        slot.kind,
                        BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_)
                    ) {
                        let BindingOperands::ImageView(other) = *operand else {
                            unreachable!("canonical storage image operand");
                        };
                        let other_extent =
                            self.image_desc_at(other.image.index)?.mip_extent(other.mip);
                        if other_extent != Some(extent) {
                            return Err(self
                                .resource_error(
                                    GraphErrorKind::InvalidInput,
                                    "storage image extents differ from the dispatch domain",
                                    other.image.index,
                                )
                                .with_binding(slot.group, slot.binding));
                        }
                    }
                }
                let local = program.workgroup_size();
                [
                    extent[0].div_ceil(local[0]),
                    extent[1].div_ceil(local[1]),
                    1,
                ]
            }
        };
        if workgroups != expected {
            return Err(GraphError::new(
                GraphErrorKind::Unsupported,
                "workgroup counts differ from the reviewed Program dispatch contract",
            ));
        }
        Ok(())
    }

    /// Append canonical owned descriptors and shader-derived accesses using only
    /// prepared storage. The caller's build_pass transaction rolls back failures.
    pub(super) fn append_bindings(
        &mut self,
        layout: &[BindingLayout],
        bytes: &[u64],
        bindings: &[Binding],
    ) -> Result<std::ops::Range<usize>, GraphError> {
        if bindings.len() != layout.len() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "bindings must cover the complete Program layout",
            ));
        }
        let first = self.bindings.len();
        let required = first.checked_add(layout.len());
        if required.is_none_or(|required| required > self.capacity.bindings) {
            return Err(capacity_error(
                GraphCapacityKind::Bindings,
                self.capacity.bindings,
                required,
            ));
        }
        for (slot, &bytes) in layout.iter().zip(bytes) {
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
                let operand = match (slot.kind, input.resource) {
                    (
                        BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite,
                        BindingResource::Buffer {
                            buffer,
                            offset,
                            size,
                        },
                    ) => {
                        let resource = self.buffer_id(buffer)?;
                        let descriptor = self.buffer_range(resource, offset, size, false)?;
                        if size < slot.min_size {
                            return Err(self
                                .resource_error(
                                    GraphErrorKind::InvalidInput,
                                    "buffer descriptor is smaller than the Program ABI",
                                    resource,
                                )
                                .with_range(super::AccessRange::Buffer(descriptor)));
                        }
                        let end = offset
                            .checked_add(bytes)
                            .filter(|&end| end <= descriptor.end)
                            .expect("reviewed footprint fits the Program minimum descriptor size");
                        let (read, write, purpose) = match slot.kind {
                            BindingType::Uniform => (true, false, AccessPurpose::Uniform),
                            BindingType::StorageRead => (true, false, AccessPurpose::StorageRead),
                            BindingType::StorageWrite => (false, true, AccessPurpose::StorageWrite),
                            _ => unreachable!(),
                        };
                        self.append_access(ResourceAccess {
                            resource,
                            range: offset..end,
                            read,
                            write,
                            initializes: write,
                            discard_after: false,
                            purpose,
                            stages: slot.stages,
                        })?;
                        BindingOperands::Buffer {
                            resource,
                            range: BufferRange { offset, size },
                        }
                    }
                    (
                        BindingType::SampledImage
                        | BindingType::StorageImageRead(_)
                        | BindingType::StorageImageWrite(_),
                        BindingResource::ImageView(view),
                    ) => {
                        let resource = self.image_id(view.image)?;
                        let desc = self.image_desc_at(resource)?;
                        let (read, write, purpose) = match slot.kind {
                            BindingType::SampledImage => (true, false, AccessPurpose::Sampled),
                            BindingType::StorageImageRead(format)
                            | BindingType::StorageImageWrite(format) => {
                                let expected = match format {
                                    crate::program::ImageFormat::Rgba8Unorm => {
                                        super::ImageFormat::Rgba8Unorm
                                    }
                                    crate::program::ImageFormat::Rgba16Float => {
                                        super::ImageFormat::Rgba16Float
                                    }
                                    crate::program::ImageFormat::D32Float => {
                                        return Err(self.resource_error(
                                            GraphErrorKind::Unsupported,
                                            "depth storage image bindings are unsupported",
                                            resource,
                                        ));
                                    }
                                };
                                if desc.format != expected {
                                    return Err(self.resource_error(
                                        GraphErrorKind::InvalidInput,
                                        "storage image View format differs from the Program ABI",
                                        resource,
                                    ));
                                }
                                let write = matches!(slot.kind, BindingType::StorageImageWrite(_));
                                (
                                    !write,
                                    write,
                                    if write {
                                        AccessPurpose::StorageWrite
                                    } else {
                                        AccessPurpose::StorageRead
                                    },
                                )
                            }
                            _ => unreachable!(),
                        };
                        // The private immutable View already establishes its
                        // range and aspect. Sampling has no write side effect.
                        let unit = u64::from(view.layer) * u64::from(desc.mip_levels)
                            + u64::from(view.mip);
                        self.append_access(ResourceAccess {
                            resource,
                            range: unit..unit + 1,
                            read,
                            write,
                            initializes: write,
                            discard_after: false,
                            purpose,
                            stages: slot.stages,
                        })?;
                        BindingOperands::ImageView(view)
                    }
                    (BindingType::Sampler, BindingResource::Sampler(sampler)) => {
                        BindingOperands::Sampler(sampler)
                    }
                    _ => {
                        return Err(GraphError::new(
                            GraphErrorKind::InvalidInput,
                            "binding resource type differs from the Program slot",
                        ));
                    }
                };
                self.data_mut().bindings.push(operand);
                Ok(())
            })();
            result.map_err(|error: GraphError| error.with_binding(slot.group, slot.binding))?;
        }
        Ok(first..self.bindings.len())
    }

    pub(super) fn conflicting_pass_access(
        &mut self,
        accesses: std::ops::Range<usize>,
    ) -> Result<Option<usize>, GraphError> {
        let data = self.data_mut();
        super::accesses::conflicting_accesses_in(
            &data.accesses[accesses.clone()],
            |access| (access.resource, access.range.clone(), access.write),
            &mut data.overlap,
        )
        .map(|conflict| conflict.map(|(left, _)| left))
        .map_err(|_| {
            capacity_error(
                GraphCapacityKind::Accesses,
                data.capacity.accesses,
                Some(accesses.len()),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{BufferDesc, GraphCapacity, GraphPool};

    #[test]
    fn sampled_and_storage_reads_share_only_disjoint_image_subresources() {
        use crate::program::{ImageFormat, ShaderStages};
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let image = graph
            .image(
                "two layers",
                super::super::ImageDesc {
                    array_layers: 2,
                    ..super::super::ImageDesc::rgba16_float(3, 5)
                },
            )
            .unwrap();
        let first = graph.image_view(image, 0, 0).unwrap();
        let second = graph.image_view(image, 0, 1).unwrap();
        let mut layout = [
            BindingLayout {
                group: 0,
                binding: 0,
                kind: BindingType::SampledImage,
                stages: ShaderStages::COMPUTE,
                min_size: 0,
            },
            BindingLayout {
                group: 0,
                binding: 1,
                kind: BindingType::StorageImageRead(ImageFormat::Rgba16Float),
                stages: ShaderStages::COMPUTE,
                min_size: 0,
            },
        ];
        for (other, accepted) in [(first, false), (second, true)] {
            let operands = [first, other].map(BindingResource::ImageView);
            let input = std::array::from_fn::<_, 2, _>(|index| Binding {
                group: 0,
                binding: index as u32,
                resource: operands[index],
            });
            let owned = graph.append_bindings(&layout, &[0, 0], &input).unwrap();
            let result = graph.validate_dispatch_image_layouts(&layout, owned);
            if accepted {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().kind(), GraphErrorKind::InvalidInput);
            }
        }
        // Read/read aliasing is allowed when both descriptors use the same
        // storage image layout. Rejecting every duplicate image is too broad.
        layout[0].kind = layout[1].kind;
        let input = std::array::from_fn::<_, 2, _>(|index| Binding {
            group: 0,
            binding: index as u32,
            resource: BindingResource::ImageView(first),
        });
        let owned = graph.append_bindings(&layout, &[0, 0], &input).unwrap();
        graph
            .validate_dispatch_image_layouts(&layout, owned)
            .unwrap();
    }

    #[test]
    fn reviewed_dispatch_footprints_cover_each_slot_and_fit_the_abi() {
        for program in [
            ManagedProgram::uniform_readback(),
            ManagedProgram::storage_readback(),
            ManagedProgram::sampled_readback(),
            ManagedProgram::sampled_float_readback(),
        ] {
            let contract = program.dispatch_contract();
            assert_eq!(contract.workgroups, DispatchWorkgroups::Fixed([1, 1, 1]));
            assert_eq!(contract.bytes.len(), program.bindings().len());
            for (slot, &bytes) in program.bindings().iter().zip(contract.bytes) {
                assert_eq!(slot.stages, crate::program::ShaderStages::COMPUTE);
                match slot.kind {
                    BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite => {
                        assert!(bytes > 0 && bytes <= slot.min_size)
                    }
                    BindingType::SampledImage
                    | BindingType::StorageImageRead(_)
                    | BindingType::StorageImageWrite(_)
                    | BindingType::Sampler => assert_eq!(bytes, 0),
                }
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
                Binding {
                    group: 1,
                    binding: 0,
                    resource: crate::graph::BindingResource::Buffer {
                        buffer: output,
                        offset: 32,
                        size: 32,
                    },
                },
                Binding {
                    group: 0,
                    binding: 0,
                    resource: crate::graph::BindingResource::Buffer {
                        buffer: source,
                        offset: 16,
                        size: 48,
                    },
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
        assert_eq!(
            match operands[0] {
                BindingOperands::Buffer { resource, .. } => resource,
                _ => panic!(),
            },
            source.index
        );
        assert_eq!(
            match operands[0] {
                BindingOperands::Buffer { range, .. } => range,
                _ => panic!(),
            },
            BufferRange {
                offset: 16,
                size: 48
            }
        );
        assert_eq!(
            match operands[1] {
                BindingOperands::Buffer { resource, .. } => resource,
                _ => panic!(),
            },
            output.index
        );
        assert_eq!(
            match operands[1] {
                BindingOperands::Buffer { range, .. } => range,
                _ => panic!(),
            },
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
