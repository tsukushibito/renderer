//! Owned graphics recording into exclusively leased, prepared native storage.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::{mem::ManuallyDrop, ops::Range};

use super::super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{ExecutionSlot, FrameWork, ReadbackImage, ReadbackMeta},
    raster::{INLINE_ROOT, RasterPipeline},
    raster_storage::{PassScopes, RasterStorage},
};
use super::transition;
use crate::{
    graph::{
        AccessPurpose, BindingOperands, CompiledGraph, LoadAction, Operation, ResourceDesc,
        SampleCount,
    },
    program::{BindingType, MAX_RASTER_COLOR_ATTACHMENTS, ManagedRasterProgram, ShaderStages},
};

fn shader_read_state(stages: ShaderStages) -> D3D12_RESOURCE_STATES {
    let mut state = D3D12_RESOURCE_STATE_COMMON;
    if stages.contains(ShaderStages::VERTEX) {
        state |= D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
    }
    if stages.contains(ShaderStages::FRAGMENT) {
        state |= D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
    }
    state
}

/// Logical binding kinds/ranges are immutable Graph guarantees. Native root
/// address alignment and limits depend on this backend and remain required.
pub(super) fn binding_requirements(
    graph: &CompiledGraph,
    program: &ManagedRasterProgram,
    bindings: &Range<usize>,
) -> Result<usize, D3D12RuntimeError> {
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
                    (16, super::super::resources::MAX_BUFFER_BYTES)
                };
                if range.offset % alignment != 0 || range.size > maximum {
                    return Err(D3D12RuntimeError::unsupported(
                        "Raster buffer descriptor exceeds D3D12 alignment/range limits",
                    ));
                }
            }
            BindingOperands::ImageView(view) => {
                let ResourceDesc::Image(desc) = graph.resources[view.image().index].desc else {
                    unreachable!()
                };
                if view.mip_level_count() != 1
                    && (view.mip_level() != 0 || view.mip_level_count() != desc.mip_levels)
                {
                    return Err(D3D12RuntimeError::unsupported(
                        "Graph sampled Views require one mip or the complete chain",
                    ));
                }
                descriptors += 1;
            }
            BindingOperands::Sampler(_) => {}
        }
    }
    Ok(descriptors)
}

pub(super) fn align_transfer(value: u64) -> Result<u64, D3D12RuntimeError> {
    value
        .checked_add(511)
        .map(|value| value & !511)
        .ok_or_else(|| {
            D3D12RuntimeError::capacity_with(
                "texture transfer offset overflow",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::TransferBytes,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })
}

pub(super) fn use_image(
    graph: &CompiledGraph,
    index: usize,
    map: &mut [usize],
    storage: &mut RasterStorage,
) -> Result<(), D3D12RuntimeError> {
    if map[index] != usize::MAX {
        return Ok(());
    }
    let ResourceDesc::Image(desc) = graph.resources[index].desc else {
        unreachable!()
    };
    if desc.array_layers != 1 {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph images currently require one layer",
        ));
    }
    let physical = storage.use_image(desc)?;
    map[index] = physical;
    Ok(())
}

pub(super) fn validate_targets(colors: &Range<usize>) -> Result<(), D3D12RuntimeError> {
    if colors.len() > MAX_RASTER_COLOR_ATTACHMENTS {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Raster color attachment count exceeds the supported maximum",
        ));
    }
    Ok(())
}

pub(super) fn target_samples(
    graph: &CompiledGraph,
    colors: &Range<usize>,
    depth: Option<usize>,
) -> SampleCount {
    let attachment = graph
        .raster_attachments(colors)
        .first()
        .or_else(|| depth.map(|index| graph.raster_attachment(index)))
        .expect("Raster has at least one target");
    let resource = &graph.resources[attachment.view.image().index];
    let ResourceDesc::Image(desc) = resource.desc else {
        unreachable!("Raster target is an image");
    };
    desc.sample_count
}

fn pass_scopes(
    graph: &CompiledGraph,
    pass: usize,
    resource_map: &[usize],
    read_promotions: &[bool],
    scopes: &mut PassScopes,
) {
    // Immutable Graph accesses establish these purposes. Aggregate every use
    // once, preserving each resource's first access order in its native class.
    for access in graph.accesses(pass) {
        // These imports start in COMMON. Preflight established that every
        // use in this list can implicitly promote to a read state. All used
        // subresources decay after this single-list ExecuteCommandLists, so
        // neither pass transitions nor the final COMMON batch are necessary.
        if read_promotions[access.resource] {
            continue;
        }
        let index = resource_map[access.resource];
        match graph.resources[access.resource].desc {
            ResourceDesc::Buffer(_) => scopes.buffer(
                index,
                match access.purpose {
                    AccessPurpose::Vertex | AccessPurpose::Uniform => {
                        D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER
                    }
                    AccessPurpose::Index => D3D12_RESOURCE_STATE_INDEX_BUFFER,
                    AccessPurpose::StorageRead => shader_read_state(access.stages),
                    _ => unreachable!("trusted Raster has read-only buffer uses"),
                },
            ),
            ResourceDesc::Image(_) => scopes.image(
                index,
                match access.purpose {
                    AccessPurpose::ColorAttachment => D3D12_RESOURCE_STATE_RENDER_TARGET,
                    AccessPurpose::ColorResolve => D3D12_RESOURCE_STATE_RESOLVE_DEST,
                    AccessPurpose::DepthAttachment => D3D12_RESOURCE_STATE_DEPTH_WRITE,
                    AccessPurpose::Sampled => shader_read_state(access.stages),
                    _ => unreachable!("trusted Raster image purposes"),
                },
            ),
        }
    }
}

