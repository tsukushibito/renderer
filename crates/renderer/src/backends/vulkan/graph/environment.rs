//! Owned environment facts and callback monitoring without native ownership.

use std::sync::Arc;

use ash::vk;

use super::super::diagnostics::Diagnostics;
use super::super::instance::{REQUIRED_API, ValidationMode, api_version};
use super::VulkanRuntime;

/// Owned adapter/driver facts. Driver version encoding is vendor-specific.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub name: String,
    pub api_version: String,
    pub api_version_raw: u32,
    pub instance_api_version: String,
    /// Raw vendor encoding; this is not a packed Vulkan API version.
    pub driver_version_raw: u32,
    pub driver_name: Option<String>,
    pub driver_info: Option<String>,
    pub driver_id_raw: i32,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: String,
    pub queue_family: u32,
}

/// Callback ownership only. Keeping this monitor after runtime destruction does
/// not keep an Instance, Device, allocation or submitted command alive.
#[derive(Clone)]
pub struct ValidationMonitor {
    diagnostics: Arc<Diagnostics>,
    requested: ValidationMode,
}

/// Counts are independent of log filtering. During live concurrent callbacks,
/// the separately sampled counters are not a coherent transactional snapshot.
/// Take a final report after destroying all callback-producing native owners.
#[derive(Clone, Debug)]
pub struct ValidationReport {
    pub requested: ValidationMode,
    pub warnings: usize,
    pub errors: usize,
    pub callback_failures: usize,
    pub dropped_problems: usize,
    pub problems: Vec<String>,
    /// Original distinct SDK startup messages, retained independently of logs.
    pub layer_status_messages: Vec<String>,
    pub dropped_layer_statuses: usize,
    /// True only for recognized, mutually consistent startup messages. Missing
    /// messages, unsupported formats and retention overflow remain unverified.
    pub startup_status_recognized: bool,
    /// Some(true) requires a recognized report with no disabled checks. This is
    /// deliberately as strict as the repository's real-GPU acceptance policy.
    pub core_validation: Option<bool>,
    pub synchronization_validation: Option<bool>,
}

impl ValidationReport {
    /// Clean callback counts do not prove that Validation was actually enabled.
    /// Correctness acceptance must additionally require both effective flags.
    pub fn is_clean(&self) -> bool {
        self.warnings == 0
            && self.errors == 0
            && self.callback_failures == 0
            && self.dropped_problems == 0
    }
}

impl ValidationMonitor {
    /// Unknown effective settings are reported as None. In Disabled mode, an
    /// absent status message does not prove that the loader injected no layers.
    pub fn report(&self) -> ValidationReport {
        let snapshot = self.diagnostics.snapshot();
        let effective = effective_validation(
            &snapshot.layer_status_messages,
            snapshot.dropped_layer_statuses,
        );
        ValidationReport {
            requested: self.requested,
            warnings: snapshot.warnings,
            errors: snapshot.errors,
            callback_failures: snapshot.callback_failures,
            dropped_problems: snapshot.dropped_problems,
            problems: snapshot
                .problems
                .into_iter()
                .map(|message| {
                    format!(
                        "{:?} [{}:{}] {}",
                        message.severity,
                        message.message_id_name.as_deref().unwrap_or("unnamed"),
                        message.message_id_number,
                        message.message
                    )
                })
                .collect(),
            layer_status_messages: snapshot.layer_status_messages,
            dropped_layer_statuses: snapshot.dropped_layer_statuses,
            startup_status_recognized: effective.is_some(),
            core_validation: effective.map(|settings| settings.core),
            synchronization_validation: effective.map(|settings| settings.synchronization),
        }
    }
}

