//! Device-only buffers and images/views, independent of logical Graph state.

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CString;
use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use super::error::RuntimeError;
use super::memory::MemoryOwner;

/// Owns the buffer and allocation together. Initialization and scheduled access
/// belong to the graph runtime, independently of this physical allocation.
pub(super) struct DeviceBuffer {
    pub(super) owner: Arc<MemoryOwner>,
    pub(super) handle: vk::Buffer,
    pub(super) size: u64,
    allocation: vk_mem::Allocation,
}

fn create_buffer<T>(
    size: u64,
    max_buffer_size: u64,
    create: impl FnOnce(
        &vk::BufferCreateInfo<'_>,
        &vk_mem::AllocationCreateInfo,
    ) -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    if size == 0 || size > max_buffer_size {
        return Err(RuntimeError::invalid(format!(
            "graph buffer size {size} must be nonzero and not exceed Device maxBufferSize {max_buffer_size}"
        )));
    }
    let info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER
                | vk::BufferUsageFlags::UNIFORM_BUFFER
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let allocation = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    create(&info, &allocation)
}

impl DeviceBuffer {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        size: u64,
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_variables))] name: &str,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        #[cfg(feature = "vulkan-diagnostics")]
        let name = CString::new(name)
            .map_err(|_| RuntimeError::invalid("graph buffer debug name contains a NUL byte"))?;
        let (handle, allocation) =
            create_buffer(size, owner.device.max_buffer_size(), |info, allocation| {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ResourceCreate,
                    1,
                );
                // SAFETY: VMA and its Device are retained by owner. This request
                // creates an unaliased, device-only buffer with no escaped mapping.
                let result = unsafe { owner.raw.create_buffer(info, allocation) };
                if let Err(result) = result {
                    owner.device.observe_result(result);
                }
                result.map_err(|result| {
                    RuntimeError::native(
                        format!("vmaCreateBuffer for graph buffer ({size} bytes)"),
                        owner.device.name(),
                        result,
                    )
                })
            })?;
        // Own successful native creation before the fallible naming operation.
        let buffer = Self {
            owner,
            handle,
            size,
            allocation,
        };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            // SAFETY: The uniquely owned buffer belongs to this live Device. The
            // CString remains valid through this synchronous naming operation.
            unsafe {
                buffer
                    .owner
                    .device
                    .debug_utils()
                    .set_debug_utils_object_name(
                        &vk::DebugUtilsObjectNameInfoEXT::default()
                            .object_handle(handle)
                            .object_name(&name),
                    )
            }
            .map_err(|result| {
                buffer.owner.device.observe_result(result);
                RuntimeError::native(
                    "vkSetDebugUtilsObjectNameEXT for graph buffer",
                    buffer.owner.device.name(),
                    result,
                )
            })?;
        }
        Ok(buffer)
    }
}

impl Drop for DeviceBuffer {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ResourceDestroy, 1);
        // SAFETY: Graph recording/pending owners retain the physical allocation.
        // Destruction is reached only before submit or after safe retirement;
        // unresolved submissions retain their complete ownership chain instead.
        // VMA and Device remain live through this owner's Arc.
        unsafe {
            self.owner
                .raw
                .destroy_buffer(self.handle, &mut self.allocation)
        };
    }
}

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}

/// Allocation and its view are one owner; layout/content state belongs to the
/// canonical runtime entry, independently of this immutable storage identity.
pub(super) struct DeviceImage {
    owner: Arc<MemoryOwner>,
    allocation: vk_mem::Allocation,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    /// Full-chain sampled View; aliases `view` only for single-mip images.
    pub(super) sampled_view: vk::ImageView,
    /// Single-mip Views for levels 1..mip_levels, prepared with the resource.
    mip_views: Vec<vk::ImageView>,
    pub(super) mip_levels: u32,
    pub(super) supports_linear_filtering: bool,
    pub(super) extent: vk::Extent2D,
    pub(super) format: vk::Format,
    pub(super) aspect: vk::ImageAspectFlags,
}