pub(super) fn provenance_requirements(graph: &CompiledGraph, needed: &mut [bool]) {
    needed[..graph.resources.len()].fill(false);
    for &pass in &graph.order {
        if let Operation::Raster { draws, .. } = &graph.passes[pass].operation {
            for draw in graph.raster_draws(draws) {
                needed[draw.indices] = true;
            }
        }
    }
    for &pass in graph.order.iter().rev() {
        if let Operation::CopyBuffer { src, dst, .. } = graph.passes[pass].operation
            && needed[dst]
        {
            needed[src] = true;
        }
    }
}

pub(super) fn retain_provenance(graph: &CompiledGraph, needed: &mut [bool]) {
    for (index, resource) in graph.resources.iter().enumerate() {
        if resource.import.is_some() && matches!(resource.desc, ResourceDesc::Buffer(_)) {
            needed[index] = true;
        }
    }
    for &index in &graph.exports {
        if matches!(graph.resources[index].desc, ResourceDesc::Buffer(_)) {
            needed[index] = true;
        }
    }
    for &pass in graph.order.iter().rev() {
        if let Operation::CopyBuffer { src, dst, .. } = graph.passes[pass].operation
            && needed[dst]
        {
            needed[src] = true;
        }
    }
}

/// Both locations are stack-owned COM references; release them on unwind too.
struct TextureLocation(D3D12_TEXTURE_COPY_LOCATION);
impl TextureLocation {
    fn image(resource: &ID3D12Resource, mip: u32) -> Self {
        Self(D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: mip,
            },
        })
    }
    fn buffer(resource: &ID3D12Resource, footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT) -> Self {
        Self(D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: footprint,
            },
        })
    }
}
impl Drop for TextureLocation {
    fn drop(&mut self) {
        // SAFETY: This stack wrapper uniquely owns the generated ManuallyDrop
        // COM field and balances exactly its own clone, including on unwind.
        unsafe { ManuallyDrop::drop(&mut self.0.pResource) };
    }
}

