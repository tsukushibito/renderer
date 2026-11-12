//! Unique VMA image ownership, with a deliberately narrow creation contract.
//!
//! Creation descriptions are independent of ownership so later image usages can
//! extend validation without replacing the lifetime model. Images currently
//! remain in UNDEFINED layout with no established contents or GPU-use API.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use super::device::DeviceState;
use super::instance::LOG_TARGET;
use super::memory::{MemoryOwner, VulkanAllocator};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ImageDesc {
    pub(super) extent: vk::Extent2D,
    pub(super) format: vk::Format,
    pub(super) usage: vk::ImageUsageFlags,
}

impl ImageDesc {
    pub(super) fn transfer(width: u32, height: u32) -> Self {
        Self {
            extent: vk::Extent2D { width, height },
            format: vk::Format::R8G8B8A8_UNORM,
            usage: vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST,
        }
    }

    fn validate(self) -> Result<(), ImageError> {
        let reason = if self.extent.width == 0 || self.extent.height == 0 {
            Some("image width and height must be nonzero")
        } else if self.format != vk::Format::R8G8B8A8_UNORM {
            Some("only RGBA8_UNORM images are implemented")
        } else if self.usage
            != vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST
        {
            Some("only transfer source and destination images are implemented")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(ImageError::InvalidDescription { desc: self, reason }),
            None => Ok(()),
        }
    }

    fn create_info(self) -> vk::ImageCreateInfo<'static> {
        vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(self.format)
            .extent(vk::Extent3D {
                width: self.extent.width,
                height: self.extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(self.usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
    }
}

#[derive(Debug)]
pub(super) enum ImageError {
    DeviceUnavailable {
        desc: ImageDesc,
        state: DeviceState,
    },
    InvalidDescription {
        desc: ImageDesc,
        reason: &'static str,
    },
    UnsupportedFormat {
        desc: ImageDesc,
        result: vk::Result,
    },
    UnsupportedLimits {
        desc: ImageDesc,
        limits: vk::ImageFormatProperties,
    },
    Vulkan {
        operation: &'static str,
        desc: ImageDesc,
        result: vk::Result,
    },
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceUnavailable { desc, state } => {
                write!(
                    f,
                    "image {desc:?} Device is {state:?}; normal use is stopped"
                )
            }
            Self::InvalidDescription { desc, reason } => {
                write!(f, "invalid image {desc:?}: {reason}")
            }
            Self::UnsupportedFormat { desc, result } => write!(
                f,
                "vkGetPhysicalDeviceImageFormatProperties2 does not support image {desc:?}: {result:?}"
            ),
            Self::UnsupportedLimits { desc, limits } => write!(
                f,
                "image {desc:?} exceeds supported extent, mip, layer or sample limits: {limits:?}"
            ),
            Self::Vulkan {
                operation,
                desc,
                result,
            } => write!(f, "{operation} for image {desc:?} failed: {result:?}"),
        }
    }
}

impl Error for ImageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnsupportedFormat { result, .. } | Self::Vulkan { result, .. } => Some(result),
            _ => None,
        }
    }
}

