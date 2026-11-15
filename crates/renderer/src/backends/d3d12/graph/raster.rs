//! Owned graphics recording into exclusively leased, prepared native storage.

use std::{mem::ManuallyDrop, ops::Range};

use super::super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{ExecutionSlot, FrameWork, ReadbackImage, ReadbackMeta},
    raster::{IMAGE_ROOT, INLINE_ROOT, RasterPipeline, SAMPLER_ROOT, UNIFORM_ROOT},
    raster_storage::RasterStorage,
};
use super::transition;
use crate::graph::{
    AccessPurpose, BindingOperands, CompiledGraph, ImageFormat, LoadAction, Operation, ResourceDesc,
};

pub(super) fn align_transfer(value: u64) -> Result<u64, D3D12RuntimeError> {
    value
        .checked_add(511)
        .map(|value| value & !511)
        .ok_or_else(|| D3D12RuntimeError::capacity("texture transfer offset overflow"))
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
    if graph.resources[index].import.is_some() {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph image import is not implemented",
        ));
    }
    let ResourceDesc::Image(desc) = graph.resources[index].desc else {
        unreachable!()
    };
    if !desc.is_single_subresource() {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph images currently require one mip and layer",
        ));
    }
    let (physical, image) = storage
        .images
        .iter_mut()
        .enumerate()
        .find(|(_, image)| !image.used && image.desc == desc)
        .ok_or_else(|| D3D12RuntimeError::capacity("prepared Raster image class exhausted"))?;
    image.used = true;
    map[index] = physical;
    Ok(())
}

pub(super) fn validate_targets(
    graph: &CompiledGraph,
    colors: &Range<usize>,
    depth: &Option<usize>,
) -> Result<(), D3D12RuntimeError> {
    if colors.len() > 1 {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Raster supports one color attachment",
        ));
    }
    if let Some(index) = depth {
        let attachment = graph.raster_attachment(*index);
        if attachment.ops.load == LoadAction::Clear && !(0.0..=1.0).contains(&attachment.clear[0]) {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 depth clear must be normalized",
            ));
        }
    }
    Ok(())
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

