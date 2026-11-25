//! Re-record changing logical work into explicitly prepared native capacity.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::sync::Arc;

use crate::graph::{AccessPurpose, CompiledGraph, Operation, ResourceDesc};

use super::super::{
    D3D12RuntimeError,
    bindings::*,
    frame::{D3D12FrameCapacity, FramePool, FrameWork, ReadbackMeta},
    raster::RasterPipeline,
    resources::MAX_BUFFER_BYTES,
};
use super::transition;

fn use_buffer(
    graph: &CompiledGraph,
    index: usize,
    resource_map: &mut [usize],
    used: &mut usize,
    maximum: u64,
    maximum_buffers: usize,
) -> Result<(), D3D12RuntimeError> {
    if resource_map[index] != usize::MAX {
        return Ok(());
    }
    let resource = &graph.resources[index];
    let ResourceDesc::Buffer(desc) = resource.desc else {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph image execution is not implemented",
        ));
    };
    if desc.size > MAX_BUFFER_BYTES {
        return Err(D3D12RuntimeError::unsupported(
            "buffer exceeds the supported 2 GiB limit",
        ));
    }
    if desc.size > maximum || *used == maximum_buffers {
        return Err(D3D12RuntimeError::capacity_with(
            "prepared DEFAULT buffer capacity exhausted",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::Resources,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        ));
    }
    resource_map[index] = *used;
    *used += 1;
    Ok(())
}

fn add_bytes(
    total: &mut u64,
    bytes: u64,
    maximum: u64,
    resource: RuntimeCapacityKind,
) -> Result<(), D3D12RuntimeError> {
    let required = total.checked_add(bytes);
    *total = required.filter(|sum| *sum <= maximum).ok_or_else(|| {
        D3D12RuntimeError::capacity_with(
            "prepared transfer byte capacity exhausted",
            RuntimeCapacityError::new(
                resource,
                CapacityRecovery::Reconfigure,
                Some(maximum),
                required,
            ),
        )
    })?;
    Ok(())
}

#[cfg(test)]
pub(super) fn requirements(
    graph: &CompiledGraph,
    resource_map: &mut [usize],
    capacity: D3D12FrameCapacity,
) -> Result<usize, D3D12RuntimeError> {
    let mut raster_read_promotions = vec![false; resource_map.len()];
    requirements_inner(
        graph,
        resource_map,
        &mut raster_read_promotions,
        capacity,
        None,
        &[],
        &[],
    )
}

pub(super) fn initialize_raster_read_promotions(graph: &CompiledGraph, promotions: &mut [bool]) {
    for (eligible, resource) in promotions.iter_mut().zip(&graph.resources) {
        *eligible = resource.import.is_some()
            && !matches!(resource.desc, ResourceDesc::Image(desc) if desc.format.aspect() == crate::graph::ImageAspect::Depth);
    }
}

pub(super) fn preflight_accesses(
    graph: &CompiledGraph,
    pass: usize,
    promotions: &mut [bool],
) -> Result<(), D3D12RuntimeError> {
    let raster = matches!(graph.passes[pass].operation, Operation::Raster { .. });
    for access in graph.accesses(pass) {
        if !raster
            || !matches!(
                access.purpose,
                AccessPurpose::Vertex
                    | AccessPurpose::Index
                    | AccessPurpose::Uniform
                    | AccessPurpose::StorageRead
                    | AccessPurpose::Sampled
            )
        {
            promotions[access.resource] = false;
        }
        if matches!(
            access.purpose,
            AccessPurpose::ColorAttachment | AccessPurpose::DepthAttachment
        ) && let ResourceDesc::Image(desc) = graph.resources[access.resource].desc
            && !desc.is_single_subresource()
        {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 attachments currently require a single-mip image",
            ));
        }
    }
    Ok(())
}