impl VulkanRuntime {
    pub fn device_info(&self) -> DeviceInfo {
        let device = &self.owner.device;
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
        // SAFETY: The retained Device owns the Instance and enumerated physical
        // adapter. Vulkan 1.3 provides driver properties; all output/pNext storage
        // is live for this read-only call and only owned values escape.
        unsafe {
            device
                .instance_raw()
                .get_physical_device_properties2(device.physical_device(), &mut properties);
        }
        let properties = properties.properties;
        DeviceInfo {
            name: device.name().to_owned(),
            api_version: api_version(properties.api_version),
            api_version_raw: properties.api_version,
            instance_api_version: api_version(REQUIRED_API),
            driver_version_raw: properties.driver_version,
            driver_name: driver
                .driver_name_as_c_str()
                .ok()
                .map(|value| value.to_string_lossy().into_owned()),
            driver_info: driver
                .driver_info_as_c_str()
                .ok()
                .map(|value| value.to_string_lossy().into_owned()),
            driver_id_raw: driver.driver_id.as_raw(),
            vendor_id: properties.vendor_id,
            device_id: properties.device_id,
            device_type: format!("{:?}", properties.device_type),
            queue_family: device.queue_family(),
        }
    }

    pub fn validation_monitor(&self) -> ValidationMonitor {
        ValidationMonitor {
            diagnostics: Arc::clone(&self.diagnostics),
            requested: self.owner.device.validation_mode(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EffectiveValidation {
    core: bool,
    synchronization: bool,
}

fn effective_validation(messages: &[String], dropped: usize) -> Option<EffectiveValidation> {
    if dropped != 0 {
        return None;
    }
    let mut statuses = messages.iter();
    let first = parse_status(statuses.next()?)?;
    for message in statuses {
        if parse_status(message)? != first {
            return None;
        }
    }
    Some(first)
}

/// This intentionally recognizes the pinned SDK's startup grammar rather than
/// guessing effective settings from arbitrary occurrences of feature names.
fn parse_status(message: &str) -> Option<EffectiveValidation> {
    const SYNCHRONIZATION: &str = "VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT";
    const ENABLES: &[&str] = &[
        "VK_VALIDATION_FEATURE_ENABLE_GPU_ASSISTED_EXT",
        "VK_VALIDATION_FEATURE_ENABLE_GPU_ASSISTED_RESERVE_BINDING_SLOT_EXT",
        "VK_VALIDATION_FEATURE_ENABLE_BEST_PRACTICES_EXT",
        "VK_VALIDATION_FEATURE_ENABLE_DEBUG_PRINTF_EXT",
        SYNCHRONIZATION,
    ];
    const DISABLES: &[&str] = &[
        "VK_VALIDATION_FEATURE_DISABLE_ALL_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_SHADERS_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_THREAD_SAFETY_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_API_PARAMETERS_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_OBJECT_LIFETIMES_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_CORE_CHECKS_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_UNIQUE_HANDLES_EXT",
        "VK_VALIDATION_FEATURE_DISABLE_SHADER_VALIDATION_CACHE_EXT",
    ];
    let mut lines = message.trim().lines().map(str::trim);
    if lines.next()? != "vkCreateInstance(): Khronos Validation Layer Active:" {
        return None;
    }
    let enables = parse_flags(lines.next()?.strip_prefix("Current Enables:")?, ENABLES)?;
    let disables = parse_flags(lines.next()?.strip_prefix("Current Disables:")?, DISABLES)?;
    if lines.any(|line| !line.is_empty()) {
        return None;
    }
    Some(EffectiveValidation {
        core: disables.is_empty(),
        synchronization: enables.contains(&SYNCHRONIZATION)
            && !disables.contains(&"VK_VALIDATION_FEATURE_DISABLE_ALL_EXT"),
    })
}

fn parse_flags<'a>(line: &'a str, known: &[&str]) -> Option<Vec<&'a str>> {
    let value = line.trim().strip_suffix('.')?;
    if value == "None" {
        return Some(Vec::new());
    }
    let values = value.split(',').map(str::trim).collect::<Vec<_>>();
    values
        .iter()
        .all(|value| known.contains(value))
        .then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE: &str = "vkCreateInstance(): Khronos Validation Layer Active:\n    Current Enables: VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT.\n    Current Disables: None.\n";
    const INACTIVE_SYNC: &str = "vkCreateInstance(): Khronos Validation Layer Active:\n    Current Enables: None.\n    Current Disables: None.\n";

    #[test]
    fn startup_parser_distinguishes_core_and_sync_effective_settings() {
        assert_eq!(
            parse_status(ACTIVE),
            Some(EffectiveValidation {
                core: true,
                synchronization: true,
            })
        );
        assert_eq!(
            parse_status(INACTIVE_SYNC),
            Some(EffectiveValidation {
                core: true,
                synchronization: false,
            })
        );
        let core_disabled =
            ACTIVE.replace("None.", "VK_VALIDATION_FEATURE_DISABLE_CORE_CHECKS_EXT.");
        assert_eq!(
            parse_status(&core_disabled),
            Some(EffectiveValidation {
                core: false,
                synchronization: true,
            })
        );
        let all_disabled = ACTIVE.replace("None.", "VK_VALIDATION_FEATURE_DISABLE_ALL_EXT.");
        assert_eq!(
            parse_status(&all_disabled),
            Some(EffectiveValidation {
                core: false,
                synchronization: false,
            })
        );
    }

    #[test]
    fn missing_unknown_truncated_or_conflicting_statuses_are_unverified() {
        assert_eq!(effective_validation(&[], 0), None);
        assert_eq!(
            parse_status("Current Enables: synchronization enabled"),
            None
        );
        assert_eq!(
            parse_status(&ACTIVE.replace("None.", "VK_VALIDATION_FEATURE_DISABLE_FUTURE_EXT.")),
            None
        );
        assert_eq!(
            parse_status(&ACTIVE.replace("Current Disables: None.", "")),
            None
        );
        assert_eq!(
            parse_status(&format!("{ACTIVE}Unknown future setting")),
            None
        );
        assert_eq!(
            effective_validation(&[ACTIVE.to_owned(), INACTIVE_SYNC.to_owned()], 0),
            None
        );
        assert_eq!(effective_validation(&[ACTIVE.to_owned()], 1), None);
    }

    #[test]
    fn startup_report_survives_recent_message_eviction_and_monitor_owner_drop() {
        let diagnostics = Diagnostics::new();
        let monitor = ValidationMonitor {
            diagnostics: Arc::clone(&diagnostics),
            requested: ValidationMode::Required,
        };
        let emit = |name: &std::ffi::CStr, message: &std::ffi::CStr, severity| {
            let callback = diagnostics.messenger_create_info();
            let data = vk::DebugUtilsMessengerCallbackDataEXT::default()
                .message_id_name(name)
                .message(message);
            // SAFETY: CPU-only invocation of our callback. The Arc allocation,
            // callback-data storage and both C strings remain live for the call.
            unsafe {
                callback.pfn_user_callback.unwrap()(
                    severity,
                    vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION,
                    &data,
                    callback.p_user_data,
                );
            }
        };
        let active = std::ffi::CString::new(ACTIVE).unwrap();
        emit(
            c"WARNING-CreateInstance-status-message",
            &active,
            vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
        );
        for _ in 0..80 {
            emit(
                c"noise",
                c"later info",
                vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
            );
        }
        emit(
            c"regression",
            c"a problem",
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
        );
        drop(diagnostics);
        let report = monitor.report();
        assert!(report.startup_status_recognized);
        assert_eq!(report.core_validation, Some(true));
        assert_eq!(report.synchronization_validation, Some(true));
        assert_eq!(report.layer_status_messages, [ACTIVE]);
        assert_eq!(report.errors, 1);
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0].contains("regression"));
        assert!(!report.is_clean());
    }

    #[test]
    fn disabled_request_without_observed_status_does_not_invent_effective_state() {
        let monitor = ValidationMonitor {
            diagnostics: Diagnostics::new(),
            requested: ValidationMode::Disabled,
        };
        let report = monitor.report();
        assert_eq!(report.requested, ValidationMode::Disabled);
        assert!(!report.startup_status_recognized);
        assert_eq!(report.core_validation, None);
        assert_eq!(report.synchronization_validation, None);
        assert!(report.is_clean());
    }
}
