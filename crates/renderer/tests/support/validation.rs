use renderer::runtime::Runtime;

/// Capture the active backend's monitor before destroying the Runtime so its
/// teardown messages are part of the acceptance check.
pub fn check_after_teardown(runtime: Runtime) {
    let strict = cfg!(feature = "development-validation");
    #[cfg(feature = "vulkan")]
    let vulkan = runtime.as_vulkan().map(|device| {
        assert_ne!(device.device_info().device_type, "CPU");
        device.validation_monitor()
    });
    #[cfg(all(windows, feature = "d3d12"))]
    let d3d12 = runtime.as_d3d12().map(|device| device.validation_monitor());
    drop(runtime);
    #[cfg(feature = "vulkan")]
    if let Some(monitor) = vulkan {
        let report = monitor.report();
        assert!(report.is_clean(), "{report:?}");
        if strict {
            assert_eq!(report.core_validation, Some(true));
            assert_eq!(report.synchronization_validation, Some(true));
        }
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(monitor) = d3d12 {
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(report.debug_layer_enabled, strict);
        assert_eq!(report.gpu_based_validation_enabled, strict);
        assert!(!strict || report.teardown_checked);
    }
}
