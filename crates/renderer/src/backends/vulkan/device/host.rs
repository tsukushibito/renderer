//! Adoption of a live host device without taking destruction authority.

use super::*;
use crate::backends::vulkan::{diagnostics::DiagnosticsHandle, error::RuntimeError};

/// Values enabled at host instance and device creation. Vulkan can subsequently
/// report hardware support, but cannot query which features the host enabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct VulkanHostFeatures {
    /// API version actually requested in the host's VkApplicationInfo.
    pub instance_api_version: u32,
    pub timeline_semaphore: bool,
    pub synchronization2: bool,
    pub dynamic_rendering: bool,
    pub multi_draw_indirect: bool,
    /// VK_EXT_debug_utils was enabled on the instance. Diagnostic builds use
    /// its object names and command-buffer labels.
    pub debug_utils: bool,
}

/// Dispatch tables and lifetime for a host-created Vulkan graphics/compute queue.
/// The host retains destruction authority over its device and instance.
pub struct VulkanHostDevice {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub physical_device: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub enabled: VulkanHostFeatures,
    /// Device-wide sampler slots exclusively reserved for this runtime.
    pub sampler_budget: u32,
    /// Shared exclusion for host operations on this queue, including waits.
    pub queue_lock: Arc<Mutex<()>>,
    /// Owns the native device, instance, loader and their callbacks. Renderer
    /// retains this through the final native resource and GPU submission.
    pub lifetime: Arc<dyn Send + Sync>,
}

