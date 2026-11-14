//! Test-only native recording and scoped ownership for managed acceptance.

use super::super::{image::D3D12Image, resources::image_desc};
use super::*;
use std::sync::atomic::Ordering;

pub(in super::super) struct ExecutionLease {
    resource: BoundResource,
    initializes: bool,
}
impl ExecutionLease {
    fn acquire(resource: &BoundResource) -> Result<Self, D3D12RuntimeError> {
        let flag = match resource {
            BoundResource::Buffer {
                _buffer: buffer, ..
            } => &buffer.0.in_use,
            BoundResource::ImageView(view) => &view.image.0.in_use,
            BoundResource::Sampler(_) => unreachable!("samplers have no mutable GPU content"),
        };
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                D3D12RuntimeError::invalid(
                    "managed resource is reserved by recorded or pending work",
                )
            })?;
        Ok(Self {
            resource: resource.clone(),
            initializes: false,
        })
    }
}
impl ExecutionLease {
    pub(in super::super) fn complete(&self) {
        if self.initializes
            && let BoundResource::ImageView(view) = &self.resource
        {
            view.image.0.initialized.store(true, Ordering::Release);
        }
    }
}
impl Drop for ExecutionLease {
    fn drop(&mut self) {
        let flag = match &self.resource {
            BoundResource::Buffer {
                _buffer: buffer, ..
            } => &buffer.0.in_use,
            BoundResource::ImageView(view) => &view.image.0.in_use,
            BoundResource::Sampler(_) => unreachable!(),
        };
        flag.store(false, Ordering::Release);
    }
}

pub(in super::super) struct FixtureRecording {
    pub(in super::super) readback: DeviceBuffer,
    pub(in super::super) size: usize,
    pub(in super::super) staging: Vec<DeviceBuffer>,
    pub(in super::super) leases: Vec<ExecutionLease>,
}

