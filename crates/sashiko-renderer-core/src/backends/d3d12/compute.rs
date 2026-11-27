//! Owned Compute recording into explicitly prepared execution-slot storage.

use crate::ImageFormat;
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use std::{mem::ManuallyDrop, ops::Range};

use super::{
    D3D12RuntimeError, bindings::*, device::DeviceOwner, frame::ExecutionSlot, graph::transition,
    program::ComputePipeline,
};
use crate::{
    graph::{AccessPurpose, BindingOperands, CompiledGraph, ResourceDesc},
    program::{BindingType, ManagedProgram},
};

pub(super) fn require_storage_format(
    owner: &DeviceOwner,
    format: ImageFormat,
    read: bool,
) -> Result<(), D3D12RuntimeError> {
    let format = match format {
        ImageFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        ImageFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        _ => {
            return Err(D3D12RuntimeError::unsupported(
                "storage images require Rgba8Unorm or Rgba16Float",
            ));
        }
    };
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: The feature tag and writable structure match; the retained Device
    // is active and the query occurs only at the preparation/change boundary.
    unsafe {
        owner.raw.CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT,
            std::ptr::from_mut(&mut support).cast(),
            std::mem::size_of_val(&support) as u32,
        )
    }
    .ok()
    .map_err(|error| owner.native("CheckFeatureSupport(storage image)", error))?;
    let required = if read {
        D3D12_FORMAT_SUPPORT2_UAV_TYPED_LOAD
    } else {
        D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE
    };
    if support.Support1 & D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW == 0
        || support.Support2 & required == 0
    {
        return Err(D3D12RuntimeError::unsupported(
            "Device does not support the Program's typed storage image access",
        ));
    }
    Ok(())
}

pub(super) fn validate_program(
    owner: &DeviceOwner,
    program: &ManagedProgram,
) -> Result<(), D3D12RuntimeError> {
    let local = program.workgroup_size();
    if local.contains(&0)
        || local[0] > 1024
        || local[1] > 1024
        || local[2] > 64
        || u64::from(local[0]) * u64::from(local[1]) * u64::from(local[2]) > 1024
    {
        return Err(D3D12RuntimeError::unsupported(
            "Compute local size exceeds D3D12 limits",
        ));
    }
    for slot in program.bindings() {
        let (format, read) = match slot.kind {
            BindingType::StorageImageRead(format) => (format, true),
            BindingType::StorageImageWrite(format) => (format, false),
            _ => continue,
        };
        require_storage_format(owner, format, read)?;
    }
    Ok(())
}