impl VulkanDevice {
    /// # Safety
    /// The caller upholds the handle, lifetime, queue and allocation contract
    /// of `VulkanRuntime::from_host_device`.
    pub(in crate::backends::vulkan) unsafe fn from_host(
        host: VulkanHostDevice,
        diagnostics: DiagnosticsHandle,
    ) -> Result<Self, RuntimeError> {
        let enabled = host.enabled;
        if !supported_api(enabled.instance_api_version)
            || !enabled.timeline_semaphore
            || !enabled.synchronization2
            || !enabled.dynamic_rendering
        {
            return Err(RuntimeError::unsupported(
                "host Vulkan instance/device must enable Vulkan 1.3, timelineSemaphore, synchronization2 and dynamicRendering",
            ));
        }
        if cfg!(feature = "vulkan-diagnostics") && !enabled.debug_utils {
            return Err(RuntimeError::unsupported(
                "Vulkan diagnostic builds require host VK_EXT_debug_utils",
            ));
        }
        if host.queue == vk::Queue::null() || host.physical_device == vk::PhysicalDevice::null() {
            return Err(RuntimeError::invalid(
                "host queue and physical device must be non-null",
            ));
        }
        let instance = VulkanInstance::from_host(
            host.entry,
            host.instance,
            enabled.instance_api_version,
            host.lifetime,
            diagnostics,
        );
        let candidate = inspect(instance.raw(), host.physical_device);
        let mut selected = candidate
            .assess_queue(Some(host.queue_family))
            .map_err(|rejection| DeviceInitError::NoSuitableDevice(vec![rejection]))?;
        if enabled.multi_draw_indirect && !selected.multi_draw_indirect {
            return Err(RuntimeError::unsupported(
                "host multiDrawIndirect is unsupported by the physical device",
            ));
        }
        selected.multi_draw_indirect = enabled.multi_draw_indirect;
        let (limits, max_buffer_size) = Self::query_limits(&instance, &selected);
        if host.sampler_budget > limits.max_sampler_allocation_count {
            return Err(RuntimeError::invalid(
                "host sampler budget exceeds the physical device limit",
            ));
        }
        Ok(Self {
            #[cfg(feature = "vulkan-diagnostics")]
            debug_utils: Some(ash::ext::debug_utils::Device::new(
                instance.raw(),
                &host.device,
            )),
            _instance: Arc::new(instance),
            device: host.device,
            selected,
            limits,
            max_buffer_size,
            // Optional extensions are never inferred from support on an adopted device.
            memory_budget_enabled: false,
            owns_device: false,
            sampler_limit: host.sampler_budget,
            queue: host.queue,
            queue_lock: host.queue_lock,
            state: AtomicU8::new(DeviceState::Active as u8),
            samplers: SamplerCount::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::vulkan::{diagnostics::Diagnostics, instance::ValidationMode};
    use crate::capacity::{ResourceClass, WorkingSet};
    use crate::graph::{BufferDesc, GraphCapacity, GraphPool};
    use crate::runtime::{ExecutionErrorKind, Runtime, RuntimeLimits, VulkanConfig};
    use std::time::Duration;

    #[test]
    #[ignore = "requires a hardware Vulkan 1.3 device; diagnostics builds require Core/SyncVal"]
    fn host_device_survives_adopted_graphs_and_failed_adoption() {
        let validation = if cfg!(feature = "vulkan-diagnostics") {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        };
        let diagnostics = Diagnostics::new();
        let instance = VulkanInstance::with_validation(diagnostics.clone(), validation).unwrap();
        let original = Arc::new(VulkanDevice::new(instance).unwrap());
        assert_ne!(original.device_type(), vk::PhysicalDeviceType::CPU);
        let host = || VulkanHostDevice {
            entry: original._instance.entry().clone(),
            instance: original.instance_raw().clone(),
            physical_device: original.physical_device(),
            device: original.raw().clone(),
            queue: original.queue(),
            queue_family: original.queue_family(),
            queue_lock: Arc::clone(&original.queue_lock),
            lifetime: original.clone(),
            sampler_budget: 8,
            enabled: VulkanHostFeatures {
                instance_api_version: REQUIRED_API,
                timeline_semaphore: true,
                synchronization2: true,
                dynamic_rendering: true,
                multi_draw_indirect: original.selected.multi_draw_indirect,
                debug_utils: cfg!(feature = "vulkan-diagnostics"),
            },
        };
        let desc = BufferDesc { size: 4 };
        let work = WorkingSet {
            resources: &[ResourceClass::Buffer { desc, count: 1 }],
            ..WorkingSet::default()
        };
        let settings = VulkanConfig {
            validation: ValidationMode::Disabled,
            ..Default::default()
        };
        let limits = RuntimeLimits {
            frames: 1,
            results: 1,
            ..Default::default()
        };
        // A conservative declaration of disabled required features is rejected
        // before dispatch; capability queries cannot substitute for enablement.
        let mut unavailable = host();
        unavailable.enabled.timeline_semaphore = false;
        // SAFETY: Original retains the live handles and exclusively owns all
        // destruction. Queue and allocation capacity are shared as documented.
        assert_eq!(
            unsafe { Runtime::from_vulkan_device(unavailable, settings, limits, work) }
                .err()
                .unwrap()
                .kind(),
            ExecutionErrorKind::Unsupported
        );
        // SAFETY: Same live device and queue, actual creation settings, and
        // original owns all native handles and observes any validation output.
        let mut runtime =
            unsafe { Runtime::from_vulkan_device(host(), settings, limits, work) }.unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let buffer = graph.buffer("buffer on host device", desc).unwrap();
        graph
            .upload_buffer("host bytes", buffer, 0, &[1, 2, 3, 4])
            .unwrap();
        let read = graph
            .readback_buffer("read host bytes", buffer, 0, 4)
            .unwrap();
        let submission = runtime.submit(&graph.compile().unwrap()).unwrap();
        let mut completion = runtime
            .wait_for_frame(submission.id(), Duration::from_secs(15))
            .unwrap()
            .unwrap();
        let mut bytes = [0; 4];
        completion.readback_into(read, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4]);
        drop((completion, submission, runtime));
        assert_eq!(Arc::strong_count(&original), 1);
        // Exercise the host queue after all Renderer children have been destroyed.
        let guard = original.lock_queue().unwrap();
        // SAFETY: We hold the original queue gate and the device remains live.
        unsafe { original.raw().queue_wait_idle(original.queue()).unwrap() };
        drop(guard);
        drop(original);
        assert!(
            diagnostics.snapshot().is_clean(),
            "{:?}",
            diagnostics.snapshot()
        );
        #[cfg(feature = "vulkan-diagnostics")]
        crate::backends::vulkan::assert_real_validation(&diagnostics);
    }
}
