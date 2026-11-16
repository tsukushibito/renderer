//! Owned graphics recording into exclusively leased, prepared native storage.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::{mem::ManuallyDrop, ops::Range};

use super::super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{ExecutionSlot, FrameWork, ReadbackImage, ReadbackMeta},
    raster::{INLINE_ROOT, RasterPipeline},
    raster_storage::RasterStorage,
};
use super::transition;
use crate::{
    graph::{AccessPurpose, BindingOperands, CompiledGraph, LoadAction, Operation, ResourceDesc},
    program::{BindingType, ManagedRasterProgram, ShaderStages},
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
    let (physical, image) = storage
        .images
        .iter_mut()
        .enumerate()
        .find(|(_, image)| !image.used && image.desc == desc)
        .ok_or_else(|| {
            D3D12RuntimeError::capacity_with(
                "prepared Raster image class exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Resources,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })?;
    image.used = true;
    map[index] = physical;
    Ok(())
}

pub(super) fn validate_targets(colors: &Range<usize>) -> Result<(), D3D12RuntimeError> {
    if colors.len() > 1 {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Raster supports one color attachment",
        ));
    }
    Ok(())
}

fn buffer_scopes(
    graph: &CompiledGraph,
    pass: usize,
    resource_map: &[usize],
    scopes: &mut [D3D12_RESOURCE_STATES],
    mut apply: impl FnMut(usize, D3D12_RESOURCE_STATES),
) {
    // Graph derives these uses from immutable commands. Combine native read
    // states for shared Vertex/Index/Uniform buffers using only this pass's
    // accesses, without clearing or scanning unrelated Graph buffers.
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Buffer(_)
        ) {
            scopes[resource_map[access.resource]] |= match access.purpose {
                AccessPurpose::Vertex | AccessPurpose::Uniform => {
                    D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER
                }
                AccessPurpose::Index => D3D12_RESOURCE_STATE_INDEX_BUFFER,
                AccessPurpose::StorageRead => shader_read_state(access.stages),
                _ => unreachable!("trusted Raster has read-only buffer uses"),
            };
        }
    }
    // Entries start at COMMON and are drained before any later fallible work.
    // Repeated accesses apply the combined scope once; every pass, including a
    // clear-only pass, leaves the scratch array ready for its next use.
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Buffer(_)
        ) {
            let index = resource_map[access.resource];
            let scope = std::mem::replace(&mut scopes[index], D3D12_RESOURCE_STATE_COMMON);
            if scope != D3D12_RESOURCE_STATE_COMMON {
                apply(index, scope);
            }
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
    let (program, draws, colors, depth) = match &graph.passes[pass].operation {
        Operation::Raster {
            program,
            draws,
            colors,
            depth,
            ..
        } => (Some(program), Some(draws), colors, depth),
        Operation::AttachmentPass { colors, depth, .. } => (None, None, colors, depth),
        _ => unreachable!(),
    };
    buffer_scopes(
        graph,
        pass,
        &slot.resource_map,
        &mut slot.buffer_scopes,
        |index, scope| {
            transition(
                &slot.list,
                slot.buffers[index].raw(),
                &mut slot.states[index],
                scope,
            );
        },
    );
    let storage = slot.raster.as_mut().unwrap();
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Image(_)
        ) {
            let image = &mut storage.images[slot.resource_map[access.resource]];
            image.raster_scope |= match access.purpose {
                AccessPurpose::ColorAttachment => D3D12_RESOURCE_STATE_RENDER_TARGET,
                AccessPurpose::DepthAttachment => D3D12_RESOURCE_STATE_DEPTH_WRITE,
                AccessPurpose::Sampled => shader_read_state(access.stages),
                _ => unreachable!("trusted Raster image purposes"),
            };
        }
    }
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Image(_)
        ) {
            let image = &mut storage.images[slot.resource_map[access.resource]];
            let state = std::mem::replace(&mut image.raster_scope, D3D12_RESOURCE_STATE_COMMON);
            if state != D3D12_RESOURCE_STATE_COMMON {
                transition(&slot.list, image.native.raw(), &mut image.state, state);
                image.uav_write = false;
            }
        }
    }
    let color = graph.raster_attachments(colors).first().map(|attachment| {
        (
            attachment,
            &storage.images[slot.resource_map[attachment.view.image().index]],
        )
    });
    let depth = depth.map(|index| {
        let attachment = graph.raster_attachment(index);
        (
            attachment,
            &storage.images[slot.resource_map[attachment.view.image().index]],
        )
    });
    let mut color_handle = None;
    let mut depth_handle = None;
    let mut extent = [0, 0];
    if let Some((attachment, image)) = color {
        let handle = image.rtv.unwrap();
        color_handle = Some(handle);
        extent = [image.desc.width, image.desc.height];
        if attachment.ops.load == LoadAction::Clear
            && attachment.clear.map(f32::to_bits) == [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits)
        {
            // SAFETY: Prepared RTV matches this retained image in RENDER_TARGET.
            // Graph guarantees finite clear color and complete attachment extent.
            unsafe {
                slot.list
                    .ClearRenderTargetView(handle, &attachment.clear, &[])
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
    let Some(program) = program else {
        return Ok(());
    };
    let pipeline = programs
        .iter()
        .find(|pipeline| pipeline.matches(program, depth.is_some()))
        .unwrap();
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
    // SAFETY: Immutable PSO matches attachment formats/depth and prepared root
    // signature; the live images and descriptor heaps stay leased. Supported
    // extents fit D3D12 viewport/scissor limits established at image creation.
    unsafe {
        slot.list.SetPipelineState(&*pipeline.pipeline);
        slot.list.SetGraphicsRootSignature(&*pipeline.signature);
        slot.list.OMSetRenderTargets(
            color_handle.is_some() as u32,
            color_handle.as_ref().map(std::ptr::from_ref),
            false,
            depth_handle.as_ref().map(std::ptr::from_ref),
        );
        slot.list.RSSetViewports(&[viewport]);
        slot.list.RSSetScissorRects(&[rect]);
        slot.list.IASetPrimitiveTopology(D3D12_PRIMITIVE_TOPOLOGY(
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        ));
        if !program.bindings().is_empty() {
            slot.list.SetDescriptorHeaps(&[
                Some(storage.srv_heap().clone()),
                Some(storage.sampler_heap().clone()),
            ]);
        }
    }
    for draw in graph.raster_draws(draws.unwrap()) {
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
                        let image = &storage.images[slot.resource_map[view.image().index]];
                        let descriptor = slot.raster_descriptors_used;
                        let srv = D3D12_SHADER_RESOURCE_VIEW_DESC {
                            Format: super::super::resources::sampled_format(image.desc.format),
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
                        // SAFETY: Each image binding owns a distinct prepared SRV
                        // slot until actual GPU completion. Its format and single
                        // subresource match the retained physical Graph image.
                        unsafe {
                            owner.raw.CreateShaderResourceView(
                                image.native.raw(),
                                Some(&srv),
                                storage.texture_cpu(descriptor),
                            );
                            slot.list.SetGraphicsRootDescriptorTable(
                                root,
                                storage.texture_gpu(descriptor),
                            );
                        }
                        slot.raster_descriptors_used += 1;
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
            GraphPool, ImageDesc, LoadOp, PassHandle, RasterAttachments, RasterDraw,
            RasterParameters, StoreOp,
        },
        program::{ManagedRasterProgram, SamplerDescriptor},
    };
    use std::time::Duration;

    struct ScopeFixture {
        _pool: GraphPool,
        plan: CompiledGraph,
        buffers: [BufferHandle; 2],
        passes: [PassHandle; 3],
        readbacks: [PassHandle; 3],
    }

    fn scope_fixture() -> ScopeFixture {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let shared = graph
            .buffer("shared geometry", BufferDesc { size: 84 })
            .unwrap();
        let other = graph
            .buffer("other geometry", BufferDesc { size: 84 })
            .unwrap();
        for buffer in [shared, other] {
            // Zero indices and vertices form a valid degenerate triangle; the
            // first 16 bytes also provide the textured shader's zero offset.
            graph
                .upload_buffer("zero geometry", buffer, 0, &[0; 84])
                .unwrap();
        }
        let texture = graph.image("texture", ImageDesc::rgba8(1, 1)).unwrap();
        graph.upload_image("white", texture, &[255; 4]).unwrap();
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
            passes.push(
                graph
                    .raster(
                        "shared buffer scopes",
                        &ManagedRasterProgram::textured_mesh(),
                        [RasterDraw {
                            bindings: &bindings,
                            vertices,
                            indices: shared,
                            index_count: 3,
                            parameters: RasterParameters::default().into(),
                        }],
                        RasterAttachments {
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
            passes: passes.try_into().unwrap(),
            readbacks: readbacks.try_into().unwrap(),
        }
    }

    #[test]
    fn buffer_read_scopes_combine_once_and_do_not_leak_into_later_passes() {
        let fixture = scope_fixture();
        let mut map = vec![usize::MAX; fixture.plan.resources.len()];
        for (index, buffer) in fixture.buffers.iter().enumerate() {
            map[buffer.index] = index;
        }
        let combined =
            D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER | D3D12_RESOURCE_STATE_INDEX_BUFFER;
        let expected = [
            vec![(0, combined)],
            vec![
                (0, D3D12_RESOURCE_STATE_INDEX_BUFFER),
                (1, D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER),
            ],
            vec![],
        ];
        let mut scopes = [D3D12_RESOURCE_STATE_COMMON; 2];
        for (pass, expected) in fixture.passes.iter().zip(expected) {
            let mut applied = Vec::new();
            buffer_scopes(
                &fixture.plan,
                pass.index,
                &map,
                &mut scopes,
                |index, scope| {
                    applied.push((index, scope));
                },
            );
            applied.sort_by_key(|(index, _)| *index);
            assert_eq!(applied, expected);
            assert_eq!(scopes, [D3D12_RESOURCE_STATE_COMMON; 2]);
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
                &[ManagedRasterProgram::textured_mesh()],
                &[],
            )
            .unwrap();
        let fixture = scope_fixture();
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
