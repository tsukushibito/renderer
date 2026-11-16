//! Explicitly prepared Graph images and descriptor storage for one frame slot.

use crate::graph::{ImageDesc, ImageFormat};

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
    resources::DeviceImage,
};

/// One exact image description and the number reserved per execution slot.
#[derive(Clone, Copy, Debug)]
pub struct D3D12ImageClass {
    pub desc: ImageDesc,
    pub count: usize,
}

pub(super) struct GraphImage {
    pub(super) desc: ImageDesc,
    pub(super) native: super::registry::PhysicalRef,
    pub(super) state: D3D12_RESOURCE_STATES,
    pub(super) uav_write: bool,
    pub(super) used: bool,
    pub(super) rtv: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
    pub(super) dsv: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
    pub(super) footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    pub(super) total_bytes: u64,
    pub(super) row_bytes: usize,
    pub(super) rows: usize,
}

struct DescriptorHeap {
    raw: NativeObject<ID3D12DescriptorHeap>,
    #[cfg(feature = "memory-profiling")]
    count: u64,
}

impl DescriptorHeap {
    fn new(
        owner: &DeviceOwner,
        count: usize,
        kind: D3D12_DESCRIPTOR_HEAP_TYPE,
        visible: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        let count = u32::try_from(count)
            .ok()
            .filter(|count| *count != 0)
            .ok_or_else(|| D3D12RuntimeError::invalid("invalid prepared descriptor count"))?;
        let desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: kind,
            NumDescriptors: count,
            Flags: if visible {
                D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE
            } else {
                D3D12_DESCRIPTOR_HEAP_FLAG_NONE
            },
            NodeMask: 0,
        };
        // SAFETY: Counts are nonzero and fit UINT; each caller supplies a valid
        // heap type and its permitted visibility at the preparation boundary.
        let raw = object(
            owner,
            "ID3D12Device::CreateDescriptorHeap(Graph)",
            || unsafe { owner.raw.CreateDescriptorHeap(&desc) },
        )?;
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(
            crate::measurement::NativeEvent::DescriptorAllocate,
            u64::from(count),
        );
        Ok(Self {
            raw,
            #[cfg(feature = "memory-profiling")]
            count: u64::from(count),
        })
    }
}

impl Drop for DescriptorHeap {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(
            crate::measurement::NativeEvent::DescriptorFree,
            self.count,
        );
    }
}

pub(super) struct RasterStorage {
    // Release descriptor owners before the images they reference. The containing
    // FramePool keeps the Device alive through every field's final destruction.
    _rtv_heap: Option<DescriptorHeap>,
    _dsv_heap: Option<DescriptorHeap>,
    srv: DescriptorHeap,
    sampler: DescriptorHeap,
    pub(super) images: Vec<GraphImage>,
    texture_cpu_start: D3D12_CPU_DESCRIPTOR_HANDLE,
    texture_gpu_start: D3D12_GPU_DESCRIPTOR_HANDLE,
    sampler_gpu_start: D3D12_GPU_DESCRIPTOR_HANDLE,
    texture_step: u32,
    descriptor_capacity: usize,
    pub(super) compute_descriptor_start: usize,
    pub(super) compute_descriptor_capacity: usize,
}

fn validate_class(class: D3D12ImageClass) -> Result<(), D3D12RuntimeError> {
    if class.count == 0 || class.desc.byte_len().is_none() {
        return Err(D3D12RuntimeError::invalid(
            "prepared image classes require a nonzero count and valid extent",
        ));
    }
    if !class.desc.is_single_subresource()
        || class.desc.width > D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32
        || class.desc.height > D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32
    {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph images require a supported single mip/layer extent",
        ));
    }
    Ok(())
}

fn require_format(
    owner: &DeviceOwner,
    format: DXGI_FORMAT,
    required: D3D12_FORMAT_SUPPORT1,
) -> Result<(), D3D12RuntimeError> {
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: The feature enum, initialized output structure and its byte count
    // match. The Device remains owned throughout this synchronous query.
    unsafe {
        owner.raw.CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT,
            std::ptr::from_mut(&mut support).cast(),
            size_of_val(&support) as u32,
        )
    }
    .ok()
    .map_err(|error| owner.native("CheckFeatureSupport(Graph image)", error))?;
    if support.Support1 & required != required {
        return Err(D3D12RuntimeError::unsupported(
            "prepared Graph image format lacks required native capabilities",
        ));
    }
    Ok(())
}

