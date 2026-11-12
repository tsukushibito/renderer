//! Single-queue Device bootstrap. Command submission is a later ownership step.

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};

use ash::vk;

use super::instance::{LOG_TARGET, REQUIRED_API, VulkanInstance, api_version};

#[derive(Clone, Debug)]
struct DriverInfo {
    name: String,
    info: String,
    id: vk::DriverId,
}

#[derive(Clone, Debug)]
struct AdapterInfo {
    name: String,
    device_type: vk::PhysicalDeviceType,
    api_version: u32,
    vendor_id: u32,
    device_id: u32,
    // Encoding is vendor-specific; do not interpret this as a Vulkan API version.
    driver_version: u32,
    driver: Option<DriverInfo>,
}

#[derive(Debug, PartialEq, Eq)]
enum RequirementFailure {
    ApiVariant(u32),
    ApiVersion { available: u32, required: u32 },
    Feature(&'static str),
    GraphicsComputeQueue,
    InvalidProperty(&'static str),
}

impl fmt::Display for RequirementFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiVariant(api) => write!(
                f,
                "unsupported Vulkan API variant {}",
                vk::api_version_variant(*api)
            ),
            Self::ApiVersion {
                available,
                required,
            } => write!(
                f,
                "Vulkan API {} is below required {}",
                api_version(*available),
                api_version(*required)
            ),
            Self::Feature(name) => write!(f, "missing feature {name}"),
            Self::GraphicsComputeQueue => write!(f, "no Graphics+Compute queue family"),
            Self::InvalidProperty(name) => write!(f, "invalid driver property {name}"),
        }
    }
}

#[derive(Debug)]
pub(super) struct AdapterRejection {
    info: Box<AdapterInfo>,
    failures: Vec<RequirementFailure>,
}

#[derive(Debug)]
pub(super) enum DeviceInitError {
    NoSuitableDevice(Vec<AdapterRejection>),
    Vulkan {
        operation: &'static str,
        device_name: Option<String>,
        result: vk::Result,
    },
}

impl fmt::Display for DeviceInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuitableDevice(rejections) if rejections.is_empty() => {
                write!(f, "Vulkan reported no physical devices")
            }
            Self::NoSuitableDevice(rejections) => {
                write!(f, "no Vulkan physical device satisfies the requirements")?;
                for rejection in rejections {
                    write!(f, "; {}: ", rejection.info.name)?;
                    for (index, failure) in rejection.failures.iter().enumerate() {
                        if index != 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{failure}")?;
                    }
                }
                Ok(())
            }
            Self::Vulkan {
                operation,
                device_name,
                result,
            } => {
                write!(f, "{operation}")?;
                if let Some(name) = device_name {
                    write!(f, " on {name}")?;
                }
                write!(f, " failed: {result:?}")
            }
        }
    }
}

impl Error for DeviceInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Vulkan { result, .. } => Some(result),
            Self::NoSuitableDevice(_) => None,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct RequiredFeatures {
    timeline_semaphore: bool,
    synchronization2: bool,
    dynamic_rendering: bool,
}

struct Candidate {
    physical_device: vk::PhysicalDevice,
    info: AdapterInfo,
    features: RequiredFeatures,
    queues: Vec<vk::QueueFamilyProperties>,
    property_failures: Vec<RequirementFailure>,
}

struct SelectedDevice {
    physical_device: vk::PhysicalDevice,
    info: AdapterInfo,
    queue_family_index: u32,
}

fn supported_api(api: u32) -> bool {
    vk::api_version_variant(api) == 0 && api >= REQUIRED_API
}