/// Both locations are stack-owned COM references; release them on unwind too.
struct TextureLocation(D3D12_TEXTURE_COPY_LOCATION);
impl TextureLocation {
    fn image(resource: &ID3D12Resource) -> Self {
        Self(D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
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
    let mut footprint = image.footprint;
    footprint.Offset += base;
    for row in 0..image.rows {
        // SAFETY: Preflight bounded the cached native footprint in the leased
        // upload arena. Graph creation established packed full-image bytes.
        unsafe {
            slot.upload.write_at(
                owner,
                footprint.Offset + row as u64 * u64::from(footprint.Footprint.RowPitch),
                &bytes[row * image.row_bytes..(row + 1) * image.row_bytes],
            )
        }?;
    }
    transition(
        &slot.list,
        image.native.raw(),
        &mut image.state,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    let source = TextureLocation::buffer(slot.upload.raw(), footprint);
    let destination = TextureLocation::image(image.native.raw());
    // SAFETY: Complete single-subresource copy uses cached native pitch/format,
    // aligned bounded placement, COPY_DEST/GENERIC_READ states and retained
    // owners. Depth copies use zero destination offsets and no source box.
    unsafe {
        slot.list
            .CopyTextureRegion(&destination.0, 0, 0, 0, &source.0, None)
    };
    slot.upload_used = base + image.total_bytes;
    Ok(())
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
    let source = TextureLocation::image(image.native.raw());
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
    slot.buffer_scopes[..slot.buffers_used].fill(D3D12_RESOURCE_STATE_COMMON);
    // Graph derives these uses once from immutable commands. Native read states
    // are combined per buffer, including a shared Vertex/Index/Uniform resource.
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Buffer(_)
        ) {
            slot.buffer_scopes[slot.resource_map[access.resource]] |= match access.purpose {
                AccessPurpose::Vertex | AccessPurpose::Uniform => {
                    D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER
                }
                AccessPurpose::Index => D3D12_RESOURCE_STATE_INDEX_BUFFER,
                _ => unreachable!("reviewed Raster has read-only geometry/uniform buffer uses"),
            };
        }
    }
    for index in 0..slot.buffers_used {
        if slot.buffer_scopes[index] != D3D12_RESOURCE_STATE_COMMON {
            transition(
                &slot.list,
                slot.buffers[index].raw(),
                &mut slot.states[index],
                slot.buffer_scopes[index],
            );
        }
    }
    let storage = slot.raster.as_mut().unwrap();
    for access in graph.accesses(pass) {
        if matches!(
            graph.resources[access.resource].desc,
            ResourceDesc::Image(_)
        ) {
            let image = &mut storage.images[slot.resource_map[access.resource]];
            let state = match access.purpose {
                AccessPurpose::ColorAttachment => D3D12_RESOURCE_STATE_RENDER_TARGET,
                AccessPurpose::DepthAttachment => D3D12_RESOURCE_STATE_DEPTH_WRITE,
                AccessPurpose::Sampled => D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                _ => unreachable!("reviewed Raster image purposes"),
            };
            transition(&slot.list, image.native.raw(), &mut image.state, state);
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
            // SAFETY: Prepared full D32 DSV is DEPTH_WRITE; preflight established
            // normalized clear depth, and this lease retains descriptor/image.
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
        if program.is_textured() {
            slot.list.SetDescriptorHeaps(&[
                Some(storage.srv_heap().clone()),
                Some(storage.sampler_heap().clone()),
            ]);
        }
    }
    for draw in graph.raster_draws(draws.unwrap()) {
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
                        D3D12RuntimeError::invalid("Raster index exceeds the vertex buffer range")
                    }
                }
            })?;
        let mut parameters = [0u32; 20];
        for (word, value) in parameters.iter_mut().zip(
            draw.parameters
                .transform
                .iter()
                .flatten()
                .chain(&draw.parameters.tint),
        ) {
            *word = value.to_bits();
        }
        // SAFETY: The retained buffer capacities cover immutable Graph ranges;
        // validated index contents fit the packed vertex stream. Root constants
        // are exactly the reviewed 80-byte ABI in column-major scalar order.
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
            slot.list
                .SetGraphicsRoot32BitConstants(INLINE_ROOT, 20, parameters.as_ptr().cast(), 0);
        }
        if program.is_textured() {
            let bindings = graph.raster_bindings(&draw.bindings);
            let BindingOperands::Buffer { resource, range } = bindings[0] else {
                unreachable!()
            };
            let BindingOperands::ImageView(view) = bindings[1] else {
                unreachable!()
            };
            let image = &storage.images[slot.resource_map[view.image().index]];
            let descriptor = slot.raster_draws_used;
            let srv = D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: match image.desc.format {
                    ImageFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
                    ImageFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
                    ImageFormat::Depth32Float => DXGI_FORMAT_R32_FLOAT,
                },
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING as u32,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                        PlaneSlice: 0,
                        ResourceMinLODClamp: 0.0,
                    },
                },
            };
            // SAFETY: This draw owns a distinct prepared descriptor slot, never
            // overwritten while recording/GPU uses exist. Canonical Graph binding
            // order and 256-byte CBV address were established before recording;
            // every referenced native resource remains in this execution lease.
            unsafe {
                owner.raw.CreateShaderResourceView(
                    image.native.raw(),
                    Some(&srv),
                    storage.texture_cpu(descriptor),
                );
                slot.list.SetGraphicsRootConstantBufferView(
                    UNIFORM_ROOT,
                    D3D12_GPU_VIRTUAL_ADDRESS(
                        slot.buffers[slot.resource_map[resource]]
                            .raw()
                            .GetGPUVirtualAddress()
                            .0
                            + range.offset,
                    ),
                );
                slot.list
                    .SetGraphicsRootDescriptorTable(IMAGE_ROOT, storage.texture_gpu(descriptor));
                slot.list
                    .SetGraphicsRootDescriptorTable(SAMPLER_ROOT, storage.sampler_gpu());
            }
        }
        // SAFETY: Every vertex/index/binding operand is valid and retained, all
        // required state is set, and native attachment order preserves draw order.
        unsafe { slot.list.DrawIndexedInstanced(draw.index_count, 1, 0, 0, 0) };
        slot.raster_draws_used += 1;
    }
    Ok(())
}