fn require_image_format(owner: &DeviceOwner, format: ImageFormat) -> Result<(), D3D12RuntimeError> {
    match format {
        ImageFormat::Depth32Float => {
            require_format(
                owner,
                DXGI_FORMAT_R32_TYPELESS,
                D3D12_FORMAT_SUPPORT1_TEXTURE2D,
            )?;
            require_format(
                owner,
                DXGI_FORMAT_D32_FLOAT,
                D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_DEPTH_STENCIL,
            )?;
            require_format(
                owner,
                DXGI_FORMAT_R32_FLOAT,
                D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE,
            )
        }
        ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float => require_format(
            owner,
            match format {
                ImageFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
                ImageFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
                ImageFormat::Depth32Float => unreachable!(),
            },
            D3D12_FORMAT_SUPPORT1_TEXTURE2D
                | D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE
                | D3D12_FORMAT_SUPPORT1_RENDER_TARGET,
        ),
    }
}

fn checked_handle_capacity(
    cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
    gpu: Option<D3D12_GPU_DESCRIPTOR_HANDLE>,
    step: u32,
    count: usize,
) -> Result<(), D3D12RuntimeError> {
    let last = count.saturating_sub(1);
    if step == 0
        || last
            .checked_mul(step as usize)
            .and_then(|offset| cpu.ptr.checked_add(offset))
            .is_none()
        || gpu.is_some_and(|gpu| {
            (last as u64)
                .checked_mul(u64::from(step))
                .and_then(|offset| gpu.ptr.checked_add(offset))
                .is_none()
        })
    {
        return Err(D3D12RuntimeError::unsupported(
            "prepared descriptor handles exceed the native address range",
        ));
    }
    Ok(())
}

fn copy_layout(
    owner: &DeviceOwner,
    image: &DeviceImage,
    desc: ImageDesc,
) -> Result<(D3D12_PLACED_SUBRESOURCE_FOOTPRINT, u64, usize, usize), D3D12RuntimeError> {
    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let mut rows = 0u32;
    let mut row_bytes = 0u64;
    let mut total_bytes = 0u64;
    // SAFETY: This retained image has the exact validated single-subresource
    // description. Every output pointer matches the native parameter type.
    unsafe {
        owner.raw.GetCopyableFootprints(
            &image.raw().GetDesc(),
            0,
            1,
            0,
            Some(&mut footprint),
            Some(&mut rows),
            Some(&mut row_bytes),
            Some(&mut total_bytes),
        );
    }
    let texel_bytes = if desc.format == ImageFormat::Rgba16Float {
        8
    } else {
        4
    };
    let expected_row = u64::from(desc.width) * texel_bytes;
    let accessed_end = u64::from(rows.saturating_sub(1))
        .checked_mul(u64::from(footprint.Footprint.RowPitch))
        .and_then(|stride| footprint.Offset.checked_add(stride))
        .and_then(|start| start.checked_add(row_bytes));
    if total_bytes == u64::MAX
        || rows != desc.height
        || row_bytes != expected_row
        || u64::from(footprint.Footprint.RowPitch) < row_bytes
        || footprint.Footprint.RowPitch % D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as u32 != 0
        || footprint.Offset % D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT as u64 != 0
        || footprint.Footprint.Width != desc.width
        || footprint.Footprint.Height != desc.height
        || footprint.Footprint.Depth != 1
        || accessed_end.is_none_or(|end| end > total_bytes)
        || total_bytes > isize::MAX as u64
    {
        return Err(D3D12RuntimeError::unsupported(
            "native Graph image copy footprint is outside the supported layout",
        ));
    }
    Ok((footprint, total_bytes, row_bytes as usize, rows as usize))
}