impl Candidate {
    fn assess(self) -> Result<SelectedDevice, AdapterRejection> {
        let mut failures = self.property_failures;
        let api = self.info.api_version;
        if vk::api_version_variant(api) != 0 {
            failures.push(RequirementFailure::ApiVariant(api));
        } else if api < REQUIRED_API {
            failures.push(RequirementFailure::ApiVersion {
                available: api,
                required: REQUIRED_API,
            });
        } else {
            // Vulkan 1.3 feature structures are queried only on the supported
            // API path. Old-API devices must not gain invented feature failures.
            for (supported, name) in [
                (self.features.timeline_semaphore, "timelineSemaphore"),
                (self.features.synchronization2, "synchronization2"),
                (self.features.dynamic_rendering, "dynamicRendering"),
            ] {
                if !supported {
                    failures.push(RequirementFailure::Feature(name));
                }
            }
        }
        let required = vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE;
        let queue_family_index = self
            .queues
            .iter()
            .position(|queue| queue.queue_count != 0 && queue.queue_flags.contains(required));
        if queue_family_index.is_none() {
            failures.push(RequirementFailure::GraphicsComputeQueue);
        }
        if failures.is_empty() {
            Ok(SelectedDevice {
                physical_device: self.physical_device,
                info: self.info,
                // Vulkan family indices are uint32_t; the queried list is
                // bounded by the same uint32_t count.
                queue_family_index: queue_family_index.expect("checked queue family") as u32,
            })
        } else {
            Err(AdapterRejection {
                info: Box::new(self.info),
                failures,
            })
        }
    }
}

fn preference(device_type: vk::PhysicalDeviceType) -> u8 {
    match device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => 0,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
        vk::PhysicalDeviceType::CPU => 4,
        _ => 3,
    }
}

fn select_device(candidates: Vec<Candidate>) -> Result<SelectedDevice, DeviceInitError> {
    let mut selected: Option<SelectedDevice> = None;
    let mut rejections = Vec::new();
    for candidate in candidates {
        match candidate.assess() {
            Ok(candidate) => {
                // A strict improvement preserves enumeration order on ties.
                if selected.as_ref().is_none_or(|current| {
                    preference(candidate.info.device_type) < preference(current.info.device_type)
                }) {
                    selected = Some(candidate);
                }
            }
            Err(rejection) => {
                tracing::info!(target: LOG_TARGET, device = %rejection.info.name,
                    reasons = ?rejection.failures, "Excluded Vulkan physical device");
                rejections.push(rejection);
            }
        }
    }
    selected.ok_or(DeviceInitError::NoSuitableDevice(rejections))
}

fn property_name(
    value: Result<&std::ffi::CStr, std::ffi::FromBytesUntilNulError>,
    field: &'static str,
    failures: &mut Vec<RequirementFailure>,
) -> String {
    match value {
        Ok(value) => value.to_string_lossy().into_owned(),
        Err(_) => {
            failures.push(RequirementFailure::InvalidProperty(field));
            format!("<invalid {field}>")
        }
    }
}

fn inspect(instance: &ash::Instance, physical_device: vk::PhysicalDevice) -> Candidate {
    // SAFETY: physical_device was enumerated from this live, uniquely owned
    // Instance. ash owns the output storage; no retained pointers escape.
    let properties = unsafe { instance.get_physical_device_properties(physical_device) };
    let mut property_failures = Vec::new();
    let mut info = AdapterInfo {
        name: property_name(
            properties.device_name_as_c_str(),
            "deviceName",
            &mut property_failures,
        ),
        device_type: properties.device_type,
        api_version: properties.api_version,
        vendor_id: properties.vendor_id,
        device_id: properties.device_id,
        driver_version: properties.driver_version,
        driver: None,
    };
    let mut features = RequiredFeatures::default();
    if supported_api(properties.api_version) {
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        let mut properties2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
        // SAFETY: Vulkan 1.3 device and Instance support the core properties2
        // entry point and driver-properties structure. The pNext storage is live.
        unsafe { instance.get_physical_device_properties2(physical_device, &mut properties2) };
        info.driver = Some(DriverInfo {
            name: property_name(
                driver.driver_name_as_c_str(),
                "driverName",
                &mut property_failures,
            ),
            info: property_name(
                driver.driver_info_as_c_str(),
                "driverInfo",
                &mut property_failures,
            ),
            id: driver.driver_id,
        });

        let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut features2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut features12)
            .push_next(&mut features13);
        // SAFETY: Same live Vulkan 1.3 device. Both core feature structures are
        // supported query inputs and remain live for this synchronous call.
        unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };
        features = RequiredFeatures {
            timeline_semaphore: features12.timeline_semaphore == vk::TRUE,
            synchronization2: features13.synchronization2 == vk::TRUE,
            dynamic_rendering: features13.dynamic_rendering == vk::TRUE,
        };
    }
    // SAFETY: Same enumerated device; ash handles the count/data output storage.
    let queues = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    tracing::info!(target: LOG_TARGET, device = %info.name, device_type = ?info.device_type,
        api = %api_version(info.api_version), api_variant = vk::api_version_variant(info.api_version),
        vendor_id = info.vendor_id, device_id = info.device_id, driver_version_raw = info.driver_version,
        driver_name = ?info.driver.as_ref().map(|driver| &driver.name),
        driver_info = ?info.driver.as_ref().map(|driver| &driver.info),
        driver_id = ?info.driver.as_ref().map(|driver| driver.id),
        "Inspected Vulkan physical device");
    Candidate {
        physical_device,
        info,
        features,
        queues,
        property_failures,
    }
}