pub(super) fn upload_image(
    slot: &mut ExecutionSlot,
    owner: &DeviceOwner,
    graph: &CompiledGraph,
    dst: usize,
    operation: &Operation,
) -> Result<(), D3D12RuntimeError> {
    let bytes = graph.upload_data(operation);
    let image = &mut slot.raster.as_mut().unwrap().images[slot.resource_map[dst]];
    let base = align_transfer(slot.upload_used)?;
    transition(
        &slot.list,
        image.native.raw(),
        &mut image.state,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    for (mip, layout) in image.mips.iter().enumerate() {
        let mut footprint = layout.footprint;
        footprint.Offset += base;
        for row in 0..layout.rows {
            let begin = layout.packed_offset + row * layout.row_bytes;
            // SAFETY: Preflight bounded all cached footprints in the retained
            // upload arena; immutable Graph guarantees packed mip0..N bytes.
            unsafe {
                slot.upload.write_at(
                    owner,
                    footprint.Offset + row as u64 * u64::from(footprint.Footprint.RowPitch),
                    &bytes[begin..begin + layout.row_bytes],
                )
            }?;
        }
        let source = TextureLocation::buffer(slot.upload.raw(), footprint);
        let destination = TextureLocation::image(image.native.raw(), mip as u32);
        // SAFETY: Each complete mip copy uses its validated native footprint,
        // bounded placement and whole-resource COPY_DEST state. Owners remain
        // pinned until actual completion; no padding bytes are read as texels.
        unsafe {
            slot.list
                .CopyTextureRegion(&destination.0, 0, 0, 0, &source.0, None)
        };
    }
    slot.upload_used = base + image.total_bytes;
    Ok(())
}

pub(super) fn copy_image(slot: &mut ExecutionSlot, src: usize, dst: usize) {
    let storage = slot.raster.as_mut().unwrap();
    let source = slot.resource_map[src];
    let destination = slot.resource_map[dst];
    let image = &mut storage.images[source];
    transition(
        &slot.list,
        image.native.raw(),
        &mut image.state,
        D3D12_RESOURCE_STATE_COPY_SOURCE,
    );
    let source = TextureLocation::image(image.native.raw(), 0);
    let image = &mut storage.images[destination];
    transition(
        &slot.list,
        image.native.raw(),
        &mut image.state,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    let destination = TextureLocation::image(image.native.raw(), 0);
    // SAFETY: Graph construction guarantees distinct, equal-format/extent color
    // images and complete initialized source content. Both single-subresource
    // native images remain exclusively leased through actual GPU completion.
    unsafe {
        slot.list
            .CopyTextureRegion(&destination.0, 0, 0, 0, &source.0, None)
    };
}

pub(super) fn readback_image(
    work: &mut FrameWork,
    _owner: &DeviceOwner,
    pass: usize,
    src: usize,
) -> Result<(), D3D12RuntimeError> {
    let base = align_transfer(work.result().unwrap().used)?;
    let buffer = work.result().unwrap().buffer.raw().clone();
    let slot = work.slot_mut();
    let image = &mut slot.raster.as_mut().unwrap().images[slot.resource_map[src]];
    let mut footprint = image.footprint;
    footprint.Offset += base;
    transition(
        &slot.list,
        image.native.raw(),
        &mut image.state,
        D3D12_RESOURCE_STATE_COPY_SOURCE,
    );
    let source = TextureLocation::image(image.native.raw(), 0);
    let destination = TextureLocation::buffer(&buffer, footprint);
    // SAFETY: Preflight bounded this cached footprint in the exclusively leased
    // result arena. The full initialized image is COPY_SOURCE; depth copies
    // use zero offsets and no source box. The result lease outlives GPU/CPU use.
    unsafe {
        slot.list
            .CopyTextureRegion(&destination.0, 0, 0, 0, &source.0, None)
    };
    let meta = ReadbackMeta {
        pass,
        size: image.row_bytes * image.rows,
        offset: footprint.Offset,
        consumed: false,
        image: Some(ReadbackImage {
            row_bytes: image.row_bytes,
            row_pitch: footprint.Footprint.RowPitch as usize,
            rows: image.rows,
        }),
    };
    let end = base + image.total_bytes;
    let result = work.result_mut().unwrap();
    result.readbacks.push(meta);
    result.used = end;
    Ok(())
}

pub(super) fn record(
    slot: &mut ExecutionSlot,
    owner: &DeviceOwner,
    graph: &CompiledGraph,
    pass: usize,
    programs: &[RasterPipeline],
    clear_programs: &super::super::raster_clear::ClearPrograms,
) -> Result<(), D3D12RuntimeError> {
    #[cfg(feature = "tracy")]
    let _span = tracy_client::Client::running().map(|client| {
        client.span(
            tracy_client::span_location!("renderer.d3d12.raster.record"),
            0,
        )
    });
    let (batches, colors, depth) = match &graph.passes[pass].operation {
        Operation::Raster {
            batches,
            colors,
            depth,
            ..
        } => (graph.raster_batches(batches), colors, depth),
        Operation::AttachmentPass { colors, depth, .. } => (&[][..], colors, depth),
        _ => unreachable!(),
    };
    let sample_count = target_samples(graph, colors, *depth);
    let storage = slot.raster.as_mut().unwrap();
    pass_scopes(
        graph,
        pass,
        &slot.resource_map,
        &slot.raster_read_promotions,
        &mut storage.scopes,
    );
    // Preserve native order: all buffer transitions precede all image
    // transitions, each in first-use order. Both scratch lists are drained
    // before any later fallible validation, descriptor or draw preparation.
    storage.scopes.drain_buffers(|index, scope| {
        transition(
            &slot.list,
            slot.buffers[index].raw(),
            &mut slot.states[index],
            scope,
        );
    });
    storage.scopes.drain_images(|index, scope| {
        let image = &mut storage.images[index];
        transition(&slot.list, image.native.raw(), &mut image.state, scope);
        image.uav_write = false;
    });
    let depth = depth.map(|index| {
        let attachment = graph.raster_attachment(index);
        (
            attachment,
            &storage.images[slot.resource_map[attachment.view.image().index]],
        )
    });
    let mut color_handles = [D3D12_CPU_DESCRIPTOR_HANDLE::default(); MAX_RASTER_COLOR_ATTACHMENTS];
    let mut depth_handle = None;
    let mut extent = [0, 0];
    for (handle, attachment) in color_handles
        .iter_mut()
        .zip(graph.raster_attachments(colors))
    {
        let image = &storage.images[slot.resource_map[attachment.view.image().index]];
        *handle = image.rtv.unwrap();
        extent = [image.desc.width, image.desc.height];
        if attachment.ops.load == LoadAction::Clear
            && attachment.clear.map(f32::to_bits) == [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits)
        {
            // SAFETY: Prepared RTV matches this retained image in RENDER_TARGET.
            // Graph guarantees finite clear color and complete attachment extent.
            unsafe {
                slot.list
                    .ClearRenderTargetView(*handle, &attachment.clear, &[])
            };
        } else if attachment.ops.load == LoadAction::Clear {
            clear_programs.record(&slot.list, image, attachment.clear, None);
        }
    }
    if let Some((attachment, image)) = depth {
        let handle = image.dsv.unwrap();
        depth_handle = Some(handle);
        extent = [image.desc.width, image.desc.height];
        if attachment.ops.load == LoadAction::Clear
            && attachment.clear[0].to_bits() == 1.0f32.to_bits()
        {
            // SAFETY: Prepared full D32 DSV is DEPTH_WRITE; Graph construction
            // established normalized clear depth, and this lease retains it.
            unsafe {
                slot.list.ClearDepthStencilView(
                    handle,
                    D3D12_CLEAR_FLAG_DEPTH,
                    attachment.clear[0],
                    0,
                    &[],
                )
            };
        } else if attachment.ops.load == LoadAction::Clear {
            clear_programs.record(&slot.list, image, [1.0; 4], Some(attachment.clear[0]));
        }
    }
    if batches.is_empty() {
        record_resolves(graph, colors, &slot.resource_map, &slot.list, storage);
        return Ok(());
    }
    let viewport = D3D12_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: extent[0] as f32,
        Height: extent[1] as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    };
    let rect = D3D12_RECT {
        left: 0,
        top: 0,
        right: extent[0] as i32,
        bottom: extent[1] as i32,
    };
    // SAFETY: Pass preparation bounded the native target count. This stack
    // array contains actual CPU handles in fragment-output slot order. The
    // descriptors are not required to be contiguous in their native heap.
    // The live images remain leased and the extent fits native limits.
    unsafe {
        slot.list.OMSetRenderTargets(
            colors.len() as u32,
            (!colors.is_empty()).then_some(color_handles.as_ptr()),
            false,
            depth_handle.as_ref().map(std::ptr::from_ref),
        );
        slot.list.RSSetViewports(&[viewport]);
        slot.list.RSSetScissorRects(&[rect]);
        slot.list.IASetPrimitiveTopology(D3D12_PRIMITIVE_TOPOLOGY(
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        ));
    }
    let mut bound_program: Option<&ManagedRasterProgram> = None;
    let mut bound_heaps = false;
    for batch in batches {
        let program = &batch.program;
        if bound_program.is_none_or(|previous| !program.same(previous)) {
            let pipeline = programs
                .iter()
                .find(|pipeline| pipeline.matches(program, depth_handle.is_some(), sample_count))
                .expect("prepared Raster pipeline");
            // SAFETY: Every prepared PSO matches these shared attachments and
            // its own root signature. Changing the signature invalidates prior
            // root arguments. record_draws sets all the current Program's roots.
            unsafe {
                slot.list.SetPipelineState(&*pipeline.pipeline);
                slot.list.SetGraphicsRootSignature(&*pipeline.signature);
            }
            bound_program = Some(program);
        }
        if !bound_heaps && !program.bindings().is_empty() {
            let storage = slot.raster.as_ref().unwrap();
            // SAFETY: The execution owns both heaps and keeps them leased
            // until every recorded use retires. Later batches reuse them.
            unsafe {
                slot.list.SetDescriptorHeaps(&[
                    Some(storage.srv_heap().clone()),
                    Some(storage.sampler_heap().clone()),
                ]);
            }
            bound_heaps = true;
        }
        record_draws(slot, owner, graph, program, &batch.draws)?;
    }
    record_resolves(
        graph,
        colors,
        &slot.resource_map,
        &slot.list,
        slot.raster.as_mut().unwrap(),
    );
    Ok(())
}

fn record_draws(
    slot: &mut ExecutionSlot,
    owner: &DeviceOwner,
    graph: &CompiledGraph,
    program: &ManagedRasterProgram,
    draws: &Range<usize>,
) -> Result<(), D3D12RuntimeError> {
    let storage = slot.raster.as_mut().unwrap();
    for draw in graph.raster_draws(draws) {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.d3d12.raster.draw"),
                0,
            )
        });
        {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!(
                        "renderer.d3d12.raster.vertex_index_and_constants"
                    ),
                    0,
                )
            });
            let vertex = slot.resource_map[draw.vertices];
            let index = slot.resource_map[draw.indices];
            let vertex_bytes = graph.resources[draw.vertices].desc.units();
            slot.provenance[index]
                .validate_indices(
                    draw.index_count,
                    vertex_bytes / u64::from(program.vertex_stride()),
                    u32::MAX,
                )
                .map_err(|error| {
                    use crate::graph::provenance::IndexError;
                    match error {
                        IndexError::Capacity(error) => D3D12RuntimeError::capacity_with(
                            "generic index prefix summary capacity exhausted",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::GraphMetadata,
                                CapacityRecovery::Reconfigure,
                                Some(error.capacity as u64),
                                Some(error.required as u64),
                            ),
                        ),
                        IndexError::UnknownContents => D3D12RuntimeError::unsupported(
                            "Raster indices require known initialized CPU contents",
                        ),
                        IndexError::VertexBounds { .. } | IndexError::DeviceLimit { .. } => {
                            D3D12RuntimeError::invalid(
                                "Raster index exceeds the vertex buffer range",
                            )
                        }
                    }
                })?;
            // SAFETY: The retained buffer capacities cover immutable Graph ranges;
            // validated index contents fit the declared vertex stream. Inline words
            // match this immutable Program's byte count and stage/register ABI.
            unsafe {
                let vertices = D3D12_VERTEX_BUFFER_VIEW {
                    BufferLocation: slot.buffers[vertex].raw().GetGPUVirtualAddress(),
                    SizeInBytes: vertex_bytes as u32,
                    StrideInBytes: program.vertex_stride(),
                };
                let indices = D3D12_INDEX_BUFFER_VIEW {
                    BufferLocation: slot.buffers[index].raw().GetGPUVirtualAddress(),
                    SizeInBytes: graph.resources[draw.indices].desc.units() as u32,
                    Format: DXGI_FORMAT_R32_UINT,
                };
                slot.list.IASetVertexBuffers(0, Some(&[vertices]));
                slot.list.IASetIndexBuffer(Some(&indices));
                if !draw.parameters.words().is_empty() {
                    slot.list.SetGraphicsRoot32BitConstants(
                        INLINE_ROOT,
                        draw.parameters.words().len() as u32,
                        draw.parameters.words().as_ptr().cast(),
                        0,
                    );
                }
            }
        }
        {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!(
                        "renderer.d3d12.raster.materialize_descriptors_and_bind"
                    ),
                    0,
                )
            });
            let first_root = u32::from(program.parameter_size() != 0);
            for (index, (layout, operand)) in program
                .bindings()
                .iter()
                .zip(graph.raster_bindings(&draw.bindings))
                .enumerate()
            {
                let root = first_root + index as u32;
                match operand {
                    BindingOperands::Buffer { resource, range } => {
                        // SAFETY: Preparation established this root's ABI and the
                        // preflight bounded address alignment and range. All native
                        // resources remain pinned by the exclusive execution lease.
                        unsafe {
                            let address = D3D12_GPU_VIRTUAL_ADDRESS(
                                slot.buffers[slot.resource_map[*resource]]
                                    .raw()
                                    .GetGPUVirtualAddress()
                                    .0
                                    + range.offset,
                            );
                            if layout.kind == BindingType::Uniform {
                                slot.list.SetGraphicsRootConstantBufferView(root, address);
                            } else {
                                slot.list.SetGraphicsRootShaderResourceView(root, address);
                            }
                        }
                    }
                    BindingOperands::ImageView(view) => {
                        let descriptor = storage.raster_srv(
                            owner,
                            slot.resource_map[view.image().index],
                            view.mip_level(),
                            view.mip_level_count(),
                        )?;
                        // SAFETY: The cached immutable SRV refers to the retained
                        // image/View, and this execution owns its descriptor heap
                        // through actual GPU completion or unsubmitted cancel.
                        unsafe {
                            slot.list.SetGraphicsRootDescriptorTable(
                                root,
                                storage.texture_gpu(descriptor),
                            );
                        }
                    }
                    BindingOperands::Sampler(sampler) => {
                        // SAFETY: The supported immutable nearest/clamp sampler and
                        // its descriptor table match the trusted Program's layout.
                        unsafe {
                            slot.list.SetGraphicsRootDescriptorTable(
                                root,
                                storage.sampler_gpu(*sampler),
                            );
                        }
                    }
                }
            }
        }
        {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.d3d12.raster.draw_indexed"),
                    0,
                )
            });
            // SAFETY: Every vertex/index/binding operand is valid and retained, all
            // required state is set, and native attachment order preserves draw order.
            unsafe { slot.list.DrawIndexedInstanced(draw.index_count, 1, 0, 0, 0) };
        }
    }
    Ok(())
}