/// Query precisely the native creation conditions, and create only after their
/// limits have been checked. The small boundaries also allow failure-path tests
/// without constructing fake VMA allocations or a replacement Vulkan driver.
fn create_resource<T>(
    desc: ImageDesc,
    query: impl FnOnce(
        &vk::PhysicalDeviceImageFormatInfo2<'_>,
    ) -> Result<vk::ImageFormatProperties, vk::Result>,
    create: impl FnOnce(
        &vk::ImageCreateInfo<'_>,
        &vk_mem::AllocationCreateInfo,
    ) -> Result<T, vk::Result>,
) -> Result<T, ImageError> {
    desc.validate()?;
    let image_info = desc.create_info();
    let format_info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(image_info.format)
        .ty(image_info.image_type)
        .tiling(image_info.tiling)
        .usage(image_info.usage)
        .flags(image_info.flags);
    let limits = query(&format_info).map_err(|result| {
        if result == vk::Result::ERROR_FORMAT_NOT_SUPPORTED {
            ImageError::UnsupportedFormat { desc, result }
        } else {
            ImageError::Vulkan {
                operation: "vkGetPhysicalDeviceImageFormatProperties2",
                desc,
                result,
            }
        }
    })?;
    if image_info.extent.width > limits.max_extent.width
        || image_info.extent.height > limits.max_extent.height
        || image_info.extent.depth > limits.max_extent.depth
        || image_info.mip_levels > limits.max_mip_levels
        || image_info.array_layers > limits.max_array_layers
        || !limits.sample_counts.contains(image_info.samples)
    {
        return Err(ImageError::UnsupportedLimits { desc, limits });
    }
    // Optimal tiling's native size cannot be inferred from packed pixel bytes.
    // vkCreateImage reports OOM if its actual size exceeds maxResourceSize.
    let allocation_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    create(&image_info, &allocation_info).map_err(|result| ImageError::Vulkan {
        operation: "vmaCreateImage",
        desc,
        result,
    })
}

/// Owns one image/allocation pair, retaining VMA and its Vulkan dependencies.
/// This private owner does not expose a raw handle or permit GPU use yet.
pub(super) struct VulkanImage {
    owner: Arc<MemoryOwner>,
    image: vk::Image,
    allocation: vk_mem::Allocation,
    desc: ImageDesc,
}

impl VulkanAllocator {
    pub(super) fn create_image(&self, desc: ImageDesc) -> Result<VulkanImage, ImageError> {
        let result = (|| {
            let device = &self.owner.device;
            device
                .ensure_active()
                .map_err(|state| ImageError::DeviceUnavailable { desc, state })?;
            let (image, allocation) = create_resource(
                desc,
                |format_info| {
                    let mut properties = vk::ImageFormatProperties2::default();
                    // SAFETY: The live allocator retains this Device and its
                    // Instance/physical device. Both input and output structures
                    // are initialized and borrowed for the duration of the call.
                    unsafe {
                        device
                            .instance_raw()
                            .get_physical_device_image_format_properties2(
                                device.physical_device(),
                                format_info,
                                &mut properties,
                            )
                    }?;
                    let limits = properties.image_format_properties;
                    tracing::info!(target: LOG_TARGET, device = device.name(), image = ?desc,
                        limits = ?limits, "Vulkan image capabilities checked");
                    Ok(limits)
                },
                |image_info, allocation_info| {
                    // SAFETY: Validated, supported creation parameters produce
                    // a unique unaliased image/allocation pair. VMA and all its
                    // Vulkan dependencies are alive through the allocator owner.
                    unsafe { self.owner.raw.create_image(image_info, allocation_info) }
                },
            )?;
            // Take ownership immediately, before information queries, naming or
            // logging, so failures and unwinding destroy the successful pair.
            let owner = VulkanImage {
                owner: Arc::clone(&self.owner),
                image,
                allocation,
                desc,
            };
            let info = owner.owner.raw.get_allocation_info(&owner.allocation);
            // SAFETY: VMA keeps the immutable memory-properties table alive for
            // the allocator lifetime retained by this image's Arc.
            let properties = unsafe { owner.owner.raw.get_memory_properties() };
            let memory_properties =
                properties.memory_types[info.memory_type as usize].property_flags;
            // SAFETY: The image belongs to this live Device, debug_utils is
            // enabled, and the static name is valid for the complete call.
            unsafe {
                device.debug_utils().set_debug_utils_object_name(
                    &vk::DebugUtilsObjectNameInfoEXT::default()
                        .object_handle(image)
                        .object_name(c"renderer.transfer-image"),
                )
            }
            .map_err(|result| ImageError::Vulkan {
                operation: "vkSetDebugUtilsObjectNameEXT",
                desc,
                result,
            })?;
            tracing::info!(target: LOG_TARGET, device = device.name(), image = ?owner.image,
                description = ?owner.desc, allocation_size = info.size,
                memory_type = info.memory_type, memory_properties = ?memory_properties,
                initial_layout = ?vk::ImageLayout::UNDEFINED, contents_initialized = false,
                "Vulkan image allocated");
            Ok(owner)
        })();
        if let Err(ImageError::Vulkan { result, .. }) = &result {
            self.owner.device.observe_result(*result);
        }
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, device = self.owner.device.name(),
                image = ?desc, error = %error, "Vulkan image creation failed");
        }
        result
    }
}

