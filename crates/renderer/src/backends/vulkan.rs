//! Private Vulkan bootstrap, memory ownership, and single-queue transfers.

mod buffer;
mod device;
mod diagnostics;
pub(crate) mod engine;
mod error;
pub(crate) mod graph;
// Standalone transfer resources remain acceptance/performance references; the
// shipping Graph owns its physical resources in resources.rs.
#[cfg(test)]
mod image;
mod instance;
mod memory;
#[cfg(all(feature = "presentation", windows))]
mod presentation;
mod program;
#[cfg(test)]
mod ranges;
mod resources;
mod transfer;

pub use instance::ValidationMode;

// Test-only cross-API identity check. Use the actual Required-validation Device
// selection path rather than a second adapter selector just for D3D12 evidence.
#[cfg(all(test, feature = "d3d12", windows))]
pub(super) fn preparation_adapter_identity() -> (String, u32, u32, [u8; 8]) {
    use ash::vk;
    let dispatch = real_vulkan_dispatch();
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let diagnostics = diagnostics::Diagnostics::new();
    let instance =
        instance::VulkanInstance::new(diagnostics.clone()).expect("Required Vulkan Instance");
    let device = device::VulkanDevice::new(instance).expect("Required Vulkan Device");
    assert_real_validation(&diagnostics);
    let mut identity = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut identity);
    // SAFETY: The selected physical Device belongs to the live owned Instance;
    // the output chain has one correctly initialized, stack-owned structure.
    unsafe {
        device
            .instance_raw()
            .get_physical_device_properties2(device.physical_device(), &mut properties);
    }
    assert_ne!(
        properties.properties.device_type,
        vk::PhysicalDeviceType::CPU,
        "hardware GPU required"
    );
    let vendor_id = properties.properties.vendor_id;
    let device_id = properties.properties.device_id;
    assert_eq!(
        identity.device_luid_valid,
        vk::TRUE,
        "Vulkan adapter LUID must be available"
    );
    let result = (
        device.name().to_owned(),
        vendor_id,
        device_id,
        identity.device_luid,
    );
    drop(device);
    assert!(
        diagnostics.snapshot().is_clean(),
        "Vulkan teardown diagnostics: {:?}",
        diagnostics.snapshot()
    );
    result
}

#[cfg(test)]
fn real_vulkan_dispatch() -> tracing::Dispatch {
    let filter = match std::env::var("RUST_LOG") {
        Ok(filter) => filter,
        Err(std::env::VarError::NotPresent) => {
            "renderer=info,renderer::vulkan::validation=warn".to_owned()
        }
        Err(error) => panic!("invalid RUST_LOG: {error}"),
    };
    tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_new(filter).expect("valid RUST_LOG filter"),
            )
            .with_test_writer()
            .with_ansi(false)
            .without_time()
            .finish(),
    )
}

#[cfg(test)]
fn assert_real_validation(diagnostics: &diagnostics::Diagnostics) {
    // External configuration can override API requests. Check the documented
    // SDK's effective settings before accepting results, including standalone
    // image tests where disabled core checks would invalidate a clean report.
    let status = diagnostics.snapshot().messages.into_iter().find(|message|
        message.message_id_name.as_deref() == Some("WARNING-CreateInstance-status-message"))
        .expect("Validation Layer startup status must be available; unknown versions/configuration require review");
    let (_, enables) = status
        .message
        .split_once("Current Enables:")
        .expect("recognized Validation Layer startup enables report");
    let (enables, disables) = enables
        .split_once("Current Disables:")
        .expect("recognized Validation Layer startup disables report");
    assert!(
        enables.contains("VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT"),
        "Synchronization Validation must actually be enabled: {}",
        status.message
    );
    assert_eq!(
        disables
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches('.'),
        "None",
        "core validation checks must remain enabled: {}",
        status.message
    );
    tracing::info!(target: instance::LOG_TARGET, validation_status = %status.message,
        "Validation Layer confirmed SyncVal enabled and no disabled checks");
}