fn record_resolves(
    graph: &CompiledGraph,
    colors: &Range<usize>,
    resource_map: &[usize],
    list: &ID3D12GraphicsCommandList,
    storage: &mut RasterStorage,
) {
    for attachment in graph.raster_attachments(colors) {
        let Some(resolve) = attachment.resolve else {
            continue;
        };
        let source_index = resource_map[attachment.view.image().index];
        let target_index = resource_map[resolve.image().index];
        let source = &mut storage.images[source_index];
        transition(
            list,
            source.native.raw(),
            &mut source.state,
            D3D12_RESOURCE_STATE_RESOLVE_SOURCE,
        );
        let source = &storage.images[source_index];
        let target = &storage.images[target_index];
        // SAFETY: Graph construction establishes equal-format, equal-extent
        // distinct single-subresource images, with a multisampled source and
        // single-sample destination. Preparation validates format capabilities.
        // Both resources remain leased, and their tracked states are RESOLVE_SOURCE
        // after rendering and RESOLVE_DEST from the start of this pass.
        unsafe {
            list.ResolveSubresource(
                target.native.raw(),
                0,
                source.native.raw(),
                0,
                super::super::resources::image_format(source.desc.format),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backends::d3d12::{
            D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
            D3D12ValidationMode,
        },
        graph::{
            Attachment, Binding, BindingResource, BufferDesc, BufferHandle, GraphCapacity,
            GraphPool, ImageDesc, ImageHandle, ImportBuffer, ImportImage, ImportRanges, LoadOp,
            PassHandle, RasterAttachments, RasterDraw, RasterParameters, ResourceKey, StoreOp,
        },
        program::{ManagedRasterProgram, SamplerDescriptor},
    };
    use std::time::Duration;

    struct ScopeFixture {
        _pool: GraphPool,
        plan: CompiledGraph,
        buffers: [BufferHandle; 2],
        texture: ImageHandle,
        target: ImageHandle,
        passes: [PassHandle; 3],
        readbacks: [PassHandle; 3],
    }

    fn scope_fixture(import_inputs: bool) -> ScopeFixture {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let key = |index| ResourceKey {
            owner: 1,
            index,
            generation: 1,
        };
        let desc = BufferDesc { size: 84 };
        let mut buffer = |name, index| {
            if import_inputs {
                let snapshot = ImportBuffer::new(
                    key(index),
                    1,
                    desc,
                    ImportRanges::test_capture(std::slice::from_ref(&(0..desc.size))),
                )
                .unwrap();
                graph.import_buffer(name, &snapshot).unwrap()
            } else {
                graph.buffer(name, desc).unwrap()
            }
        };
        let shared = buffer("shared geometry", 1);
        let other = buffer("other geometry", 2);
        for buffer in [shared, other] {
            if import_inputs && buffer == shared {
                continue;
            }
            // Zero indices and vertices form a valid degenerate triangle; the
            // first 16 bytes also provide the textured shader's zero offset.
            graph
                .upload_buffer("zero geometry", buffer, 0, &[0; 84])
                .unwrap();
        }
        let texture_desc = ImageDesc::rgba8(1, 1);
        let texture = if import_inputs {
            let snapshot = ImportImage::test_initialized(key(3), 1, texture_desc);
            graph.import_image("texture", &snapshot).unwrap()
        } else {
            let texture = graph.image("texture", texture_desc).unwrap();
            graph.upload_image("white", texture, &[255; 4]).unwrap();
            texture
        };
        let target = graph.image("target", ImageDesc::rgba8(1, 1)).unwrap();
        let mut passes = Vec::new();
        let mut readbacks = Vec::new();
        for (vertices, uniform, clear) in [
            (shared, shared, [1.0, 0.0, 0.0, 1.0]),
            (other, other, [0.0, 1.0, 0.0, 1.0]),
        ] {
            let bindings = [
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: uniform,
                        offset: 0,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(texture, 0, 0).unwrap()),
                },
                Binding {
                    group: 1,
                    binding: 1,
                    resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                },
            ];
            let bindings = graph
                .raster_bindings(&ManagedRasterProgram::textured_mesh(), &bindings)
                .unwrap();
            passes.push(
                graph
                    .raster(
                        "shared buffer scopes",
                        &ManagedRasterProgram::textured_mesh(),
                        [RasterDraw {
                            bindings: Some(bindings),
                            vertices,
                            indices: shared,
                            index_count: 3,
                            parameters: RasterParameters::default().into(),
                        }],
                        RasterAttachments {
                            color_resolves: &[],
                            colors: &[Attachment {
                                view: graph.image_view(target, 0, 0).unwrap(),
                                load: LoadOp::Clear(clear),
                                store: StoreOp::Store,
                            }],
                            depth: None,
                        },
                    )
                    .unwrap(),
            );
            readbacks.push(graph.readback_image("raster result", target).unwrap());
        }
        passes.push(
            graph
                .attachment_pass(
                    "clear only",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[Attachment {
                            view: graph.image_view(target, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                )
                .unwrap(),
        );
        readbacks.push(graph.readback_image("clear result", target).unwrap());
        ScopeFixture {
            _pool: pool,
            plan: graph.compile().unwrap(),
            buffers: [shared, other],
            texture,
            target,
            passes: passes.try_into().unwrap(),
            readbacks: readbacks.try_into().unwrap(),
        }
    }

    #[test]
    fn pass_read_scopes_combine_once_in_native_first_use_order() {
        let fixture = scope_fixture(false);
        let mut map = vec![usize::MAX; fixture.plan.resources.len()];
        for (index, buffer) in fixture.buffers.iter().enumerate() {
            map[buffer.index] = index;
        }
        let mut image = 0;
        for (index, resource) in fixture.plan.resources.iter().enumerate() {
            if matches!(resource.desc, ResourceDesc::Image(_)) {
                map[index] = image;
                image += 1;
            }
        }
        let combined =
            D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER | D3D12_RESOURCE_STATE_INDEX_BUFFER;
        let expected_buffers = [
            vec![(0, combined)],
            vec![
                (1, D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER),
                (0, D3D12_RESOURCE_STATE_INDEX_BUFFER),
            ],
            vec![],
        ];
        let expected_images = [
            vec![
                (0, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE),
                (1, D3D12_RESOURCE_STATE_RENDER_TARGET),
            ],
            vec![
                (0, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE),
                (1, D3D12_RESOURCE_STATE_RENDER_TARGET),
            ],
            vec![(1, D3D12_RESOURCE_STATE_RENDER_TARGET)],
        ];
        let mut scopes = PassScopes::new(2, 2).unwrap();
        let promotions = vec![false; map.len()];
        for ((pass, buffers), images) in fixture
            .passes
            .iter()
            .zip(expected_buffers)
            .zip(expected_images)
        {
            pass_scopes(&fixture.plan, pass.index, &map, &promotions, &mut scopes);
            let mut applied = Vec::new();
            scopes.drain_buffers(|index, scope| applied.push((index, scope)));
            assert_eq!(applied, buffers);
            applied.clear();
            scopes.drain_images(|index, scope| applied.push((index, scope)));
            assert_eq!(applied, images);
        }
    }

    #[test]
    fn raster_read_promotion_excludes_uploaded_imports_and_attachments() {
        use super::super::bounded::{initialize_raster_read_promotions, preflight_accesses};

        let fixture = scope_fixture(true);
        let plan = &fixture.plan;
        let [shared, updated] = fixture.buffers;
        let mut promotions = vec![true; plan.resources.len()];
        initialize_raster_read_promotions(plan, &mut promotions);
        for &pass in &plan.order {
            preflight_accesses(plan, pass, &mut promotions).unwrap();
        }
        let promoted: Vec<_> = promotions
            .iter()
            .enumerate()
            .filter_map(|(index, &eligible)| eligible.then_some(index))
            .collect();
        assert_eq!(promoted, [shared.index, fixture.texture.index]);
        let mut map = vec![usize::MAX; plan.resources.len()];
        map[shared.index] = 0;
        map[updated.index] = 1;
        map[fixture.texture.index] = 0;
        map[fixture.target.index] = 1;
        let mut scopes = PassScopes::new(2, 2).unwrap();
        pass_scopes(
            plan,
            fixture.passes[1].index,
            &map,
            &promotions,
            &mut scopes,
        );
        let mut buffers = Vec::new();
        let mut images = Vec::new();
        scopes.drain_buffers(|index, _| buffers.push(index));
        scopes.drain_images(|index, _| images.push(index));
        assert_eq!(buffers, [1]);
        assert_eq!(images, [1]);
    }

    fn sampled_views_fixture(
        invalid_index: bool,
        texture: &ImportImage,
    ) -> (GraphPool, CompiledGraph, Vec<(PassHandle, [u8; 4])>) {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        let uniform = graph.buffer("offset", BufferDesc { size: 16 }).unwrap();
        let vertex_data: [[f32; 7]; 3] = [
            [-1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            [3.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            [-1.0, 3.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        ];
        let vertex_data = vertex_data
            .into_iter()
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        let index_data = [0_u32, 1, 2]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        graph
            .upload_buffer("vertices", vertices, 0, &vertex_data)
            .unwrap();
        graph
            .upload_buffer("indices", indices, 0, &index_data)
            .unwrap();
        graph.upload_buffer("offset", uniform, 0, &[0; 16]).unwrap();
        let texture = graph.import_image("mipped texture", texture).unwrap();
        let other = graph
            .image("other texture", ImageDesc::rgba8(1, 1))
            .unwrap();
        let target = graph.image("target", ImageDesc::rgba8(1, 1)).unwrap();
        graph
            .upload_image("blue", other, &[0, 0, 255, 255])
            .unwrap();
        let program = ManagedRasterProgram::textured_mesh();
        let mut readbacks = Vec::new();
        for (case, (image, mip, expected)) in [
            (texture, 0, [255, 0, 0, 255]),
            (texture, 1, [0, 255, 0, 255]),
            (other, 0, [0, 0, 255, 255]),
            (texture, 0, [255, 0, 0, 255]),
        ]
        .into_iter()
        .enumerate()
        {
            if invalid_index && case == 1 {
                // Reject a later pass after the first pass has already written
                // a cached SRV. Unsubmitted failure must not publish that cache
                // into the next execution lease.
                let invalid = [0_u32, 1, 99]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>();
                graph
                    .upload_buffer("invalid later indices", indices, 0, &invalid)
                    .unwrap();
            }
            let bindings = [
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: uniform,
                        offset: 0,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(image, mip, 0).unwrap()),
                },
                Binding {
                    group: 1,
                    binding: 1,
                    resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                },
            ];
            let bindings = graph.raster_bindings(&program, &bindings).unwrap();
            graph
                .raster(
                    "sample view",
                    &program,
                    [RasterDraw {
                        bindings: Some(bindings),
                        vertices,
                        indices,
                        index_count: 3,
                        parameters: RasterParameters::default().into(),
                    }; 2],
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[Attachment {
                            view: graph.image_view(target, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                )
                .unwrap();
            readbacks.push((
                graph.readback_image("sampled result", target).unwrap(),
                expected,
            ));
        }
        (pool, graph.compile().unwrap(), readbacks)
    }

    fn mip_layers() -> [u8; 20] {
        [
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255,
        ]
    }

    fn srv_creations(prepared: &super::super::D3D12PreparedGraph) -> usize {
        let super::super::PreparedWork::Pooled(work) = &prepared.kind else {
            panic!("pooled Graph expected")
        };
        work.slot().raster.as_ref().unwrap().srv_creations
    }

    fn target_view_creations(prepared: &super::super::D3D12PreparedGraph) -> [usize; 2] {
        let super::super::PreparedWork::Pooled(work) = &prepared.kind else {
            panic!("pooled Graph expected")
        };
        work.slot().raster.as_ref().unwrap().target_view_creations
    }

    #[test]
    #[ignore = "requires real D3D12 hardware and requested native diagnostics"]
    fn d3d12_raster_srv_reuse_preserves_views_cancel_and_pending_ownership() {
        let strict = cfg!(feature = "development-validation");
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                buffers: 3,
                buffer_bytes: 84,
                upload_bytes: 4096,
                readback_bytes: 4096,
                ..Default::default()
            },
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        let mut mipped = ImageDesc::rgba8(2, 2);
        mipped.mip_levels = 2;
        runtime
            .prepare_graph_working_set(
                None,
                &[
                    D3D12ImageClass {
                        desc: mipped,
                        count: 1,
                    },
                    D3D12ImageClass {
                        desc: ImageDesc::rgba8(1, 1),
                        count: 2,
                    },
                ],
                &[ManagedRasterProgram::textured_mesh().into()],
                &[],
            )
            .unwrap();
        let mut upload_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut upload = upload_pool.begin().unwrap();
        let texture = upload.image("mipped texture", mipped).unwrap();
        upload.upload_image("mips", texture, &mip_layers()).unwrap();
        upload.export_image(texture).unwrap();
        let mut upload_submission = runtime.submit(&upload.compile().unwrap()).unwrap();
        let snapshot = upload_submission.export_image(texture).unwrap();
        assert!(
            upload_submission
                .wait(Duration::from_secs(30))
                .unwrap()
                .is_some()
        );
        drop((upload_submission, upload_pool));
        let (bad_pool, bad_graph, _) = sampled_views_fixture(true, &snapshot);
        assert!(
            runtime.prepare(&bad_graph).is_err(),
            "late index rejection must return the slot"
        );
        let (pool, graph, readbacks) = sampled_views_fixture(false, &snapshot);
        let cancelled = runtime.prepare(&graph).unwrap();
        // Eight binding occurrences across four passes create only three views.
        assert_eq!(srv_creations(&cancelled), 3);
        // All four passes share one target. The two sampled-only color images
        // must not receive unused RTVs (including the multi-mip texture).
        assert_eq!(target_view_creations(&cancelled), [1, 0]);
        let super::super::PreparedWork::Pooled(work) = &cancelled.kind else {
            panic!("pooled Graph expected")
        };
        // The final batch contains three uploaded buffers, the local image
        // and the readback target. The imported mip chain decays implicitly.
        assert_eq!(work.slot().common_barriers.recorded_transitions, 5);
        assert!(
            runtime.prepare(&graph).is_err(),
            "recorded descriptors remain exclusively leased"
        );
        drop(cancelled);
        for _ in 0..2 {
            let prepared = runtime.prepare(&graph).unwrap();
            // Reset invalidates every prior key, even if the native resources
            // happen to keep the same address after cancel/retirement.
            assert_eq!(srv_creations(&prepared), 3);
            assert_eq!(target_view_creations(&prepared), [1, 0]);
            let mut submission = runtime.submit_prepared(prepared).unwrap();
            assert!(
                runtime.prepare(&graph).is_err(),
                "pending descriptors cannot be overwritten"
            );
            let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
            for &(pass, expected) in &readbacks {
                let mut actual = [0; 4];
                result.readback_into(pass, &mut actual).unwrap();
                assert_eq!(actual, expected);
            }
            drop((result, submission));
        }
        drop((bad_graph, bad_pool, graph, pool));
        runtime.delete(snapshot.key()).unwrap();
        // Exercise the attachment-only path after a working-set change. A live
        // transfer-only color image still requires no target view, while the
        // depth attachment requires one DSV even though there are no draws.
        runtime
            .prepare_graph_working_set(
                None,
                &[
                    D3D12ImageClass {
                        desc: ImageDesc::depth32(1, 1),
                        count: 1,
                    },
                    D3D12ImageClass {
                        desc: ImageDesc::rgba8(1, 1),
                        count: 1,
                    },
                ],
                &[],
                &[],
            )
            .unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(1, 1)).unwrap();
        let color = graph
            .image("transfer only", ImageDesc::rgba8(1, 1))
            .unwrap();
        graph
            .upload_image("transfer", color, &[1, 2, 3, 4])
            .unwrap();
        graph
            .attachment_pass(
                "depth only",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[],
                    depth: Some(Attachment {
                        view: graph.image_view(depth, 0, 0).unwrap(),
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    }),
                },
            )
            .unwrap();
        let depth_result = graph.readback_image("depth result", depth).unwrap();
        let color_result = graph.readback_image("transfer result", color).unwrap();
        let graph = graph.compile().unwrap();
        let cancelled = runtime.prepare(&graph).unwrap();
        assert_eq!(target_view_creations(&cancelled), [0, 1]);
        drop(cancelled);
        let prepared = runtime.prepare(&graph).unwrap();
        assert_eq!(target_view_creations(&prepared), [0, 1]);
        let mut submission = runtime.submit_prepared(prepared).unwrap();
        let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
        for (pass, expected) in [
            (depth_result, 1.0_f32.to_le_bytes()),
            (color_result, [1, 2, 3, 4]),
        ] {
            let mut actual = [0; 4];
            result.readback_into(pass, &mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        drop((result, submission, graph, pool));
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

    #[test]
    #[ignore = "requires real D3D12 hardware and requested native diagnostics"]
    fn d3d12_shared_buffer_scopes_preserve_clear_results() {
        let strict = cfg!(feature = "development-validation");
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity: D3D12FrameCapacity {
                executions: 1,
                buffers: 2,
                buffer_bytes: 84,
                upload_bytes: 1024,
                readback_bytes: 2048,
                ..Default::default()
            },
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        runtime
            .prepare_graph_working_set(
                None,
                &[D3D12ImageClass {
                    desc: ImageDesc::rgba8(1, 1),
                    count: 2,
                }],
                &[ManagedRasterProgram::textured_mesh().into()],
                &[],
            )
            .unwrap();
        let fixture = scope_fixture(false);
        let mut submission = runtime.submit(&fixture.plan).unwrap();
        let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
        for (pass, expected) in
            fixture
                .readbacks
                .iter()
                .zip([[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]])
        {
            let mut actual = [0; 4];
            result.readback_into(*pass, &mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        drop((result, submission, fixture));
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