fn upload_image(
    list: &ID3D12GraphicsCommandList,
    image: &D3D12Image,
    pixels: &[u8],
) -> Result<DeviceBuffer, D3D12RuntimeError> {
    let owner = &image.0.owner;
    let desc = image.desc();
    let texel_bytes = match desc.format {
        crate::program::ImageFormat::Rgba8Unorm => 4,
        crate::program::ImageFormat::Rgba16Float => 8,
    };
    let row_bytes = desc.width as usize * texel_bytes;
    if pixels.len() != row_bytes * desc.height as usize {
        return Err(D3D12RuntimeError::invalid(
            "sampled fixture pixels must cover the complete image",
        ));
    }
    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let mut total = 0;
    // SAFETY: The creation boundary established this exact single-subresource
    // uncompressed description. Outputs are correctly typed and live locally.
    unsafe {
        owner.raw.GetCopyableFootprints(
            &image_desc(desc),
            0,
            1,
            0,
            Some(&mut footprint),
            None,
            None,
            Some(&mut total),
        )
    };
    if total == u64::MAX || total > MAX_BUFFER_BYTES {
        return Err(D3D12RuntimeError::unsupported(
            "texture upload footprint exceeds supported staging size",
        ));
    }
    let total = usize::try_from(total).map_err(|_| {
        D3D12RuntimeError::unsupported("texture footprint exceeds CPU address space")
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(total)
        .map_err(|_| D3D12RuntimeError::capacity("texture staging CPU allocation failed"))?;
    bytes.resize(total, 0);
    for row in 0..desc.height as usize {
        let offset = footprint.Offset as usize + row * footprint.Footprint.RowPitch as usize;
        // GetCopyableFootprints may omit the final row's padding from total.
        // Copy only the actual texel bytes, not a full padded final-row slice.
        bytes[offset..offset + row_bytes]
            .copy_from_slice(&pixels[row * row_bytes..(row + 1) * row_bytes]);
    }
    let upload = DeviceBuffer::new(owner, total as u64, D3D12_HEAP_TYPE_UPLOAD)?;
    // SAFETY: New, exact-sized upload ownership excludes all GPU access and
    // overlaps. The CPU packing buffer is initialized and independently owned.
    unsafe { upload.write(owner, &bytes) }?;
    let mut state = D3D12_RESOURCE_STATE_COMMON;
    super::super::graph::transition(
        list,
        image.0.native.raw(),
        &mut state,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    let mut src = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(upload.raw().clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: footprint,
        },
    };
    let mut dst = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(image.0.native.raw().clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    // SAFETY: Native footprints define the aligned source, complete image extent
    // and copy range. COPY_DEST and GENERIC_READ are valid states. Work retains
    // the Image/View and upload allocation through cancellation or completion.
    unsafe {
        list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
        ManuallyDrop::drop(&mut src.pResource);
        ManuallyDrop::drop(&mut dst.pResource);
    }
    super::super::graph::transition(
        list,
        image.0.native.raw(),
        &mut state,
        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
    );
    Ok(upload)
}

impl D3D12Bindings {
    pub(in super::super) fn check_owner(
        &self,
        owner: &Arc<DeviceOwner>,
    ) -> Result<(), D3D12RuntimeError> {
        require_same_owner(owner, &self.0._program.0.owner)
    }
    pub(in super::super) fn record_fixture(
        &self,
        list: &ID3D12GraphicsCommandList,
        pixels: Option<&[u8]>,
    ) -> Result<FixtureRecording, D3D12RuntimeError> {
        let binding = &self.0;
        let program = &binding._program.0;
        let storage_index = self
            .layout()
            .iter()
            .position(|slot| slot.kind == BindingType::StorageWrite)
            .unwrap();
        let (output, range) = match &binding.resources[storage_index] {
            BoundResource::Buffer {
                _buffer: buffer,
                _range: range,
            } => (buffer, range),
            _ => unreachable!("binding creation established the storage kind"),
        };
        let mut leases = Vec::new();
        for (slot, resource) in self.layout().iter().zip(&binding.resources) {
            if matches!(
                slot.kind,
                BindingType::StorageWrite | BindingType::SampledImage
            ) {
                leases.push(ExecutionLease::acquire(resource)?);
            }
        }
        // These acceptance shaders write exactly their minimum storage ABI;
        // min_size alone is not a write guarantee for arbitrary Programs.
        let output_bytes = self.layout()[storage_index].min_size as usize;
        let readback = DeviceBuffer::new(
            &program.owner,
            output_bytes as u64,
            D3D12_HEAP_TYPE_READBACK,
        )?;
        let mut staging = Vec::new();
        for resource in &binding.resources {
            if let BoundResource::ImageView(view) = resource {
                if let Some(pixels) = pixels {
                    staging.push(upload_image(list, &view.image, pixels)?);
                    for lease in &mut leases {
                        if let BoundResource::ImageView(leased) = &lease.resource
                            && Arc::ptr_eq(&leased.image.0, &view.image.0)
                        {
                            lease.initializes = true;
                        }
                    }
                } else {
                    if !view.image.0.initialized.load(Ordering::Acquire) {
                        return Err(D3D12RuntimeError::invalid(
                            "sampled fixture image has no completed upload",
                        ));
                    }
                    let mut state = D3D12_RESOURCE_STATE_COMMON;
                    super::super::graph::transition(
                        list,
                        view.image.0.native.raw(),
                        &mut state,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                }
            }
        }
        let mut output_state = D3D12_RESOURCE_STATE_COMMON;
        super::super::graph::transition(
            list,
            output.0._native.raw(),
            &mut output_state,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        binding._heaps.bind(list);
        // SAFETY: Complete immutable bindings establish all slot/kind/owner/range
        // guarantees. Uploaded image content and exclusive leases cover sampled
        // reads; the reviewed shaders initialize exactly their manifested output bytes.
        unsafe {
            list.SetPipelineState(&program._pipeline);
            list.SetComputeRootSignature(&program._signature);
            for (root, (slot, resource)) in self.layout().iter().zip(&binding.resources).enumerate()
            {
                match resource {
                    BoundResource::Buffer {
                        _buffer: buffer,
                        _range: range,
                    } => {
                        let address = D3D12_GPU_VIRTUAL_ADDRESS(
                            buffer.0._native.raw().GetGPUVirtualAddress().0 + range.offset,
                        );
                        match slot.kind {
                            BindingType::Uniform => {
                                list.SetComputeRootConstantBufferView(root as u32, address)
                            }
                            BindingType::StorageWrite => {
                                list.SetComputeRootUnorderedAccessView(root as u32, address)
                            }
                            _ => unreachable!(),
                        }
                    }
                    BoundResource::ImageView(_) | BoundResource::Sampler(_) => list
                        .SetComputeRootDescriptorTable(
                            root as u32,
                            binding._heaps._tables[root].unwrap(),
                        ),
                }
            }
            list.Dispatch(1, 1, 1);
        }
        super::super::graph::transition(
            list,
            output.0._native.raw(),
            &mut output_state,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        // SAFETY: UAV writes precede this transition and exact initialized range.
        unsafe {
            list.CopyBufferRegion(
                readback.raw(),
                0,
                output.0._native.raw(),
                range.offset,
                output_bytes as u64,
            )
        };
        super::super::graph::transition(
            list,
            output.0._native.raw(),
            &mut output_state,
            D3D12_RESOURCE_STATE_COMMON,
        );
        for resource in &binding.resources {
            if let BoundResource::ImageView(view) = resource {
                let mut state = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
                super::super::graph::transition(
                    list,
                    view.image.0.native.raw(),
                    &mut state,
                    D3D12_RESOURCE_STATE_COMMON,
                );
            }
        }
        Ok(FixtureRecording {
            readback,
            size: output_bytes,
            staging,
            leases,
        })
    }
}