fn requirements_inner(
    graph: &CompiledGraph,
    resource_map: &mut [usize],
    raster_read_promotions: &mut [bool],
    capacity: D3D12FrameCapacity,
    mut raster: Option<&mut super::super::raster_storage::RasterStorage>,
    programs: &[super::super::raster::RasterPipeline],
    compute_programs: &[super::super::program::ComputePipeline],
) -> Result<usize, D3D12RuntimeError> {
    #[cfg(feature = "tracy")]
    let _span = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("renderer.d3d12.graph.preflight"),
            0,
        )
    });
    if graph.resources.len() > capacity.resources
        || graph.resources.len() > resource_map.len()
        || graph.resources.len() > raster_read_promotions.len()
        || graph.order.len() > capacity.operations
    {
        return Err(D3D12RuntimeError::capacity_with(
            "prepared Graph metadata capacity exhausted",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::GraphMetadata,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        ));
    }
    // Each Graph uses only this prefix; later checkouts reinitialize their own
    // full active range, including when it grows after a smaller Graph.
    resource_map[..graph.resources.len()].fill(usize::MAX);
    initialize_raster_read_promotions(graph, raster_read_promotions);
    let mut used = 0;
    let mut upload_bytes = 0;
    let mut readback_bytes = 0;
    let mut draws_total = 0usize;
    let mut compute_descriptors = 0usize;
    let mut raster_descriptors = 0usize;
    // Establish support and all variable capacity requirements before recording.
    // This scans changing work; it does not revalidate Graph byte-range contents.
    for &pass in &graph.order {
        preflight_accesses(graph, pass, raster_read_promotions)?;
        match &graph.passes[pass].operation {
            Operation::UploadBuffer { dst, .. } => {
                use_buffer(
                    graph,
                    *dst,
                    resource_map,
                    &mut used,
                    capacity.buffer_bytes,
                    capacity.buffers,
                )?;
                add_bytes(
                    &mut upload_bytes,
                    graph.upload_data(&graph.passes[pass].operation).len() as u64,
                    capacity.upload_bytes,
                    RuntimeCapacityKind::UploadBytes,
                )?;
            }
            Operation::CopyBuffer { src, dst, size, .. } => {
                use_buffer(
                    graph,
                    *src,
                    resource_map,
                    &mut used,
                    capacity.buffer_bytes,
                    capacity.buffers,
                )?;
                use_buffer(
                    graph,
                    *dst,
                    resource_map,
                    &mut used,
                    capacity.buffer_bytes,
                    capacity.buffers,
                )?;
                if src == dst && *size > capacity.scratch_bytes {
                    return Err(D3D12RuntimeError::capacity_with(
                        "prepared self-copy scratch capacity exhausted",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::TransferBytes,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    ));
                }
            }
            Operation::ReadbackBuffer { src, size, .. } => {
                use_buffer(
                    graph,
                    *src,
                    resource_map,
                    &mut used,
                    capacity.buffer_bytes,
                    capacity.buffers,
                )?;
                add_bytes(
                    &mut readback_bytes,
                    *size,
                    capacity.readback_bytes,
                    RuntimeCapacityKind::ReadbackBytes,
                )?;
            }
            Operation::UploadImage { dst, .. } | Operation::ReadbackImage { src: dst } => {
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Raster image working set has not been prepared",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::WorkingSet,
                            CapacityRecovery::Prepare,
                            None,
                            None,
                        ),
                    )
                })?;
                super::raster::use_image(graph, *dst, resource_map, storage)?;
                let image = &storage.images[resource_map[*dst]];
                let (total, size, limit, kind) = match graph.passes[pass].operation {
                    Operation::UploadImage { region, .. } => (
                        &mut upload_bytes,
                        super::raster::upload_bytes(image, region),
                        capacity.upload_bytes,
                        RuntimeCapacityKind::UploadBytes,
                    ),
                    _ => (
                        &mut readback_bytes,
                        image.total_bytes,
                        capacity.readback_bytes,
                        RuntimeCapacityKind::ReadbackBytes,
                    ),
                };
                *total = super::raster::align_transfer(*total)?;
                add_bytes(total, size, limit, kind)?;
            }
            Operation::CopyImage { src, dst, .. } => {
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Graph image working set has not been prepared",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::WorkingSet,
                            CapacityRecovery::Prepare,
                            None,
                            None,
                        ),
                    )
                })?;
                super::raster::use_image(graph, *src, resource_map, storage)?;
                super::raster::use_image(graph, *dst, resource_map, storage)?;
            }
            Operation::Dispatch {
                program,
                bindings,
                workgroups,
            } => {
                let pipeline = compute_programs
                    .iter()
                    .find(|native| native.matches(*program))
                    .ok_or_else(|| {
                        D3D12RuntimeError::capacity_with(
                            "Compute pipeline working set has not been prepared",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::WorkingSet,
                                CapacityRecovery::Prepare,
                                None,
                                None,
                            ),
                        )
                    })?;
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Graph working set has not been prepared",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::WorkingSet,
                            CapacityRecovery::Prepare,
                            None,
                            None,
                        ),
                    )
                })?;
                for access in graph.accesses(pass) {
                    match graph.resources[access.resource].desc {
                        ResourceDesc::Buffer(_) => use_buffer(
                            graph,
                            access.resource,
                            resource_map,
                            &mut used,
                            capacity.buffer_bytes,
                            capacity.buffers,
                        )?,
                        ResourceDesc::Image(_) => {
                            super::raster::use_image(graph, access.resource, resource_map, storage)?
                        }
                    }
                }
                compute_descriptors = compute_descriptors
                    .checked_add(super::super::compute::requirements(
                        graph,
                        pass,
                        pipeline.managed(),
                        bindings,
                        *workgroups,
                    )?)
                    .filter(|count| *count <= storage.compute_descriptor_capacity)
                    .ok_or_else(|| {
                        D3D12RuntimeError::capacity_with(
                            "prepared Compute image descriptor capacity exhausted",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::Descriptors,
                                CapacityRecovery::Reconfigure,
                                None,
                                None,
                            ),
                        )
                    })?;
            }
            Operation::Raster {
                batches,
                draws,
                colors,
                depth,
                ..
            } => {
                super::raster::validate_targets(colors)?;
                let sample_count = super::raster::target_samples(graph, colors, *depth);
                draws_total = draws_total
                    .checked_add(draws.len())
                    .filter(|count| *count <= capacity.operations)
                    .ok_or_else(|| {
                        D3D12RuntimeError::capacity_with(
                            "prepared Raster draw descriptor capacity exhausted",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::Descriptors,
                                CapacityRecovery::Reconfigure,
                                None,
                                None,
                            ),
                        )
                    })?;
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Raster image working set has not been prepared",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::WorkingSet,
                            CapacityRecovery::Prepare,
                            None,
                            None,
                        ),
                    )
                })?;
                for access in graph.accesses(pass) {
                    match graph.resources[access.resource].desc {
                        ResourceDesc::Buffer(_) => use_buffer(
                            graph,
                            access.resource,
                            resource_map,
                            &mut used,
                            capacity.buffer_bytes,
                            capacity.buffers,
                        )?,
                        ResourceDesc::Image(_) => {
                            super::raster::use_image(
                                graph,
                                access.resource,
                                resource_map,
                                storage,
                            )?;
                            if matches!(
                                access.purpose,
                                crate::graph::AccessPurpose::ColorAttachment
                                    | crate::graph::AccessPurpose::DepthAttachment
                            ) {
                                storage.require_target_view(resource_map[access.resource]);
                            }
                        }
                    }
                }
                let mut last_pipeline: Option<&RasterPipeline> = None;
                for batch in graph.raster_batches(batches) {
                    let pipeline = last_pipeline
                        .filter(|pipeline| pipeline.managed.identity() == batch.program)
                        .or_else(|| {
                            programs.iter().find(|pipeline| {
                                pipeline.matches(batch.program, depth.is_some(), sample_count)
                            })
                        })
                        .ok_or_else(|| {
                            D3D12RuntimeError::capacity_with(
                                "Raster pipeline working set has not been prepared",
                                RuntimeCapacityError::new(
                                    RuntimeCapacityKind::WorkingSet,
                                    CapacityRecovery::Prepare,
                                    None,
                                    None,
                                ),
                            )
                        })?;
                    last_pipeline = Some(pipeline);
                    for draw in graph.raster_draws(&batch.draws) {
                        raster_descriptors = raster_descriptors
                            .checked_add(super::raster::binding_requirements(
                                graph,
                                &pipeline.managed,
                                &draw.bindings,
                            )?)
                            .filter(|count| *count <= storage.raster_descriptor_capacity)
                            .ok_or_else(|| {
                                D3D12RuntimeError::capacity_with(
                                    "prepared Raster image descriptor capacity exhausted",
                                    RuntimeCapacityError::new(
                                        RuntimeCapacityKind::Descriptors,
                                        CapacityRecovery::Reconfigure,
                                        None,
                                        None,
                                    ),
                                )
                            })?;
                    }
                }
            }
            Operation::AttachmentPass { colors, .. } => {
                super::raster::validate_targets(colors)?;
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Raster image working set has not been prepared",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::WorkingSet,
                            CapacityRecovery::Prepare,
                            None,
                            None,
                        ),
                    )
                })?;
                for access in graph.accesses(pass) {
                    super::raster::use_image(graph, access.resource, resource_map, storage)?;
                    if matches!(
                        access.purpose,
                        AccessPurpose::ColorAttachment | AccessPurpose::DepthAttachment
                    ) {
                        storage.require_target_view(resource_map[access.resource]);
                    }
                }
            }
            _ => {
                return Err(D3D12RuntimeError::unsupported(
                    "D3D12 supports live transfers and owned Raster/Compute",
                ));
            }
        }
    }
    for &index in &graph.exports {
        match graph.resources[index].desc {
            ResourceDesc::Buffer(_) => use_buffer(
                graph,
                index,
                resource_map,
                &mut used,
                capacity.buffer_bytes,
                capacity.buffers,
            )?,
            ResourceDesc::Image(_) => super::raster::use_image(
                graph,
                index,
                resource_map,
                raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "image working set is not prepared",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::WorkingSet,
                            CapacityRecovery::Prepare,
                            None,
                            None,
                        ),
                    )
                })?,
            )?,
        }
    }
    Ok(used)
}