/// Device/state limitations that cannot be established by the GPU-independent
/// Graph. Shader layout, logical ranges and initialization are already fixed.
pub(super) fn requirements(
    graph: &CompiledGraph,
    pass: usize,
    program: &ManagedProgram,
    bindings: &Range<usize>,
    workgroups: [u32; 3],
) -> Result<usize, D3D12RuntimeError> {
    let graph = graph.data();
    if workgroups.iter().any(|count| *count == 0 || *count > 65535) {
        return Err(D3D12RuntimeError::unsupported(
            "Compute dispatch exceeds D3D12 group limits",
        ));
    }
    let mut descriptors = 0;
    for (layout, operand) in program
        .bindings()
        .iter()
        .zip(graph.raster_bindings(bindings))
    {
        match operand {
            BindingOperands::Buffer { range, .. } => {
                let (alignment, maximum) = if layout.kind == BindingType::Uniform {
                    (256, 65536)
                } else {
                    (16, super::resources::MAX_BUFFER_BYTES)
                };
                if range.offset % alignment != 0 || range.size > maximum {
                    return Err(D3D12RuntimeError::unsupported(
                        "Compute buffer descriptor exceeds D3D12 alignment/range limits",
                    ));
                }
            }
            BindingOperands::ImageView(view) => {
                if layout.kind != BindingType::SampledImage
                    && (view.mip_level() != 0
                        || view.mip_level_count() != 1
                        || !matches!(graph.resources[view.image().index].desc, ResourceDesc::Image(desc) if desc.is_single_subresource()))
                {
                    return Err(D3D12RuntimeError::unsupported(
                        "D3D12 storage Views currently require mip zero",
                    ));
                }
                descriptors += 1;
            }
            BindingOperands::Sampler(_) => {}
        }
    }
    // D3D12 states cover complete buffers. A root UAV cannot coexist with a
    // CBV/SRV state for the same resource, even for disjoint logical byte ranges.
    for access in graph.accesses(pass) {
        if matches!(
            access.purpose,
            AccessPurpose::StorageWrite | AccessPurpose::StorageReadWrite
        ) && matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Buffer(_)
        ) && graph.accesses(pass).iter().any(|other| {
            other.resource == access.resource
                && !matches!(
                    other.purpose,
                    AccessPurpose::StorageWrite | AccessPurpose::StorageReadWrite
                )
        }) {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 Compute cannot bind one buffer as both UAV and CBV/SRV",
            ));
        }
    }
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Image(_)
        ) && graph.accesses(pass).iter().any(|other| {
            other.resource == access.resource
                && (other.purpose == AccessPurpose::Sampled)
                    != (access.purpose == AccessPurpose::Sampled)
        }) {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 cannot mix sampled and storage states for one image in a dispatch",
            ));
        }
    }
    Ok(descriptors)
}

fn uav_barrier(list: &ID3D12GraphicsCommandList, resource: &ID3D12Resource) {
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
            }),
        },
    };
    // SAFETY: The tagged UAV union retains the live resource. This orders the
    // preceding shader access before another access without changing its state.
    unsafe {
        list.ResourceBarrier(std::slice::from_ref(&barrier));
        ManuallyDrop::drop(&mut (*barrier.Anonymous.UAV).pResource);
    }
}