/// Own the short-lived pNext and queue arrays through the creation call. Query
/// results are deliberately not reused as enabled-feature structures.
fn with_create_info<T>(
    queue_family_index: u32,
    create: impl FnOnce(&vk::DeviceCreateInfo<'_>) -> T,
) -> T {
    let priorities = [1.0];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&priorities)];
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true);
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
        .synchronization2(true)
        .dynamic_rendering(true);
    let create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues)
        .push_next(&mut features12)
        .push_next(&mut features13);
    create(&create_info)
}

/// Owns the Instance until the Device is destroyed. Allocator/buffer owners keep
/// this owner alive until all children and submitted work have been retired.
pub(super) struct VulkanDevice {
    _instance: VulkanInstance,
    device: ash::Device,
    debug_utils: Option<ash::ext::debug_utils::Device>,
    selected: SelectedDevice,
    queue: vk::Queue,
    queue_lock: Mutex<()>,
    state: AtomicU8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum DeviceState {
    Active,
    Stopped,
    Lost,
}

impl VulkanDevice {
    pub(super) fn new(instance: VulkanInstance) -> Result<Self, DeviceInitError> {
        let result = Self::initialize(instance);
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan Device initialization failed");
        }
        result
    }

    pub(super) fn instance_raw(&self) -> &ash::Instance {
        self._instance.raw()
    }

    pub(super) fn raw(&self) -> &ash::Device {
        &self.device
    }

    pub(super) fn physical_device(&self) -> vk::PhysicalDevice {
        self.selected.physical_device
    }

    pub(super) fn name(&self) -> &str {
        &self.selected.info.name
    }

    pub(super) fn device_type(&self) -> vk::PhysicalDeviceType {
        self.selected.info.device_type
    }

    pub(super) fn state(&self) -> DeviceState {
        match self.state.load(Ordering::Acquire) {
            0 => DeviceState::Active,
            2 => DeviceState::Lost,
            _ => DeviceState::Stopped,
        }
    }

    pub(super) fn ensure_active(&self) -> Result<(), DeviceState> {
        match self.state() {
            DeviceState::Active => Ok(()),
            state => Err(state),
        }
    }