impl Drop for VulkanImage {
    fn drop(&mut self) {
        // SAFETY: This owner uniquely owns the live image/allocation pair. No
        // raw image/GPU-use API exists, so it cannot have pending commands or
        // dependent ImageViews. The Arc keeps VMA and Device alive through
        // destruction. Future GPU-use APIs must add completion-based retention.
        unsafe {
            self.owner
                .raw
                .destroy_image(self.image, &mut self.allocation)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::super::device::VulkanDevice;
    use super::super::diagnostics::Diagnostics;
    use super::super::instance::VulkanInstance;
    use super::*;

    fn limits() -> vk::ImageFormatProperties {
        vk::ImageFormatProperties {
            max_extent: vk::Extent3D {
                width: 257,
                height: 13,
                depth: 1,
            },
            max_mip_levels: 1,
            max_array_layers: 1,
            sample_counts: vk::SampleCountFlags::TYPE_1,
            max_resource_size: 1 << 31,
        }
    }

    #[test]
    fn invalid_descriptions_never_query_or_allocate() {
        let mut descriptions = vec![ImageDesc::transfer(0, 1), ImageDesc::transfer(1, 0)];
        let mut wrong_format = ImageDesc::transfer(17, 13);
        wrong_format.format = vk::Format::R8_UNORM;
        descriptions.push(wrong_format);
        let mut wrong_usage = ImageDesc::transfer(17, 13);
        wrong_usage.usage |= vk::ImageUsageFlags::SAMPLED;
        descriptions.push(wrong_usage);
        for desc in descriptions {
            let result: Result<(), _> = create_resource(
                desc,
                |_| panic!("invalid description must not reach Vulkan"),
                |_, _| panic!("invalid description must not reach VMA"),
            );
            assert!(matches!(result, Err(ImageError::InvalidDescription { .. })));
        }
    }

    #[test]
    fn device_limits_gate_the_matching_transfer_image_request() {
        let desc = ImageDesc::transfer(257, 13);
        create_resource(
            desc,
            |request| {
                assert_eq!(request.format, desc.format);
                assert_eq!(request.ty, vk::ImageType::TYPE_2D);
                assert_eq!(request.tiling, vk::ImageTiling::OPTIMAL);
                assert_eq!(request.usage, desc.usage);
                assert!(request.flags.is_empty());
                Ok(limits())
            },
            |request, allocation| {
                assert_eq!(
                    request.extent,
                    vk::Extent3D {
                        width: 257,
                        height: 13,
                        depth: 1
                    }
                );
                assert_eq!(request.format, desc.format);
                assert_eq!(request.usage, desc.usage);
                assert_eq!(request.image_type, vk::ImageType::TYPE_2D);
                assert_eq!(request.tiling, vk::ImageTiling::OPTIMAL);
                assert_eq!(request.mip_levels, 1);
                assert_eq!(request.array_layers, 1);
                assert_eq!(request.samples, vk::SampleCountFlags::TYPE_1);
                assert_eq!(request.sharing_mode, vk::SharingMode::EXCLUSIVE);
                assert_eq!(request.initial_layout, vk::ImageLayout::UNDEFINED);
                assert_eq!(allocation.usage, vk_mem::MemoryUsage::AutoPreferDevice);
                assert!(allocation.flags.is_empty());
                Ok(())
            },
        )
        .unwrap();

        for changed in 0..6 {
            let mut unsupported = limits();
            match changed {
                0 => unsupported.max_extent.width = 256,
                1 => unsupported.max_extent.height = 12,
                2 => unsupported.max_extent.depth = 0,
                3 => unsupported.max_mip_levels = 0,
                4 => unsupported.max_array_layers = 0,
                _ => unsupported.sample_counts = vk::SampleCountFlags::TYPE_4,
            }
            let result: Result<(), _> = create_resource(
                desc,
                |_| Ok(unsupported),
                |_, _| panic!("unsupported limits must not reach VMA"),
            );
            assert!(matches!(result, Err(ImageError::UnsupportedLimits { .. })));
        }
    }

    #[test]
    fn query_and_creation_failures_preserve_the_native_cause_and_context() {
        let desc = ImageDesc::transfer(17, 13);
        for result in [
            vk::Result::ERROR_FORMAT_NOT_SUPPORTED,
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        ] {
            let error = create_resource::<()>(
                desc,
                |_| Err(result),
                |_, _| panic!("failed query must not reach VMA"),
            )
            .unwrap_err();
            assert_eq!(
                error.source().unwrap().downcast_ref::<vk::Result>(),
                Some(&result)
            );
            assert_eq!(
                matches!(error, ImageError::UnsupportedFormat { .. }),
                result == vk::Result::ERROR_FORMAT_NOT_SUPPORTED
            );
            let message = error.to_string();
            assert!(message.contains("vkGetPhysicalDeviceImageFormatProperties2"));
            assert!(
                message.contains("17") && message.contains("13"),
                "{message}"
            );
        }
        let error = create_resource::<()>(
            desc,
            |_| Ok(limits()),
            |_, _| Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY),
        )
        .unwrap_err();
        assert_eq!(
            error.source().unwrap().downcast_ref::<vk::Result>(),
            Some(&vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
        );
        let message = error.to_string();
        assert!(message.contains("vmaCreateImage"));
        assert!(
            message.contains("17") && message.contains("13"),
            "{message}"
        );
    }

    #[test]
    #[ignore = "explicit real-GPU VMA image ownership test; requires validation and debug_utils"]
    fn vulkan_image_smoke() {
        let dispatch = super::super::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            for iteration in 0..4 {
                let diagnostics = Diagnostics::new();
                let instance = VulkanInstance::new(Arc::clone(&diagnostics))
                    .unwrap_or_else(|error| panic!("iteration {iteration}: {error}"));
                super::super::assert_real_validation(&diagnostics);
                let device = VulkanDevice::new(instance)
                    .unwrap_or_else(|error| panic!("iteration {iteration}: {error}"));
                assert_ne!(
                    device.device_type(),
                    vk::PhysicalDeviceType::CPU,
                    "CPU fallback does not count as real-GPU acceptance"
                );
                let allocator = VulkanAllocator::new(device).unwrap();
                let lifetime = Arc::downgrade(&allocator.owner);
                let first = allocator.create_image(ImageDesc::transfer(1, 1)).unwrap();
                let odd = allocator.create_image(ImageDesc::transfer(17, 13)).unwrap();
                let wide = allocator.create_image(ImageDesc::transfer(257, 3)).unwrap();
                assert_eq!(odd.desc, ImageDesc::transfer(17, 13));
                assert!(matches!(
                    allocator.create_image(ImageDesc::transfer(0, 13)),
                    Err(ImageError::InvalidDescription { .. })
                ));
                allocator.owner.device.stop();
                assert!(matches!(
                    allocator.create_image(ImageDesc::transfer(1, 1)),
                    Err(ImageError::DeviceUnavailable {
                        state: DeviceState::Stopped,
                        ..
                    })
                ));
                // The factory may disappear while images retain VMA and Device.
                // A stopped Device still permits destruction of unused children.
                drop(allocator);
                assert!(lifetime.upgrade().is_some());
                drop(first);
                drop(odd);
                assert!(lifetime.upgrade().is_some());
                drop(wide);
                assert!(lifetime.upgrade().is_none());
                let report = diagnostics.snapshot();
                assert!(
                    report.is_clean(),
                    "iteration {iteration}, including shutdown: {report:#?}"
                );
                tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                    errors = report.errors, callback_failures = report.callback_failures,
                    "Vulkan image ownership smoke including shutdown passed");
            }
        });
    }
}
