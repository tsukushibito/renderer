//! Re-record changing logical work into explicitly prepared native capacity.

use std::sync::Arc;

use crate::graph::{CompiledGraph, Operation, ResourceDesc};

use super::super::{
    D3D12RuntimeError,
    bindings::*,
    frame::{D3D12FrameCapacity, FramePool, FrameWork, ReadbackMeta},
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
    if resource.import.is_some() {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph import is not implemented",
        ));
    }
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
        return Err(D3D12RuntimeError::capacity(
            "prepared DEFAULT buffer capacity exhausted",
        ));
    }
    resource_map[index] = *used;
    *used += 1;
    Ok(())
}

fn add_bytes(total: &mut u64, bytes: u64, maximum: u64) -> Result<(), D3D12RuntimeError> {
    *total = total
        .checked_add(bytes)
        .filter(|sum| *sum <= maximum)
        .ok_or_else(|| D3D12RuntimeError::capacity("prepared transfer byte capacity exhausted"))?;
    Ok(())
}

#[cfg(test)]
pub(super) fn requirements(
    graph: &CompiledGraph,
    resource_map: &mut [usize],
    capacity: D3D12FrameCapacity,
) -> Result<usize, D3D12RuntimeError> {
    requirements_inner(graph, resource_map, capacity, None, &[])
}

fn requirements_inner(
    graph: &CompiledGraph,
    resource_map: &mut [usize],
    capacity: D3D12FrameCapacity,
    mut raster: Option<&mut super::super::raster_storage::RasterStorage>,
    programs: &[super::super::raster::RasterPipeline],
) -> Result<usize, D3D12RuntimeError> {
    if !graph.exports.is_empty() {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph export is not implemented",
        ));
    }
    if graph.resources.len() > capacity.resources
        || graph.resources.len() > resource_map.len()
        || graph.order.len() > capacity.operations
    {
        return Err(D3D12RuntimeError::capacity(
            "prepared Graph metadata capacity exhausted",
        ));
    }
    // Each Graph uses only this prefix; later checkouts reinitialize their own
    // full active range, including when it grows after a smaller Graph.
    resource_map[..graph.resources.len()].fill(usize::MAX);
    let mut used = 0;
    let mut upload_bytes = 0;
    let mut readback_bytes = 0;
    let mut draws_total = 0usize;
    // Establish support and all variable capacity requirements before recording.
    // This scans changing work; it does not revalidate Graph byte-range contents.
    for &pass in &graph.order {
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
                    return Err(D3D12RuntimeError::capacity(
                        "prepared self-copy scratch capacity exhausted",
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
                add_bytes(&mut readback_bytes, *size, capacity.readback_bytes)?;
            }
            Operation::UploadImage { dst, .. } | Operation::ReadbackImage { src: dst } => {
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity("Raster image working set has not been prepared")
                })?;
                super::raster::use_image(graph, *dst, resource_map, storage)?;
                let image = &storage.images[resource_map[*dst]];
                let total = if matches!(graph.passes[pass].operation, Operation::UploadImage { .. })
                {
                    &mut upload_bytes
                } else {
                    &mut readback_bytes
                };
                let maximum =
                    if matches!(graph.passes[pass].operation, Operation::UploadImage { .. }) {
                        capacity.upload_bytes
                    } else {
                        capacity.readback_bytes
                    };
                *total = super::raster::align_transfer(*total)?;
                add_bytes(total, image.total_bytes, maximum)?;
            }
            Operation::Raster {
                program,
                draws,
                colors,
                depth,
                ..
            } => {
                if !programs
                    .iter()
                    .any(|pipeline| pipeline.matches(program, depth.is_some()))
                {
                    return Err(D3D12RuntimeError::capacity(
                        "Raster pipeline working set has not been prepared",
                    ));
                }
                draws_total = draws_total
                    .checked_add(draws.len())
                    .filter(|count| *count <= capacity.operations)
                    .ok_or_else(|| {
                        D3D12RuntimeError::capacity(
                            "prepared Raster draw descriptor capacity exhausted",
                        )
                    })?;
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity("Raster image working set has not been prepared")
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
                for draw in graph.raster_draws(draws) {
                    for binding in graph.raster_bindings(&draw.bindings) {
                        if let crate::graph::BindingOperands::Buffer { range, .. } = binding
                            && range.offset % 256 != 0
                        {
                            return Err(D3D12RuntimeError::unsupported(
                                "D3D12 Raster uniform GPU address must be 256-byte aligned",
                            ));
                        }
                    }
                }
                super::raster::validate_targets(colors)?;
            }
            Operation::AttachmentPass { colors, .. } => {
                let storage = raster.as_deref_mut().ok_or_else(|| {
                    D3D12RuntimeError::capacity("Raster image working set has not been prepared")
                })?;
                for access in graph.accesses(pass) {
                    super::raster::use_image(graph, access.resource, resource_map, storage)?;
                }
                super::raster::validate_targets(colors)?;
            }
            _ => {
                return Err(D3D12RuntimeError::unsupported(
                    "D3D12 supports live buffer transfers, image transfers and owned Raster",
                ));
            }
        }
    }
    Ok(used)
}