    pub(super) fn stop(&self) {
        let _ = self.state.compare_exchange(
            DeviceState::Active as u8,
            DeviceState::Stopped as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(super) fn observe_result(&self, result: vk::Result) {
        if result == vk::Result::ERROR_DEVICE_LOST {
            self.state.store(DeviceState::Lost as u8, Ordering::Release);
        }
    }

    /// Every host operation on the one native Queue takes this Device-owned lock.
    pub(super) fn lock_queue(&self) -> Result<MutexGuard<'_, ()>, DeviceState> {
        let guard = self.queue_lock.lock().map_err(|_| {
            self.stop();
            self.state()
        })?;
        self.ensure_active()?;
        Ok(guard)
    }

    pub(super) fn queue(&self) -> vk::Queue {
        self.queue
    }

    pub(super) fn queue_family(&self) -> u32 {
        self.selected.queue_family_index
    }

    pub(super) fn debug_utils(&self) -> &ash::ext::debug_utils::Device {
        self.debug_utils
            .as_ref()
            .expect("Device dispatch initialized")
    }

    fn initialize(instance: VulkanInstance) -> Result<Self, DeviceInitError> {
        // SAFETY: The Instance is live and uniquely owned here. ash handles the
        // enumeration retry/count protocol; physical handles stay within it.
        let physical_devices =
            unsafe { instance.raw().enumerate_physical_devices() }.map_err(|result| {
                DeviceInitError::Vulkan {
                    operation: "vkEnumeratePhysicalDevices",
                    device_name: None,
                    result,
                }
            })?;
        let candidates = physical_devices
            .into_iter()
            .map(|physical_device| inspect(instance.raw(), physical_device))
            .collect();
        let selected = select_device(candidates)?;
        Self::create_selected(instance, selected)
    }

    fn create_selected(
        instance: VulkanInstance,
        selected: SelectedDevice,
    ) -> Result<Self, DeviceInitError> {
        tracing::info!(target: LOG_TARGET, device = %selected.info.name,
            device_type = ?selected.info.device_type, family = selected.queue_family_index,
            queue_index = 0, queue_count = 1,
            selection_rule = "discrete > integrated > virtual > other > CPU; enumeration order on ties",
            cpu_fallback = selected.info.device_type == vk::PhysicalDeviceType::CPU,
            "Selected Vulkan physical device");
        let device = with_create_info(selected.queue_family_index, |create_info| {
            // SAFETY: Selected features and queue family were queried from this
            // Instance/device. All create-info storage remains live for the call.
            // No extensions, children, custom allocator, or work are introduced.
            unsafe {
                instance
                    .raw()
                    .create_device(selected.physical_device, create_info, None)
            }
        })
        .map_err(|result| DeviceInitError::Vulkan {
            operation: "vkCreateDevice",
            device_name: Some(selected.info.name.clone()),
            result,
        })?;
        // Establish ownership before queue lookup or any subsequent logging can
        // unwind. A failed creation above owns no Device and drops the Instance.
        let mut owner = Self {
            _instance: instance,
            device,
            debug_utils: None,
            selected,
            queue: vk::Queue::null(),
            queue_lock: Mutex::new(()),
            state: AtomicU8::new(DeviceState::Active as u8),
        };
        // SAFETY: Exactly one queue was requested from this family with index 0.
        // It is owned by the Device and requires no separate destruction.
        owner.queue = unsafe {
            owner
                .device
                .get_device_queue(owner.selected.queue_family_index, 0)
        };
        owner.debug_utils = Some(ash::ext::debug_utils::Device::new(
            owner.instance_raw(),
            owner.raw(),
        ));
        // SAFETY: The exclusively owned Device/Queue are not yet exposed. The
        // enabled debug_utils table and static name are live for this call.
        unsafe {
            owner.debug_utils().set_debug_utils_object_name(
                &vk::DebugUtilsObjectNameInfoEXT::default()
                    .object_handle(owner.queue)
                    .object_name(c"renderer.graphics-compute.queue"),
            )
        }
        .map_err(|result| {
            owner.observe_result(result);
            DeviceInitError::Vulkan {
                operation: "vkSetDebugUtilsObjectNameEXT(queue)",
                device_name: Some(owner.name().to_owned()),
                result,
            }
        })?;
        tracing::info!(target: LOG_TARGET, device = %owner.selected.info.name,
            family = owner.selected.queue_family_index, queue_index = 0,
            timeline_semaphore = true, synchronization2 = true, dynamic_rendering = true,
            "Vulkan Device and single queue initialized");
        Ok(owner)
    }
}

impl Drop for VulkanDevice {
    fn drop(&mut self) {
        // SAFETY: Buffer and submission owners retain the containing MemoryOwner.
        // Submitted work is completed or retired after Device loss before those
        // owners are released. Unresolved work instead retains/leaks its owner.
        // MemoryOwner drops its Allocator before this Device; no children remain.
        // It exclusively owns the Device and Queue;
        // the Instance, loader, and diagnostic callback remain live throughout.
        unsafe { self.device.destroy_device(None) };
        // Rust drops the owned Instance only after this Drop body returns.
    }
}

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::super::diagnostics::Diagnostics;
    use super::*;
    use ash::vk::Handle;
    use std::sync::Arc;