pub(super) fn prepare(
    pool: &Arc<FramePool>,
    graph: &CompiledGraph,
    jobs: Option<&std::sync::Mutex<sashiko_jobs::JobSystem>>,
) -> Result<FrameWork, D3D12RuntimeError> {
    #[cfg(feature = "tracy")]
    let _span = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("renderer.d3d12.graph.prepare_record"),
            0,
        )
    });
    let started = pool.profiling_enabled().then(std::time::Instant::now);
    let needs_result = graph.order.iter().any(|&pass| {
        matches!(
            &graph.passes[pass].operation,
            Operation::ReadbackBuffer { .. } | Operation::ReadbackImage { .. }
        )
    });
    let mut work = pool.acquire(needs_result)?;
    work.begin_profile(started);
    work.set_graph(graph.id);
    let capacity = work.capacity();
    let slot = work.slot_mut();
    let used = requirements_inner(
        graph,
        &mut slot.resource_map,
        &mut slot.raster_read_promotions,
        capacity,
        slot.raster.as_mut(),
        &pool.raster_programs,
        &pool.compute_programs,
    )?;
    super::raster::provenance_requirements(graph, &mut slot.provenance_needed);
    work.slot_mut().buffers_used = used;
    {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.d3d12.graph.registry_bind"),
                0,
            )
        });
        let mut registry = pool.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.bind(graph, work.slot_mut())?;
    }
    super::raster::retain_provenance(graph, &mut work.slot_mut().provenance_needed);
    let owner = Arc::clone(work.owner());
    if let Some(storage) = &mut work.slot_mut().raster {
        storage.bind_views(&owner);
    }
    let list = work.slot().list.clone();
    let readback = work.result().map(|result| result.buffer.raw().clone());
    if let Some(result) = work.result_mut() {
        result.graph = graph.id;
        result.used = 0;
        result.readbacks.clear();
    }
    {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.d3d12.graph.record_operations"),
                0,
            )
        });
        work.begin_recording_metrics();
        if let Some(jobs) = jobs
            && let Err(error) =
                super::bundles::record(work.slot_mut(), &owner, graph, &pool.raster_programs, jobs)
        {
            work.poison_recording();
            return Err(error);
        }
        for &pass in &graph.order {
            match &graph.passes[pass].operation {
                Operation::UploadBuffer { dst, offset, .. } => {
                    let bytes = graph.upload_data(&graph.passes[pass].operation);
                    let slot = work.slot_mut();
                    let destination = slot.resource_map[*dst];
                    let start = slot.upload_used;
                    // SAFETY: The preflight bounded this exact write within the
                    // exclusively leased UPLOAD buffer, before any GPU submission.
                    unsafe { slot.upload.write_at(&owner, start, bytes) }?;
                    transition(
                        &list,
                        slot.buffers[destination].raw(),
                        &mut slot.states[destination],
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    );
                    // SAFETY: The immutable Graph establishes the destination range;
                    // preflight bounded the source and the leased owners stay live.
                    unsafe {
                        list.CopyBufferRegion(
                            slot.buffers[destination].raw(),
                            *offset,
                            slot.upload.raw(),
                            start,
                            bytes.len() as u64,
                        )
                    };
                    slot.upload_used += bytes.len() as u64;
                    if slot.provenance_needed[*dst] {
                        let provenance = &mut slot.provenance[destination];
                        let target = *offset..*offset + bytes.len() as u64;
                        match &graph.passes[pass].operation {
                            Operation::UploadBuffer {
                                mesh: Some(crate::graph::MeshUpload::Indices { byte_len, maximum }),
                                ..
                            } => provenance.write_mesh_indices(target, *byte_len, *maximum),
                            Operation::UploadBuffer {
                                mesh: Some(crate::graph::MeshUpload::Vertices),
                                ..
                            } => provenance.clear(),
                            _ => provenance
                                .write_graph_slice(target, graph, pass, 0..bytes.len())
                                .map_err(|_| {
                                    D3D12RuntimeError::capacity_with(
                                        "prepared index provenance capacity exhausted",
                                        RuntimeCapacityError::new(
                                            RuntimeCapacityKind::GraphMetadata,
                                            CapacityRecovery::Reconfigure,
                                            None,
                                            None,
                                        ),
                                    )
                                })?,
                        }
                    }
                }
                Operation::CopyBuffer {
                    src,
                    dst,
                    src_offset,
                    dst_offset,
                    size,
                } => {
                    let slot = work.slot_mut();
                    let source = slot.resource_map[*src];
                    let destination = slot.resource_map[*dst];
                    transition(
                        &list,
                        slot.buffers[source].raw(),
                        &mut slot.states[source],
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    );
                    if src == dst {
                        transition(
                            &list,
                            slot.scratch.raw(),
                            &mut slot.scratch_state,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        // SAFETY: Separate scratch capacity and Graph source range
                        // were established before recording; both are retained.
                        unsafe {
                            list.CopyBufferRegion(
                                slot.scratch.raw(),
                                0,
                                slot.buffers[source].raw(),
                                *src_offset,
                                *size,
                            )
                        };
                        transition(
                            &list,
                            slot.scratch.raw(),
                            &mut slot.scratch_state,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                        );
                        transition(
                            &list,
                            slot.buffers[destination].raw(),
                            &mut slot.states[destination],
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        // SAFETY: The independent scratch holds the initialized copy
                        // and the destination's checked nonoverlapping Graph range.
                        unsafe {
                            list.CopyBufferRegion(
                                slot.buffers[destination].raw(),
                                *dst_offset,
                                slot.scratch.raw(),
                                0,
                                *size,
                            )
                        };
                    } else {
                        transition(
                            &list,
                            slot.buffers[destination].raw(),
                            &mut slot.states[destination],
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        // SAFETY: Distinct prepared resources have matching states
                        // and immutable Graph-guaranteed initialized copy ranges.
                        unsafe {
                            list.CopyBufferRegion(
                                slot.buffers[destination].raw(),
                                *dst_offset,
                                slot.buffers[source].raw(),
                                *src_offset,
                                *size,
                            )
                        };
                    }
                    if slot.provenance_needed[*dst] {
                        slot.provenance[source]
                            .copy_into(
                                *src_offset..*src_offset + *size,
                                *dst_offset,
                                &mut slot.provenance_scratch,
                            )
                            .map_err(|_| {
                                D3D12RuntimeError::capacity_with(
                                    "prepared index provenance copy capacity exhausted",
                                    RuntimeCapacityError::new(
                                        RuntimeCapacityKind::GraphMetadata,
                                        CapacityRecovery::Reconfigure,
                                        None,
                                        None,
                                    ),
                                )
                            })?;
                        slot.provenance[destination]
                            .replace(*dst_offset..*dst_offset + *size, &slot.provenance_scratch)
                            .map_err(|_| {
                                D3D12RuntimeError::capacity_with(
                                    "prepared index provenance replacement capacity exhausted",
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
                Operation::ReadbackBuffer { src, offset, size } => {
                    let start = work.result().unwrap().used;
                    let slot = work.slot_mut();
                    let source = slot.resource_map[*src];
                    transition(
                        &list,
                        slot.buffers[source].raw(),
                        &mut slot.states[source],
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    );
                    // SAFETY: Only initialized Graph bytes are copied; preflight
                    // bounded this exclusive result arena's destination range.
                    unsafe {
                        list.CopyBufferRegion(
                            readback.as_ref().unwrap(),
                            start,
                            slot.buffers[source].raw(),
                            *offset,
                            *size,
                        )
                    };
                    let result = work.result_mut().unwrap();
                    result.readbacks.push(ReadbackMeta {
                        pass,
                        size: *size as usize,
                        offset: start,
                        consumed: false,
                        image: None,
                    });
                    result.used += *size;
                }
                Operation::UploadImage { dst, region, .. } => super::raster::upload_image(
                    work.slot_mut(),
                    &owner,
                    graph,
                    *dst,
                    *region,
                    &graph.passes[pass].operation,
                )?,
                Operation::CopyImage {
                    src,
                    dst,
                    source,
                    destination,
                } => super::raster::copy_image(work.slot_mut(), *src, *dst, *source, *destination),
                Operation::Dispatch { .. } => super::super::compute::record(
                    work.slot_mut(),
                    &owner,
                    graph,
                    pass,
                    &pool.compute_programs,
                )?,
                Operation::ReadbackImage { src } => {
                    super::raster::readback_image(&mut work, &owner, pass, *src)?
                }
                Operation::Raster { .. } | Operation::AttachmentPass { .. } => {
                    super::raster::record(
                        work.slot_mut(),
                        &owner,
                        graph,
                        pass,
                        &pool.raster_programs,
                        pool.clear_programs.as_ref().unwrap(),
                        pool.indexed_indirect.as_ref(),
                    )?
                }
                _ => unreachable!("preflight established supported immutable operations"),
            }
        }
    }
    {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.d3d12.graph.common_barriers"),
                0,
            )
        });
        super::transition_to_common(work.slot_mut(), &list)?;
        work.slot_mut().provenance_scratch.clear();
    }
    {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.d3d12.graph.exports"),
                0,
            )
        });
        // Reserve immutable export tokens before native acceptance. Contents and
        // canonical registration are published only after successful Signal.
        work.exports = pool
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .capture_exports(graph, work.slot())?;
        work.plan = Some(graph.clone());
    }
    work.finish_recording()?;
    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_transfer_limits_report_required_bytes_without_partial_accounting() {
        let mut used = 512;
        let error = add_bytes(
            &mut used,
            19_814_400,
            16_777_216,
            RuntimeCapacityKind::ReadbackBytes,
        )
        .unwrap_err();
        assert_eq!(used, 512);
        assert_eq!(
            error.capacity(),
            Some(RuntimeCapacityError::new(
                RuntimeCapacityKind::ReadbackBytes,
                CapacityRecovery::Reconfigure,
                Some(16_777_216),
                Some(19_814_912)
            ))
        );
        assert!(error.to_string().contains("19814912"));
        add_bytes(&mut used, 16, 1024, RuntimeCapacityKind::ReadbackBytes).unwrap();
        assert_eq!(used, 528);
        let error = add_bytes(
            &mut used,
            u64::MAX,
            u64::MAX,
            RuntimeCapacityKind::UploadBytes,
        )
        .unwrap_err();
        assert_eq!(error.capacity().unwrap().required, None);
        assert_eq!(used, 528);
    }

    #[test]
    fn missing_image_preparation_is_not_poll_backpressure() {
        use crate::graph::{GraphCapacity, GraphPool, ImageDesc};
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let image = graph.image("input", ImageDesc::rgba8(1, 1)).unwrap();
        graph
            .upload_image("pixels", image, &[1, 2, 3, 255])
            .unwrap();
        graph.readback_image("result", image).unwrap();
        let graph = graph.compile().unwrap();
        let error =
            requirements(&graph, &mut [usize::MAX], D3D12FrameCapacity::default()).unwrap_err();
        assert_eq!(
            error.capacity().unwrap().recovery,
            CapacityRecovery::Prepare
        );
        assert_eq!(
            error.capacity().unwrap().resource,
            RuntimeCapacityKind::WorkingSet
        );
    }
}