pub(super) fn record(
    slot: &mut ExecutionSlot,
    owner: &DeviceOwner,
    graph: &CompiledGraph,
    pass: usize,
    programs: &[ComputePipeline],
) -> Result<(), D3D12RuntimeError> {
    let crate::graph::Operation::Dispatch {
        program,
        bindings,
        workgroups,
    } = &graph.data().passes[pass].operation
    else {
        unreachable!()
    };
    let pipeline = programs
        .iter()
        .find(|native| native.matches(*program))
        .expect("prepared Compute pipeline");
    for access in graph.data().accesses(pass) {
        let physical = slot.resource_map[access.resource];
        match graph.data().resources[access.resource].desc {
            ResourceDesc::Buffer(_) => {
                slot.buffer_scopes[physical] |= match access.purpose {
                    AccessPurpose::Uniform => D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER,
                    AccessPurpose::StorageRead => D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    AccessPurpose::StorageWrite | AccessPurpose::StorageReadWrite => {
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS
                    }
                    _ => unreachable!(),
                };
            }
            ResourceDesc::Image(_) => {
                let image = &mut slot.raster.as_mut().unwrap().images[physical];
                let storage = matches!(
                    access.purpose,
                    AccessPurpose::StorageRead | AccessPurpose::StorageWrite
                );
                let write = access.purpose == AccessPurpose::StorageWrite;
                let after = if storage {
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS
                } else {
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                };
                if image.state == after && storage && (image.uav_write || write) {
                    uav_barrier(&slot.list, image.native.raw());
                } else {
                    transition(&slot.list, image.native.raw(), &mut image.state, after);
                }
                image.uav_write = write;
            }
        }
    }
    for access in graph.data().accesses(pass) {
        if matches!(
            graph.data().resources[access.resource].desc,
            ResourceDesc::Buffer(_)
        ) {
            let physical = slot.resource_map[access.resource];
            let after = std::mem::replace(
                &mut slot.buffer_scopes[physical],
                D3D12_RESOURCE_STATE_COMMON,
            );
            if after != D3D12_RESOURCE_STATE_COMMON {
                if after == D3D12_RESOURCE_STATE_UNORDERED_ACCESS && slot.states[physical] == after
                {
                    uav_barrier(&slot.list, slot.buffers[physical].raw());
                } else {
                    transition(
                        &slot.list,
                        slot.buffers[physical].raw(),
                        &mut slot.states[physical],
                        after,
                    );
                }
            }
        }
    }
    // Drain all scope scratch before this fallible provenance operation.
    for access in graph.data().accesses(pass) {
        if matches!(
            access.purpose,
            AccessPurpose::StorageWrite | AccessPurpose::StorageReadWrite
        ) && matches!(
            graph.data().resources[access.resource].desc,
            ResourceDesc::Buffer(_)
        ) && slot.provenance_needed[access.resource]
        {
            slot.provenance[slot.resource_map[access.resource]]
                .clear_range(access.range.clone())
                .map_err(|_| {
                    D3D12RuntimeError::capacity_with(
                        "prepared index provenance invalidation capacity exhausted",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::GraphMetadata,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    )
                })?;
        }
    }
    // SAFETY: Preparation retained this immutable PSO/signature and bounded all
    // native operands; the exclusive execution lease protects roots/descriptors.
    unsafe {
        slot.list.SetPipelineState(pipeline.pipeline());
        slot.list.SetComputeRootSignature(pipeline.signature());
    }
    let storage = slot.raster.as_ref().unwrap();
    // SAFETY: One retained shader-visible heap per supported type, with immutable
    // published sampler slots and distinct per-dispatch image descriptors.
    unsafe {
        slot.list.SetDescriptorHeaps(&[
            Some(storage.srv_heap().clone()),
            Some(storage.sampler_heap().clone()),
        ]);
    }
    for (root, (layout, operand)) in pipeline
        .managed()
        .bindings()
        .iter()
        .zip(graph.data().raster_bindings(bindings))
        .enumerate()
    {
        match operand {
            BindingOperands::Buffer { resource, range } => {
                let buffer = slot.buffers[slot.resource_map[*resource]].raw();
                // SAFETY: Root kind/register/space came from this Program's ABI.
                // Preflight established native alignment/range; trusted shader
                // footprints bound the access despite root descriptors lacking size.
                unsafe {
                    let address =
                        D3D12_GPU_VIRTUAL_ADDRESS(buffer.GetGPUVirtualAddress().0 + range.offset);
                    match layout.kind {
                        BindingType::Uniform => slot
                            .list
                            .SetComputeRootConstantBufferView(root as u32, address),
                        BindingType::StorageRead => slot
                            .list
                            .SetComputeRootShaderResourceView(root as u32, address),
                        BindingType::StorageWrite | BindingType::StorageReadWrite => slot
                            .list
                            .SetComputeRootUnorderedAccessView(root as u32, address),
                        _ => unreachable!(),
                    }
                }
            }
            BindingOperands::ImageView(view) => {
                let image = &storage.images[slot.resource_map[view.image().index]];
                let index = storage.compute_descriptor_start + slot.compute_descriptors_used;
                let cpu = storage.texture_cpu(index);
                // SAFETY: Creation checked typed format support and resource UAV
                // flags. Preflight bounded a unique descriptor slot, unmodified
                // until actual completion returns this execution lease.
                unsafe {
                    if layout.kind == BindingType::SampledImage {
                        let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
                            Format: super::resources::sampled_format(image.desc.format),
                            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING
                                as u32,
                            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                                Texture2D: D3D12_TEX2D_SRV {
                                    MostDetailedMip: view.mip_level(),
                                    MipLevels: view.mip_level_count(),
                                    PlaneSlice: 0,
                                    ResourceMinLODClamp: 0.0,
                                },
                            },
                        };
                        owner
                            .raw
                            .CreateShaderResourceView(image.native.raw(), Some(&desc), cpu);
                    } else {
                        owner.raw.CreateUnorderedAccessView(
                            image.native.raw(),
                            None::<&ID3D12Resource>,
                            None,
                            cpu,
                        );
                    }
                    slot.list
                        .SetComputeRootDescriptorTable(root as u32, storage.texture_gpu(index));
                }
                slot.compute_descriptors_used += 1;
            }
            BindingOperands::Sampler(sampler) => {
                // SAFETY: The prepared immutable sampler and table layout
                // were fixed at working-set preparation and Program creation.
                unsafe {
                    slot.list
                        .SetComputeRootDescriptorTable(root as u32, storage.sampler_gpu(*sampler)?)
                };
            }
        }
    }
    // SAFETY: The trusted Program contract and Graph fix all logical footprints;
    // native preparation checked group-count limits and every resource state.
    unsafe {
        slot.list
            .Dispatch(workgroups[0], workgroups[1], workgroups[2])
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Binding, BindingResource, BufferDesc, GraphCapacity, GraphPool, Operation};

    fn dispatch_graph(shared: bool, program: &ManagedProgram) -> CompiledGraph {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let source = graph.buffer("input", BufferDesc { size: 512 }).unwrap();
        let destination = if shared {
            source
        } else {
            graph.buffer("output", BufferDesc { size: 512 }).unwrap()
        };
        graph.upload_buffer("input", source, 0, &[0; 512]).unwrap();
        graph
            .dispatch(
                "dispatch",
                program,
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: source,
                            offset: 0,
                            size: 16,
                        },
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: destination,
                            offset: 256,
                            size: 16,
                        },
                    },
                ],
            )
            .unwrap();
        graph
            .readback_buffer("output", destination, 256, 16)
            .unwrap();
        graph.compile().unwrap()
    }

    fn dispatch(graph: &CompiledGraph) -> (usize, &Range<usize>) {
        let graph = graph.data();
        graph
            .order
            .iter()
            .find_map(|&pass| match &graph.passes[pass].operation {
                Operation::Dispatch { bindings, .. } => Some((pass, bindings)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn compute_native_limits_preserve_logical_graph_and_reject_shared_uav_read_states() {
        let uniform = ManagedProgram::uniform_readback();
        let graph = dispatch_graph(false, &uniform);
        let (pass, bindings) = dispatch(&graph);
        assert_eq!(
            requirements(&graph, pass, &uniform, bindings, [1, 1, 1]).unwrap(),
            0
        );
        assert!(requirements(&graph, pass, &uniform, bindings, [65536, 1, 1]).is_err());
        for program in [uniform, ManagedProgram::storage_readback()] {
            let graph = dispatch_graph(true, &program);
            let (pass, bindings) = dispatch(&graph);
            let error = requirements(&graph, pass, &program, bindings, [1, 1, 1]).unwrap_err();
            assert!(error.to_string().contains("both UAV and CBV/SRV"));
        }
    }
}

#[cfg(test)]
mod texture_tests {
    use super::*;
    use crate::{
        backends::d3d12::{
            D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
            D3D12ValidationMode,
        },
        graph::{Binding, BindingResource, BufferDesc, GraphCapacity, GraphPool, ImageDesc},
        program::SamplerDescriptor,
    };

    #[test]
    #[ignore = "requires real D3D12 hardware and requested native diagnostics"]
    fn d3d12_srgb_mip_views_filter_in_linear_space_and_select_sampler_policy() {
        let strict = cfg!(feature = "development-validation");
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                buffers: 1,
                buffer_bytes: 64,
                upload_bytes: 4096,
                readback_bytes: 256,
                ..Default::default()
            },
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        let desc = ImageDesc {
            width: 4,
            height: 4,
            format: ImageFormat::Rgba8Srgb,
            mip_levels: 3,
            array_layers: 1,
            sample_count: Default::default(),
        };
        let program = ManagedProgram::sampled_float_readback();
        runtime
            .prepare_graph_working_set(
                None,
                &[D3D12ImageClass { desc, count: 1 }],
                &[],
                std::slice::from_ref(&program),
            )
            .unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let image = graph.image("sRGB mip chain", desc).unwrap();
        let mut pixels = [255, 0, 0, 255].repeat(16);
        pixels.extend_from_slice(&[
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ]);
        pixels.extend_from_slice(&[0, 0, 255, 255]);
        graph
            .upload_image("all mip levels", image, &pixels)
            .unwrap();
        let output = graph
            .buffer("sampled float output", BufferDesc { size: 64 })
            .unwrap();
        let mirrored =
            SamplerDescriptor::from_gltf(Some(33648), Some(33071), Some(9728), Some(9729)).unwrap();
        runtime
            .prepare_samplers(&[
                SamplerDescriptor::LinearRepeat,
                SamplerDescriptor::NearestClamp,
            ])
            .unwrap();
        let mut readbacks = Vec::new();
        for (base, count, sampler) in [
            (1, 1, SamplerDescriptor::LinearRepeat),
            (2, 1, SamplerDescriptor::NearestClamp),
            (0, 3, SamplerDescriptor::NearestClamp),
            (1, 1, mirrored),
        ] {
            let view = graph.sampled_image_view(image, base, count).unwrap();
            graph
                .dispatch(
                    "sample selected mip",
                    &program,
                    &[
                        Binding {
                            group: 0,
                            binding: 0,
                            resource: BindingResource::ImageView(view),
                        },
                        Binding {
                            group: 0,
                            binding: 1,
                            resource: BindingResource::Sampler(sampler),
                        },
                        Binding {
                            group: 1,
                            binding: 0,
                            resource: BindingResource::Buffer {
                                buffer: output,
                                offset: 0,
                                size: 64,
                            },
                        },
                    ],
                )
                .unwrap();
            readbacks.push(
                graph
                    .readback_buffer("sample values", output, 0, 64)
                    .unwrap(),
            );
        }
        let plan = graph.compile().unwrap();
        // The failed recording must release its only slot. Preparing the
        // missing policy then retrying the same graph must succeed.
        assert!(runtime.submit(&plan).is_err());
        runtime.prepare_samplers(&[mirrored, mirrored]).unwrap();
        let mut submission = runtime.submit(&plan).unwrap();
        // Extend the Device heap without rewriting tables seen by already submitted work.
        let another = SamplerDescriptor::from_gltf(Some(33071), Some(33648), None, None).unwrap();
        runtime.prepare_samplers(&[another, mirrored]).unwrap();
        let mut result = submission
            .wait(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap();
        for (case, readback) in readbacks.into_iter().enumerate() {
            let mut actual = [0u8; 64];
            result.readback_into(readback, &mut actual).unwrap();
            for (sample, value) in actual.as_chunks::<16>().0.iter().enumerate() {
                let values: [f32; 4] = std::array::from_fn(|component| {
                    f32::from_le_bytes(value[component * 4..component * 4 + 4].try_into().unwrap())
                });
                let expected = match case {
                    0 => {
                        let gray = [1.0, 0.7, 0.1, 0.0][sample];
                        [gray, gray, gray, 1.0]
                    }
                    1 => [0.0, 0.0, 1.0, 1.0],
                    2 => [1.0, 0.0, 0.0, 1.0],
                    _ => {
                        let gray = [0.0, 1.0, 0.0, 1.0][sample];
                        [gray, gray, gray, 1.0]
                    }
                };
                for component in 0..4 {
                    assert!(
                        (values[component] - expected[component]).abs() < 0.01,
                        "case {case} sample {sample}: {values:?} expected {expected:?}"
                    );
                }
            }
        }
        drop((result, submission, plan, pool));
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:#?}");
        if strict {
            assert!(
                report.teardown_checked
                    && report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
            );
        }
    }
}