    fn candidate(index: u64, device_type: vk::PhysicalDeviceType) -> Candidate {
        Candidate {
            physical_device: vk::PhysicalDevice::from_raw(index),
            info: AdapterInfo {
                name: format!("device-{index}"),
                device_type,
                api_version: REQUIRED_API,
                vendor_id: 0,
                device_id: index as u32,
                driver_version: 0,
                driver: None,
            },
            features: RequiredFeatures {
                timeline_semaphore: true,
                synchronization2: true,
                dynamic_rendering: true,
            },
            queues: vec![vk::QueueFamilyProperties {
                queue_flags: vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE,
                queue_count: 1,
                ..Default::default()
            }],
            property_failures: Vec::new(),
        }
    }

    #[test]
    fn requirements_report_known_failures_without_inventing_unqueried_features() {
        let mut missing = candidate(1, vk::PhysicalDeviceType::DISCRETE_GPU);
        missing.features = RequiredFeatures::default();
        missing.queues = vec![vk::QueueFamilyProperties {
            queue_flags: vk::QueueFlags::GRAPHICS,
            queue_count: 1,
            ..Default::default()
        }];
        let rejection = missing
            .assess()
            .err()
            .expect("requirements must fail together");
        assert_eq!(
            rejection.failures,
            [
                RequirementFailure::Feature("timelineSemaphore"),
                RequirementFailure::Feature("synchronization2"),
                RequirementFailure::Feature("dynamicRendering"),
                RequirementFailure::GraphicsComputeQueue,
            ]
        );
        for api in [vk::API_VERSION_1_2, vk::make_api_version(1, 1, 3, 0)] {
            let mut old = candidate(2, vk::PhysicalDeviceType::DISCRETE_GPU);
            old.info.api_version = api;
            old.features = RequiredFeatures::default();
            let rejection = old.assess().err().expect("unsupported API");
            assert_eq!(rejection.failures.len(), 1);
            assert!(!matches!(
                rejection.failures[0],
                RequirementFailure::Feature(_)
            ));
        }
        let mut supported = candidate(3, vk::PhysicalDeviceType::DISCRETE_GPU);
        supported.info.api_version = vk::make_api_version(0, 1, 4, 0);
        supported.queues.insert(
            0,
            vk::QueueFamilyProperties {
                queue_flags: vk::QueueFlags::GRAPHICS,
                queue_count: 1,
                ..Default::default()
            },
        );
        assert_eq!(
            supported
                .assess()
                .expect("supported future API")
                .queue_family_index,
            1
        );
    }

    #[test]
    fn selection_prioritizes_suitable_hardware_and_uses_cpu_as_last_candidate() {
        let kinds = [
            vk::PhysicalDeviceType::CPU,
            vk::PhysicalDeviceType::OTHER,
            vk::PhysicalDeviceType::VIRTUAL_GPU,
            vk::PhysicalDeviceType::INTEGRATED_GPU,
            vk::PhysicalDeviceType::DISCRETE_GPU,
        ];
        // Each prefix has a different preferred type; a later tie must not win.
        for end in 1..=kinds.len() {
            let mut candidates: Vec<_> = kinds[..end]
                .iter()
                .enumerate()
                .map(|(index, kind)| candidate(index as u64 + 1, *kind))
                .collect();
            candidates.push(candidate(90, kinds[end - 1]));
            let mut unsuitable = candidate(99, vk::PhysicalDeviceType::DISCRETE_GPU);
            unsuitable.features.dynamic_rendering = false;
            candidates.insert(0, unsuitable);
            let selected = select_device(candidates).expect("one suitable device");
            assert_eq!(selected.physical_device.as_raw(), end as u64);
        }
    }