impl RasterStorage {
    pub(super) fn bind_views(&self, owner: &DeviceOwner) {
        for image in &self.images {
            if !image.used {
                continue;
            }
            if let Some(handle) = image.rtv {
                // SAFETY: This exclusive execution slot owns the CPU descriptor;
                // the leased native image matches its prepared color class.
                unsafe {
                    owner
                        .raw
                        .CreateRenderTargetView(image.native.raw(), None, handle)
                };
            }
            if let Some(handle) = image.dsv {
                let view = D3D12_DEPTH_STENCIL_VIEW_DESC {
                    Format: DXGI_FORMAT_D32_FLOAT,
                    ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
                    Flags: D3D12_DSV_FLAG_NONE,
                    Anonymous: D3D12_DEPTH_STENCIL_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_DSV { MipSlice: 0 },
                    },
                };
                // SAFETY: The exclusive descriptor and retained single-mip/layer
                // typeless depth resource match the concrete D32 view.
                unsafe {
                    owner
                        .raw
                        .CreateDepthStencilView(image.native.raw(), Some(&view), handle)
                };
            }
        }
    }
    pub(super) fn new(
        owner: &DeviceOwner,
        classes: &[D3D12ImageClass],
        draw_capacity: usize,
        compute_descriptor_capacity: usize,
        storage_formats: [bool; 2],
    ) -> Result<Self, D3D12RuntimeError> {
        owner.ensure_active()?;
        let descriptor_capacity = draw_capacity
            .checked_add(compute_descriptor_capacity)
            .ok_or_else(|| D3D12RuntimeError::capacity("prepared descriptor capacity overflow"))?;
        // One million is the portable shader-visible CBV/SRV/UAV heap bound
        // at the supported baseline; samplers need only their single fixed slot.
        if !(1..=1_000_000).contains(&descriptor_capacity) {
            return Err(D3D12RuntimeError::invalid(
                "prepared Graph descriptor capacity must be within 1..=1000000",
            ));
        }
        let mut total = 0usize;
        let mut color_count = 0usize;
        let mut depth_count = 0usize;
        for &class in classes {
            validate_class(class)?;
            require_image_format(owner, class.desc.format)?;
            total = total
                .checked_add(class.count)
                .ok_or_else(|| D3D12RuntimeError::capacity("prepared image count overflows"))?;
            if class.desc.format == ImageFormat::Depth32Float {
                depth_count += class.count;
            } else {
                color_count += class.count;
            }
        }
        let mut images = Vec::new();
        images.try_reserve_exact(total).map_err(|_| {
            D3D12RuntimeError::capacity("prepared image metadata allocation failed")
        })?;
        let srv = DescriptorHeap::new(
            owner,
            descriptor_capacity,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            true,
        )?;
        let sampler = DescriptorHeap::new(owner, 1, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, true)?;
        let rtv_heap = (color_count != 0)
            .then(|| DescriptorHeap::new(owner, color_count, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, false))
            .transpose()?;
        let dsv_heap = (depth_count != 0)
            .then(|| DescriptorHeap::new(owner, depth_count, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, false))
            .transpose()?;
        // SAFETY: Every heap is live and owned, each handle's CPU/GPU visibility
        // matches the getter, and increments are immutable Device properties.
        let (
            texture_cpu_start,
            texture_gpu_start,
            sampler_gpu_start,
            rtv_start,
            dsv_start,
            rtv_step,
            dsv_step,
        ) = unsafe {
            (
                srv.raw.GetCPUDescriptorHandleForHeapStart(),
                srv.raw.GetGPUDescriptorHandleForHeapStart(),
                sampler.raw.GetGPUDescriptorHandleForHeapStart(),
                rtv_heap
                    .as_ref()
                    .map(|heap| heap.raw.GetCPUDescriptorHandleForHeapStart()),
                dsv_heap
                    .as_ref()
                    .map(|heap| heap.raw.GetCPUDescriptorHandleForHeapStart()),
                owner
                    .raw
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV),
                owner
                    .raw
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_DSV),
            )
        };
        checked_handle_capacity(
            texture_cpu_start,
            Some(texture_gpu_start),
            owner.descriptor_steps[0],
            descriptor_capacity,
        )?;
        if let Some(start) = rtv_start {
            checked_handle_capacity(start, None, rtv_step, color_count)?;
        }
        if let Some(start) = dsv_start {
            checked_handle_capacity(start, None, dsv_step, depth_count)?;
        }
        let mut color_index = 0usize;
        let mut depth_index = 0usize;
        for &class in classes {
            for _ in 0..class.count {
                let storage_image = match class.desc.format {
                    ImageFormat::Rgba8Unorm => storage_formats[0],
                    ImageFormat::Rgba16Float => storage_formats[1],
                    ImageFormat::Depth32Float => false,
                };
                let native = DeviceImage::graph(owner, class.desc, storage_image)?;
                let (footprint, total_bytes, row_bytes, rows) =
                    copy_layout(owner, &native, class.desc)?;
                let (rtv, dsv) = if class.desc.format == ImageFormat::Depth32Float {
                    let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: dsv_start.unwrap().ptr + depth_index * dsv_step as usize,
                    };
                    let view = D3D12_DEPTH_STENCIL_VIEW_DESC {
                        Format: DXGI_FORMAT_D32_FLOAT,
                        ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
                        Flags: D3D12_DSV_FLAG_NONE,
                        Anonymous: D3D12_DEPTH_STENCIL_VIEW_DESC_0 {
                            Texture2D: D3D12_TEX2D_DSV { MipSlice: 0 },
                        },
                    };
                    // SAFETY: The checked single-subresource typeless image is
                    // depth-capable; this concrete D32 view occupies its own
                    // in-bounds CPU-only DSV slot, retained with the image.
                    unsafe {
                        owner
                            .raw
                            .CreateDepthStencilView(native.raw(), Some(&view), handle)
                    };
                    depth_index += 1;
                    (None, Some(handle))
                } else {
                    let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: rtv_start.unwrap().ptr + color_index * rtv_step as usize,
                    };
                    // SAFETY: The checked concrete color resource permits render
                    // targets. Its default view covers the sole mip/layer, and
                    // the independent CPU descriptor slot is in bounds.
                    unsafe { owner.raw.CreateRenderTargetView(native.raw(), None, handle) };
                    color_index += 1;
                    (Some(handle), None)
                };
                images.push(GraphImage {
                    desc: class.desc,
                    native: super::registry::PhysicalRef::image(native),
                    state: D3D12_RESOURCE_STATE_COMMON,
                    uav_write: false,
                    used: false,
                    rtv,
                    dsv,
                    footprint,
                    total_bytes,
                    row_bytes,
                    rows,
                });
            }
        }
        let desc = D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
            AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: 0.0,
        };
        // SAFETY: The reviewed nearest/clamp sampler occupies the sole valid
        // sampler slot. It is initialized before any execution can use this set.
        unsafe {
            owner
                .raw
                .CreateSampler(&desc, sampler.raw.GetCPUDescriptorHandleForHeapStart());
        }
        Ok(Self {
            _rtv_heap: rtv_heap,
            _dsv_heap: dsv_heap,
            srv,
            sampler,
            images,
            texture_cpu_start,
            texture_gpu_start,
            sampler_gpu_start,
            texture_step: owner.descriptor_steps[0],
            descriptor_capacity,
            compute_descriptor_start: draw_capacity,
            compute_descriptor_capacity,
        })
    }

    pub(super) fn reset(&mut self) {
        // Only an exclusive available frame lease may reset this storage. The
        // recorder restored actual resources to COMMON before successful Close;
        // cancelled recording was never executed and cannot change their state.
        for image in &mut self.images {
            image.state = D3D12_RESOURCE_STATE_COMMON;
            image.used = false;
            image.uav_write = false;
        }
    }

    pub(super) fn texture_cpu(&self, index: usize) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        assert!(
            index < self.descriptor_capacity,
            "prepared Raster descriptor index"
        );
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.texture_cpu_start.ptr + index * self.texture_step as usize,
        }
    }

    pub(super) fn texture_gpu(&self, index: usize) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        assert!(
            index < self.descriptor_capacity,
            "prepared Raster descriptor index"
        );
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: self.texture_gpu_start.ptr + index as u64 * u64::from(self.texture_step),
        }
    }

    pub(super) fn sampler_gpu(&self) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        self.sampler_gpu_start
    }

    pub(super) fn srv_heap(&self) -> &ID3D12DescriptorHeap {
        &self.srv.raw
    }

    pub(super) fn sampler_heap(&self) -> &ID3D12DescriptorHeap {
        &self.sampler.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_classes_reject_empty_and_unsupported_image_descriptions() {
        assert!(
            validate_class(D3D12ImageClass {
                desc: ImageDesc::rgba8(2, 2),
                count: 1
            })
            .is_ok()
        );
        assert!(
            validate_class(D3D12ImageClass {
                desc: ImageDesc::rgba8(2, 2),
                count: 0
            })
            .is_err()
        );
        assert!(
            validate_class(D3D12ImageClass {
                desc: ImageDesc::rgba8(0, 2),
                count: 1
            })
            .is_err()
        );
        let mut desc = ImageDesc::rgba8(2, 2);
        desc.mip_levels = 2;
        assert!(validate_class(D3D12ImageClass { desc, count: 1 }).is_err());
    }
}