impl DeviceImage {
    pub(super) fn belongs_to(&self, owner: &Arc<MemoryOwner>) -> bool {
        Arc::ptr_eq(&self.owner, owner)
    }

    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        width: u32,
        height: u32,
        mip_levels: u32,
        format: vk::Format,
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_variables))] name: &str,
    ) -> Result<Self, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let (usage, aspect) = match format {
            vk::Format::R8G8B8A8_UNORM => (
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                vk::ImageAspectFlags::COLOR,
            ),
            vk::Format::R16G16B16A16_SFLOAT => (
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                vk::ImageAspectFlags::COLOR,
            ),
            vk::Format::R8G8B8A8_SRGB => (
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
                vk::ImageAspectFlags::COLOR,
            ),
            vk::Format::D32_SFLOAT => (
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
                vk::ImageAspectFlags::DEPTH,
            ),
            _ => {
                return Err(RuntimeError::unsupported(format!(
                    "unsupported Graph image format {format:?}"
                )));
            }
        };
        if width == 0
            || height == 0
            || mip_levels == 0
            || mip_levels > u32::BITS - width.max(height).leading_zeros()
        {
            return Err(RuntimeError::invalid(
                "Graph image dimensions and mip count must describe a nonempty 2D chain",
            ));
        }
        let supports_linear_filtering = sampled_linear_support(&owner, format)?;
        let query = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage);
        let mut support = vk::ImageFormatProperties2::default();
        // SAFETY: Live Device retains its Instance and adapter; the typed input
        // describes exactly the format/type/tiling/usage created below.
        unsafe {
            owner
                .device
                .instance_raw()
                .get_physical_device_image_format_properties2(
                    owner.device.physical_device(),
                    &query,
                    &mut support,
                )
        }
        .map_err(|result| {
            native(
                &owner,
                "vkGetPhysicalDeviceImageFormatProperties2(Graph)",
                result,
            )
        })?;
        let limits = support.image_format_properties;
        if width > limits.max_extent.width
            || height > limits.max_extent.height
            || limits.max_extent.depth < 1
            || limits.max_mip_levels < mip_levels
            || limits.max_array_layers < 1
            || !limits.sample_counts.contains(vk::SampleCountFlags::TYPE_1)
        {
            return Err(RuntimeError::unsupported(format!(
                "Graph image {width}x{height} {format:?} exceeds format limits {limits:?}"
            )));
        }
        let limits = owner.device.limits();
        if usage.intersects(
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        ) && (width > limits.max_framebuffer_width
            || height > limits.max_framebuffer_height
            || width > limits.max_viewport_dimensions[0]
            || height > limits.max_viewport_dimensions[1]
            || width as f32 > limits.viewport_bounds_range[1]
            || height as f32 > limits.viewport_bounds_range[1]
            || limits.viewport_bounds_range[0] > 0.0)
        {
            return Err(RuntimeError::unsupported(
                "Graph image exceeds framebuffer/viewport dimensions",
            ));
        }
        let extent = vk::Extent2D { width, height };
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let allocation_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let mip_views = Vec::with_capacity((mip_levels - 1) as usize);
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ResourceCreate, 1);
        // SAFETY: VMA retains this Device, internally synchronizes allocation,
        // and satisfies the exact native image's requirements/bind operation.
        let (image, allocation) = unsafe { owner.raw.create_image(&info, &allocation_info) }
            .map_err(|result| native(&owner, "vmaCreateImage(Graph)", result))?;
        let mut result = Self {
            owner,
            allocation,
            image,
            view: vk::ImageView::null(),
            sampled_view: vk::ImageView::null(),
            mip_views,
            mip_levels,
            supports_linear_filtering,
            extent,
            format,
            aspect,
        };
        result.view = result.create_view(0, 1, usage)?;
        for mip in 1..mip_levels {
            let view = result.create_view(mip, 1, usage)?;
            result.mip_views.push(view);
        }
        result.sampled_view = if mip_levels == 1 {
            result.view
        } else {
            result.create_view(0, mip_levels, vk::ImageUsageFlags::SAMPLED)?
        };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            let name = CString::new(name)
                .map_err(|_| RuntimeError::invalid("Graph image name contains NUL"))?;
            let naming = vk::DebugUtilsObjectNameInfoEXT::default()
                .object_handle(image)
                .object_name(&name);
            // SAFETY: The Device owns this live image and consumes the name inline.
            unsafe {
                result
                    .owner
                    .device
                    .debug_utils()
                    .set_debug_utils_object_name(&naming)
            }
            .map_err(|error| {
                native(
                    &result.owner,
                    "vkSetDebugUtilsObjectNameEXT(Graph image)",
                    error,
                )
            })?;
            let view_name = CString::new(format!("{}-view", name.to_string_lossy()))
                .expect("image name already checked for NUL");
            let view_naming = vk::DebugUtilsObjectNameInfoEXT::default()
                .object_handle(result.view)
                .object_name(&view_name);
            // SAFETY: This owner holds the live view; the name is consumed inline.
            unsafe {
                result
                    .owner
                    .device
                    .debug_utils()
                    .set_debug_utils_object_name(&view_naming)
            }
            .map_err(|error| {
                native(
                    &result.owner,
                    "vkSetDebugUtilsObjectNameEXT(Graph view)",
                    error,
                )
            })?;
        }
        Ok(result)
    }

    /// Native Graph descriptors support a single level or the complete chain.
    /// Every View is created at preparation, never while recording a frame.
    pub(super) fn view_for_mips(&self, base: u32, count: u32) -> Option<vk::ImageView> {
        if count == 1 && base < self.mip_levels {
            return Some(if base == 0 {
                self.view
            } else {
                self.mip_views[(base - 1) as usize]
            });
        }
        (base == 0 && count == self.mip_levels).then_some(self.sampled_view)
    }

    fn create_view(
        &self,
        base: u32,
        count: u32,
        usage: vk::ImageUsageFlags,
    ) -> Result<vk::ImageView, RuntimeError> {
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(self.aspect)
            .base_mip_level(base)
            .level_count(count)
            .layer_count(1);
        let mut view_usage = vk::ImageViewUsageCreateInfo::default().usage(usage);
        let info = vk::ImageViewCreateInfo::default()
            .image(self.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(self.format)
            .subresource_range(range)
            .push_next(&mut view_usage);
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: The live owner holds the exact-format image; this checked
        // level range and usage are subsets of its immutable creation contract.
        // The caller immediately retains every successful View for Drop.
        unsafe { self.owner.device.raw().create_image_view(&info, None) }
            .map_err(|error| native(&self.owner, "vkCreateImageView(Graph)", error))
    }
}