    #[test]
    fn no_suitable_device_retains_each_adapters_reason() {
        let mut old = candidate(1, vk::PhysicalDeviceType::DISCRETE_GPU);
        old.info.api_version = vk::API_VERSION_1_2;
        let mut missing = candidate(2, vk::PhysicalDeviceType::INTEGRATED_GPU);
        missing.features.timeline_semaphore = false;
        let error = select_device(vec![old, missing])
            .err()
            .expect("no supported device");
        let text = error.to_string();
        assert!(text.contains("device-1") && text.contains("1.2.0"));
        assert!(text.contains("device-2") && text.contains("timelineSemaphore"));
        let DeviceInitError::NoSuitableDevice(rejections) = error else {
            panic!("wrong error")
        };
        assert_eq!(rejections.len(), 2);
        assert!(
            matches!(select_device(Vec::new()), Err(DeviceInitError::NoSuitableDevice(rejections)) if rejections.is_empty())
        );
    }

    #[test]
    fn actual_creation_configuration_requests_one_queue_and_required_features_only() {
        with_create_info(7, |info| {
            assert_eq!(info.queue_create_info_count, 1);
            assert_eq!(info.enabled_extension_count, 0);
            assert!(info.p_enabled_features.is_null());
            // SAFETY: with_create_info retains all referenced data for this call.
            let queue = unsafe { &*info.p_queue_create_infos };
            assert_eq!(queue.queue_family_index, 7);
            assert_eq!(queue.queue_count, 1);
            // SAFETY: The queue advertises the one live priority held by the helper.
            assert_eq!(unsafe { *queue.p_queue_priorities }, 1.0);
            let mut next = info.p_next.cast::<vk::BaseInStructure<'_>>();
            let mut seen = Vec::new();
            while !next.is_null() {
                // SAFETY: The helper supplies the live, well-formed feature chain.
                let header = unsafe { &*next };
                seen.push(header.s_type);
                match header.s_type {
                    vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES => {
                        // SAFETY: sType identifies this structure in the live chain.
                        let feature =
                            unsafe { &*next.cast::<vk::PhysicalDeviceVulkan12Features<'_>>() };
                        assert_eq!(feature.timeline_semaphore, vk::TRUE);
                        assert_eq!(feature.buffer_device_address, vk::FALSE);
                        assert_eq!(feature.descriptor_indexing, vk::FALSE);
                    }
                    vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES => {
                        // SAFETY: sType identifies this structure in the live chain.
                        let feature =
                            unsafe { &*next.cast::<vk::PhysicalDeviceVulkan13Features<'_>>() };
                        assert_eq!(feature.synchronization2, vk::TRUE);
                        assert_eq!(feature.dynamic_rendering, vk::TRUE);
                        assert_eq!(feature.maintenance4, vk::FALSE);
                        assert_eq!(feature.robust_image_access, vk::FALSE);
                    }
                    other => panic!("unexpected enabled feature structure: {other:?}"),
                }
                next = header.p_next;
            }
            assert_eq!(seen.len(), 2);
            assert!(seen.contains(&vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES));
            assert!(seen.contains(&vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES));
        });
    }

    #[test]
    #[ignore = "explicit real-GPU Vulkan 1.3 Device test; requires validation and debug_utils"]
    fn vulkan_device_smoke() {
        let dispatch = super::super::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            for iteration in 0..4 {
                let diagnostics = Diagnostics::new();
                let instance =
                    VulkanInstance::new(Arc::clone(&diagnostics)).unwrap_or_else(|error| {
                        panic!(
                            "iteration {iteration}: {error}; {:?}",
                            diagnostics.snapshot()
                        )
                    });
                super::super::assert_real_validation(&diagnostics);
                let device = VulkanDevice::new(instance).unwrap_or_else(|error| {
                    panic!(
                        "iteration {iteration}: {error}; {:?}",
                        diagnostics.snapshot()
                    )
                });
                assert_ne!(
                    device.selected.info.device_type,
                    vk::PhysicalDeviceType::CPU,
                    "CPU fallback is supported but does not count as a real-GPU acceptance test"
                );
                assert_ne!(device.queue, vk::Queue::null());
                drop(device);
                let report = diagnostics.snapshot();
                assert!(
                    report.is_clean(),
                    "iteration {iteration}, including shutdown: {report:#?}"
                );
                tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                    errors = report.errors, callback_failures = report.callback_failures,
                    "Vulkan Device smoke including shutdown passed");
            }
        });
    }
}