pub(super) fn prepare(
    pool: &Arc<FramePool>,
    graph: &CompiledGraph,
) -> Result<FrameWork, D3D12RuntimeError> {
    let needs_result = graph.order.iter().any(|&pass| {
        matches!(
            &graph.passes[pass].operation,
            Operation::ReadbackBuffer { .. } | Operation::ReadbackImage { .. }
        )
    });
    let mut work = pool.acquire(needs_result)?;
    work.set_graph(graph.id);
    let capacity = work.capacity();
    let slot = work.slot_mut();
    let used = requirements_inner(
        graph,
        &mut slot.resource_map,
        capacity,
        slot.raster.as_mut(),
        &pool.raster_programs,
    )?;
    super::raster::provenance_requirements(graph, &mut slot.provenance_needed);
    work.slot_mut().buffers_used = used;
    let owner = Arc::clone(work.owner());
    let list = work.slot().list.clone();
    let readback = work.result().map(|result| result.buffer.raw().clone());
    if let Some(result) = work.result_mut() {
        result.graph = graph.id;
        result.used = 0;
        result.readbacks.clear();
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
                                D3D12RuntimeError::capacity(
                                    "prepared index provenance capacity exhausted",
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
                            D3D12RuntimeError::capacity(
                                "prepared index provenance copy capacity exhausted",
                            )
                        })?;
                    slot.provenance[destination]
                        .replace(*dst_offset..*dst_offset + *size, &slot.provenance_scratch)
                        .map_err(|_| {
                            D3D12RuntimeError::capacity(
                                "prepared index provenance replacement capacity exhausted",
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
            Operation::UploadImage { dst, .. } => super::raster::upload_image(
                work.slot_mut(),
                &owner,
                graph,
                *dst,
                &graph.passes[pass].operation,
            )?,
            Operation::ReadbackImage { src } => {
                super::raster::readback_image(&mut work, &owner, pass, *src)?
            }
            Operation::Raster { .. } | Operation::AttachmentPass { .. } => super::raster::record(
                work.slot_mut(),
                &owner,
                graph,
                pass,
                &pool.raster_programs,
                pool.clear_programs.as_ref().unwrap(),
            )?,
            _ => unreachable!("preflight established supported immutable operations"),
        }
    }
    let slot = work.slot_mut();
    for index in 0..slot.buffers_used {
        transition(
            &list,
            slot.buffers[index].raw(),
            &mut slot.states[index],
            D3D12_RESOURCE_STATE_COMMON,
        );
    }
    transition(
        &list,
        slot.scratch.raw(),
        &mut slot.scratch_state,
        D3D12_RESOURCE_STATE_COMMON,
    );
    if let Some(storage) = &mut slot.raster {
        for image in &mut storage.images {
            if image.used {
                transition(
                    &list,
                    image.native.raw(),
                    &mut image.state,
                    D3D12_RESOURCE_STATE_COMMON,
                );
            }
        }
    }
    // Recording no longer needs CPU provenance. Native operands remain in the
    // execution lease; release Graph pins before returning the prepared work.
    for provenance in &mut slot.provenance {
        provenance.clear();
    }
    slot.provenance_scratch.clear();
    work.finish_recording()?;
    Ok(work)
}
