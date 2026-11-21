//! Exclusive, same-device exchange of host textures at a change boundary.

use super::*;
use crate::graph::ImageDesc;

/// Keeps the Runtime's Vulkan Device, Instance and loader alive for a host
/// texture. Keep this owner in resources which invoke Vulkan during Drop.
#[derive(Clone)]
pub struct VulkanExternalDevice(Arc<MemoryOwner>);

impl VulkanExternalDevice {
    pub fn device(&self) -> &ash::Device {
        self.0.device.raw()
    }
    pub fn instance(&self) -> &ash::Instance {
        self.0.device.instance_raw()
    }
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.0.device.physical_device()
    }
    pub fn queue_family(&self) -> u32 {
        self.0.device.queue_family()
    }

    /// Perform host operations on the shared queue under the Device's lock.
    /// Do not recursively enter a Renderer queue operation inside the callback.
    pub fn with_queue<T>(&self, work: impl FnOnce(vk::Queue) -> T) -> Result<T, RuntimeError> {
        let _guard = self
            .0
            .device
            .lock_queue()
            .map_err(RuntimeError::device_unavailable)?;
        Ok(work(self.0.device.queue()))
    }

    /// Native creation parameters and required usages for a host image.
    pub fn image_create_info(
        &self,
        desc: ImageDesc,
    ) -> Result<vk::ImageCreateInfo<'static>, RuntimeError> {
        if desc.array_layers != 1 {
            return Err(RuntimeError::unsupported(
                "external images require a single layer",
            ));
        }
        Ok(DeviceImage::prepare(&self.0, desc)?.info())
    }
}

/// Layout and full-image content validity at an exclusive handoff. On return,
/// all Renderer uses have retired. Use ALL_COMMANDS and MEMORY_READ |
/// MEMORY_WRITE as the source scope for the next barrier on the shared queue.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct VulkanImageState {
    pub layout: vk::ImageLayout,
    pub initialized: bool,
}

impl VulkanImageState {
    /// Discard previous contents on registration, including for a fresh allocation.
    pub const DISCARD: Self = Self {
        layout: vk::ImageLayout::UNDEFINED,
        initialized: false,
    };
}

impl VulkanRuntime {
    pub fn external_device(&self) -> VulkanExternalDevice {
        VulkanExternalDevice(Arc::clone(&self.owner))
    }

    /// Register a host image as ordinary Graph storage. The runtime owns
    /// its views and retains `lifetime`; the host still owns image and memory.
    /// Handoff does not wait or submit GPU commands.
    ///
    /// # Safety
    /// `image` must be a live, bound, unaliased image on `device`, compatible
    /// with every field and usage returned by `device.image_create_info(desc)`.
    /// The caller must ensure it is not registered elsewhere, all producer
    /// work has completed, and exclusive queue family ownership and memory
    /// visibility have been transferred to the Runtime queue. All subresources
    /// must match the stated layout or allow a discard; every texel must be
    /// defined if `state.initialized` is true.
    /// `lifetime` must keep the image and its memory live until dropped. No host
    /// or GPU access, layout transition, rebind or alias use is allowed until
    /// release_external_image returns Some. A failed registration transfers no access.
    pub unsafe fn register_external_image(
        &mut self,
        device: &VulkanExternalDevice,
        image: vk::Image,
        desc: ImageDesc,
        state: VulkanImageState,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<ImportImage, RuntimeError> {
        if !Arc::ptr_eq(&self.owner, &device.0) {
            return Err(RuntimeError::wrong_owner(
                "external image Device differs from Runtime",
            ));
        }
        if desc.array_layers != 1 || image == vk::Image::null() {
            return Err(RuntimeError::invalid(
                "expected a live single-layer host image",
            ));
        }
        if state.layout == vk::ImageLayout::PREINITIALIZED
            || (state.layout == vk::ImageLayout::UNDEFINED && state.initialized)
        {
            return Err(RuntimeError::invalid(
                "initial contents and layout must describe an optimal-tiling image",
            ));
        }
        if self
            .entries
            .iter()
            .filter_map(|e| e.physical.as_ref())
            .any(|p| matches!(&p.native, NativeResource::Image(i) if i.image == image))
        {
            return Err(RuntimeError::invalid(
                "image is already known to this Runtime",
            ));
        }
        let index = self.vacant.first().copied().unwrap_or(self.entries.len());
        if index >= self.config.resource_capacity {
            return Err(Self::physical_capacity_exhausted());
        }
        let capacity = self.frame_capacity.ok_or_else(|| {
            RuntimeError::invalid("prepare the Graph working set before registering an image")
        })?;
        let mut scheduled = Snapshot::for_working_set(capacity)?;
        scheduled.layout = state.layout;
        scheduled.stage = vk::PipelineStageFlags2::ALL_COMMANDS;
        scheduled.access = vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE;
        scheduled.writer_stage = vk::PipelineStageFlags2::ALL_COMMANDS;
        scheduled.writer_access = vk::AccessFlags2::MEMORY_WRITE;
        if state.initialized {
            scheduled.set_initialized(std::slice::from_ref(&(0..u64::from(desc.mip_levels))))?;
        }
        // SAFETY: The caller guarantees compatible bound storage and transfers
        // exclusive access for the lifetime of the retained allocation owner.
        let native =
            unsafe { DeviceImage::from_host(Arc::clone(&self.owner), desc, image, lifetime) }?;
        let key = ResourceKey {
            owner: self.id,
            index: index as u32,
            generation: self
                .entries
                .get(index)
                .map_or(1, |entry| entry.generation + 1),
        };
        let imported = ImportImage::new(key, scheduled.revision, desc, &scheduled.initialized)
            .map_err(|e| RuntimeError::invalid(e.to_string()))?;
        let physical = Arc::new(Physical {
            owner: Arc::clone(&self.owner),
            desc: ResourceDesc::Image(desc),
            native: NativeResource::Image(native),
            state: Mutex::new(State {
                scheduled,
                completed_serial: 0,
            }),
        });
        self.install_physical(index, physical);
        self.register(index);
        Ok(imported)
    }

    /// Return the image after all recording, GPU and presentation leases have
    /// retired. None requests another poll; the registration is still usable.
    /// Success invalidates all existing Graph snapshots of this registration.
    pub fn release_external_image(
        &mut self,
        key: ResourceKey,
    ) -> Result<Option<VulkanImageState>, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let physical = self.entry(key)?.physical();
        if !matches!(&physical.native, NativeResource::Image(image) if image.is_external()) {
            return Err(RuntimeError::invalid(
                "expected an external image registration",
            ));
        }
        if Arc::strong_count(physical) != 1 {
            return Ok(None);
        }
        let layout = physical.lock().scheduled.layout;
        let result = VulkanImageState {
            layout,
            initialized: self.refresh_image(key)?.initialized(),
        };
        let index = key.index as usize;
        self.entries[index].registered = false;
        self.release_idle_physical(index);
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Runtime, RuntimeConfig, RuntimeLimits, native_image_tests};

