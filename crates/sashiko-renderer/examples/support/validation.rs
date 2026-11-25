use sashiko_renderer_core::runtime::Runtime;
use std::error::Error;
pub fn check_validation(runtime: Runtime) -> Result<(), Box<dyn Error>> {
    let strict = cfg!(feature = "development-validation");
    #[cfg(feature = "vulkan")]
    if let Some(vulkan) = runtime.as_vulkan() {
        let monitor = vulkan.validation_monitor();
        drop(runtime);
        let report = monitor.report();
        if !report.is_clean()
            || (strict
                && (report.core_validation != Some(true)
                    || report.synchronization_validation != Some(true)))
        {
            return Err(format!("unexpected Vulkan validation report {report:#?}").into());
        }
        return Ok(());
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(d3d12) = runtime.as_d3d12() {
        let monitor = d3d12.validation_monitor();
        drop(runtime);
        let report = monitor.snapshot();
        if !report.is_clean()
            || report.debug_layer_enabled != strict
            || report.gpu_based_validation_enabled != strict
            || (strict && !report.teardown_checked)
        {
            return Err(format!("unexpected D3D12 validation report {report:#?}").into());
        }
        return Ok(());
    }
    unreachable!("a Runtime has a compiled backend");
}