impl Drop for DeviceImage {
    fn drop(&mut self) {
        // SAFETY: Runtime leases retain storage through recording and GPU use.
        // The view is destroyed before its image, then the retained VMA/Device.
        unsafe {
            if self.sampled_view != vk::ImageView::null() && self.sampled_view != self.view {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                self.owner
                    .device
                    .raw()
                    .destroy_image_view(self.sampled_view, None);
            }
            for view in self.mip_views.drain(..) {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                self.owner.device.raw().destroy_image_view(view, None);
            }
            if self.view != vk::ImageView::null() {
                #[cfg(feature = "memory-profiling")]
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ObjectDestroy,
                    1,
                );
                self.owner.device.raw().destroy_image_view(self.view, None);
            }
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::ResourceDestroy, 1);
            self.owner
                .raw
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// Native image representation; support for each usage is checked separately.
pub(super) fn image_format(format: crate::ImageFormat) -> vk::Format {
    match format {
        crate::ImageFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        crate::ImageFormat::Rgba8Srgb => vk::Format::R8G8B8A8_SRGB,
        crate::ImageFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        crate::ImageFormat::Depth32Float => vk::Format::D32_SFLOAT,
    }
}

/// Cache format filtering support at creation; descriptor construction checks
/// it only when the chosen immutable sampler requires linear filtering.
pub(super) fn sampled_linear_support(
    owner: &MemoryOwner,
    format: vk::Format,
) -> Result<bool, RuntimeError> {
    // SAFETY: The retained Device owns this live adapter and Instance. This is
    // an immutable capability query at the resource creation boundary.
    let properties = unsafe {
        owner
            .device
            .instance_raw()
            .get_physical_device_format_properties(owner.device.physical_device(), format)
    };
    let features = properties.optimal_tiling_features;
    if !features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE) {
        return Err(RuntimeError::unsupported(
            "image format does not support optimal sampled images",
        ));
    }
    Ok(features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_size_is_rejected_before_native_creation() {
        for size in [0, 97, u64::MAX] {
            let error =
                create_buffer::<()>(size, 96, |_, _| panic!("invalid size must not reach VMA"))
                    .unwrap_err();
            assert_eq!(
                error.kind(),
                super::super::error::RuntimeErrorKind::InvalidInput
            );
            assert!(error.message.contains("maxBufferSize 96"));
        }
    }

    #[test]
    fn graph_buffer_request_supports_actual_operations_and_preserves_failure() {
        let error = create_buffer::<()>(96, 96, |info, allocation| {
            assert_eq!(info.size, 96);
            assert_eq!(info.sharing_mode, vk::SharingMode::EXCLUSIVE);
            assert_eq!(
                info.usage,
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::VERTEX_BUFFER
                    | vk::BufferUsageFlags::INDEX_BUFFER
                    | vk::BufferUsageFlags::UNIFORM_BUFFER
                    | vk::BufferUsageFlags::STORAGE_BUFFER
            );
            assert_eq!(allocation.usage, vk_mem::MemoryUsage::AutoPreferDevice);
            assert!(allocation.flags.is_empty());
            Err(RuntimeError::native(
                "vmaCreateBuffer for graph buffer (96 bytes)",
                "test device",
                vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), super::super::error::RuntimeErrorKind::Native);
        assert!(error.message.contains("vmaCreateBuffer"));
        assert!(error.message.contains("96 bytes"));
        assert!(error.message.contains("ERROR_OUT_OF_DEVICE_MEMORY"));
    }
}