    struct HostTexture {
        device: VulkanExternalDevice,
        image: vk::Image,
        memory: vk::DeviceMemory,
    }

    impl HostTexture {
        fn new(device: VulkanExternalDevice, desc: ImageDesc) -> Self {
            let info = device.image_create_info(desc).unwrap();
            // SAFETY: Creation info was checked for precisely this Device.
            let image = unsafe { device.device().create_image(&info, None) }.unwrap();
            let mut host = Self {
                device,
                image,
                memory: vk::DeviceMemory::null(),
            };
            let dev = &host.device;
            // SAFETY: The host owns an unbound image on this live Device.
            let req = unsafe { dev.device().get_image_memory_requirements(image) };
            // SAFETY: The physical adapter belongs to the retained Instance.
            let props = unsafe {
                dev.instance()
                    .get_physical_device_memory_properties(dev.physical_device())
            };
            let memory_type = (0..props.memory_type_count)
                .find(|&i| req.memory_type_bits & (1 << i) != 0)
                .unwrap();
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let allocation = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(memory_type)
                .push_next(&mut dedicated);
            // SAFETY: Size/type follow requirements on the same Device; dedicated memory is exclusive.
            host.memory = unsafe { dev.device().allocate_memory(&allocation, None) }.unwrap();
            // SAFETY: Offset zero satisfies alignment and the allocation covers the entire image.
            unsafe { dev.device().bind_image_memory(image, host.memory, 0) }.unwrap();
            host
        }
    }

    impl Drop for HostTexture {
        fn drop(&mut self) {
            // SAFETY: The last Arc returns after all borrowed views and GPU use retire.
            // Null memory is valid on a partially constructed allocation.
            unsafe {
                self.device.device().destroy_image(self.image, None);
                self.device.device().free_memory(self.memory, None);
            }
        }
    }

    #[test]
    #[ignore = "requires a hardware Vulkan adapter and the configured validation layers"]
    fn vulkan_host_texture_roundtrip() {
        let desc = ImageDesc::rgba8(2, 1);
        let mut runtime = Runtime::new(
            RuntimeConfig::Vulkan(Default::default()),
            RuntimeLimits::default(),
            WorkingSet::default(),
        )
        .unwrap();
        let vk = runtime.as_vulkan_mut().unwrap();
        assert_ne!(vk.owner.device.device_type(), vk::PhysicalDeviceType::CPU);
        let monitor = vk.validation_monitor();
        let device = vk.external_device();
        let texture = Arc::new(HostTexture::new(device.clone(), desc));
        let mut state = VulkanImageState::DISCARD;
        for write in [true, false] {
            // SAFETY: Fresh bound storage or the same retired registration with its
            // returned layout. Its allocation stays pinned and the host never accesses it.
            let image = unsafe {
                runtime.as_vulkan_mut().unwrap().register_external_image(
                    &device,
                    texture.image,
                    desc,
                    state,
                    texture.clone(),
                )
            }
            .unwrap();
            let key = image.key();
            assert!(runtime.delete(key).is_err());
            native_image_tests::transfer(&mut runtime, &image, write, |rt| {
                assert!(
                    rt.as_vulkan_mut()
                        .unwrap()
                        .release_external_image(key)
                        .unwrap()
                        .is_none()
                );
            });
            let returned = runtime
                .as_vulkan_mut()
                .unwrap()
                .release_external_image(key)
                .unwrap()
                .unwrap();
            assert!(returned.initialized);
            assert_eq!(returned.layout, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
            state = returned;
            assert!(runtime.refresh_image(key).is_err());
            assert_eq!(Arc::strong_count(&texture), 1);
        }
        drop((texture, device, runtime));
        let report = monitor.report();
        assert!(report.is_clean(), "{report:#?}");
        if cfg!(feature = "development-validation") {
            assert_eq!(report.core_validation, Some(true));
            assert_eq!(report.synchronization_validation, Some(true));
        }
    }
}
