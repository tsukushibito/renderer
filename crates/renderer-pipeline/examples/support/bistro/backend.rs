//! Explicit preparation budgets and backend-specific acceptance diagnostics.

use super::{
    Error, Options, lifecycle,
    pipeline::{Budget, PreparedSet},
};
use renderer::{
    graph::{BufferDesc, ImageDesc, PassHandle},
    runtime::{
        ExecutionError, FrameCompletion, FrameId, ResourceClass, Runtime, RuntimeConfig,
        RuntimeLimits, WorkingSet,
    },
};

fn config(options: &Options, budget: &Budget) -> Result<RuntimeConfig, Error> {
    match options.api.as_str() {
        #[cfg(feature = "vulkan")]
        "vulkan" => Ok(RuntimeConfig::Vulkan(renderer::runtime::VulkanConfig {
            staging_bytes: budget.vulkan_staging_bytes(options.inflight),
            staging_ranges: budget.vulkan_staging_ranges(options.inflight),
            max_shader_bindings: budget.vulkan_shader_bindings,
            ..Default::default()
        })),
        #[cfg(all(windows, feature = "d3d12"))]
        "d3d12" => Ok(RuntimeConfig::D3D12(renderer::runtime::D3D12Config {
            buffer_count: budget.buffer_count,
            max_buffer_bytes: budget.max_buffer_bytes,
            upload_bytes: budget.upload_bytes as u64,
            readback_bytes: budget.readback_bytes,
            scratch_bytes: 16,
            ..Default::default()
        })),
        _ => Err("selected backend was not compiled into this example".into()),
    }
}

fn limits(options: &Options, budget: &Budget) -> RuntimeLimits {
    RuntimeLimits {
        graph: budget.graph,
        frames: options.inflight,
        results: options.inflight + 1,
        resource_capacity: budget.physical_resources,
        range_snapshots: budget.ranges,
        initialized_ranges_per_resource: 32,
        provenance_segments_per_resource: 1,
        index_prefixes_per_resource: 0,
    }
}

fn with_working_set<T>(
    resource_checks: bool,
    buffers: &[(u64, usize)],
    images: &[(ImageDesc, usize)],
    prepared: &PreparedSet,
    prepare: impl FnOnce(WorkingSet<'_>) -> Result<T, ExecutionError>,
) -> Result<T, Error> {
    let resources = buffers
        .iter()
        .map(|&(size, count)| ResourceClass::Buffer {
            desc: BufferDesc { size },
            count,
        })
        .chain(
            images
                .iter()
                .map(|&(desc, count)| ResourceClass::Image { desc, count }),
        )
        .collect::<Vec<_>>();
    let mut raster = Vec::new();
    let mut compute = Vec::new();
    for set in prepared.working_sets() {
        raster.extend_from_slice(set.raster);
        compute.extend_from_slice(set.compute);
    }
    if resource_checks {
        raster.push(lifecycle::MIP_PROBE_SDR.pipeline());
    }
    Ok(prepare(WorkingSet {
        resources: &resources,
        raster: &raster,
        compute: &compute,
    })?)
}

fn require_hardware(_runtime: &Runtime) -> Result<(), Error> {
    #[cfg(feature = "vulkan")]
    if let Some(runtime) = _runtime.as_vulkan()
        && runtime.device_info().device_type == "CPU"
    {
        return Err("CPU Vulkan adapters are not accepted".into());
    }
    Ok(())
}

pub(super) fn create(options: &Options, budget: &Budget) -> Result<Runtime, Error> {
    let config = config(options, budget)?;
    let prepared = budget.prepared.as_ref().expect("prepared YAML pipeline");
    let runtime = with_working_set(
        options.resource_checks,
        &budget.buffers,
        &budget.images,
        prepared,
        |set| Runtime::new(config, limits(options, budget), set),
    )?;
    require_hardware(&runtime)?;
    Ok(runtime)
}

#[cfg(all(windows, feature = "presentation"))]
pub(super) fn open(
    options: &Options,
    budget: &Budget,
    window: std::sync::Arc<winit::window::Window>,
    presentation: renderer::PresentationConfig,
) -> Result<(Runtime, renderer::runtime::WindowPresenter), Error> {
    let config = config(options, budget)?;
    let prepared = budget.prepared.as_ref().expect("prepared YAML pipeline");
    let (runtime, presenter) = with_working_set(
        options.resource_checks,
        &budget.buffers,
        &budget.images,
        prepared,
        |set| Runtime::for_window(config, limits(options, budget), set, window, presentation),
    )?;
    require_hardware(&runtime)?;
    Ok((runtime, presenter))
}

#[cfg(all(windows, feature = "presentation"))]
pub(super) fn resize(
    runtime: &mut Runtime,
    options: &Options,
    budget: &Budget,
    prepared: &PreparedSet,
) -> Result<(), Error> {
    with_working_set(
        options.resource_checks,
        &budget.resized_buffers(options, prepared),
        &Budget::frame_images(options, prepared),
        prepared,
        |set| runtime.prepare_working_set(set),
    )
}

pub(super) fn wait_for(runtime: &mut Runtime, frame: FrameId) -> Result<FrameCompletion, Error> {
    runtime
        .wait_for_frame(frame, super::WAIT)?
        .ok_or_else(|| "Bistro completion timed out".into())
}

pub(super) fn readback(
    completed: &mut FrameCompletion,
    pass: PassHandle,
    bytes: &mut [u8],
) -> Result<(), Error> {
    if completed.readback_len(pass)? != bytes.len() {
        return Err("capture readback length mismatch".into());
    }
    Ok(completed.readback_into(pass, bytes)?)
}

pub(super) fn info(runtime: &Runtime) -> serde_json::Value {
    #[cfg(feature = "vulkan")]
    if let Some(runtime) = runtime.as_vulkan() {
        let info = runtime.device_info();
        return serde_json::json!({"api":"vulkan","name":info.name,"device_type":info.device_type,"vendor_id":info.vendor_id,
            "device_id":info.device_id,"device_luid":info.device_luid,"api_version":info.api_version,
            "driver_version_raw":info.driver_version_raw,"driver_name":info.driver_name,"driver_info":info.driver_info,
            "validation_mode":if cfg!(feature="development-validation"){"strict"}else{"disabled"}});
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(runtime) = runtime.as_d3d12() {
        let info = runtime.device_info();
        return serde_json::json!({"api":"d3d12","name":info.name,"vendor_id":info.vendor_id,"device_id":info.device_id,
            "device_luid":info.luid,"driver_version":info.driver_version,
            "validation_mode":if cfg!(feature="development-validation"){"strict"}else{"disabled"}});
    }
    unreachable!("Bistro Runtime uses a compiled backend")
}

pub(super) fn memory_snapshot(runtime: &Runtime) -> Result<serde_json::Value, Error> {
    #[cfg(feature = "vulkan")]
    if let Some(runtime) = runtime.as_vulkan() {
        return super::memory::vulkan(runtime);
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(runtime) = runtime.as_d3d12() {
        return super::memory::d3d12(runtime);
    }
    unreachable!("Bistro Runtime uses a compiled backend")
}

pub(super) fn close(runtime: Runtime) -> Result<serde_json::Value, Error> {
    #[cfg(feature = "vulkan")]
    if let Some(native) = runtime.as_vulkan() {
        let monitor = native.validation_monitor();
        drop(runtime);
        let report = monitor.report();
        if !report.is_clean() {
            return Err(format!("Vulkan validation: {report:#?}").into());
        }
        let strict = cfg!(feature = "development-validation");
        if strict
            && (report.core_validation != Some(true)
                || report.synchronization_validation != Some(true))
        {
            return Err(format!("Vulkan effective validation not verified: {report:#?}").into());
        }
        return Ok(
            serde_json::json!({"clean":true,"mode":if strict{"strict"}else{"disabled"},
            "requested":format!("{:?}",report.requested),"core_validation":report.core_validation,
            "synchronization_validation":report.synchronization_validation,"warnings":report.warnings,
            "errors":report.errors,"callback_failures":report.callback_failures}),
        );
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(native) = runtime.as_d3d12() {
        let monitor = native.validation_monitor();
        drop(runtime);
        let report = monitor.snapshot();
        let strict = cfg!(feature = "development-validation");
        if !report.is_clean()
            || report.debug_layer_enabled != strict
            || report.gpu_based_validation_enabled != strict
            || (strict && !report.teardown_checked)
        {
            return Err(format!("D3D12 validation: {report:#?}").into());
        }
        return Ok(
            serde_json::json!({"clean":true,"mode":if strict{"strict"}else{"disabled"},
            "debug_layer_enabled":report.debug_layer_enabled,"gpu_based_validation_enabled":report.gpu_based_validation_enabled,
            "teardown_checked":report.teardown_checked,"warnings":report.warnings,"errors":report.errors,
            "corruption":report.corruption,"callback_failures":report.callback_failures}),
        );
    }
    unreachable!("Bistro Runtime uses a compiled backend")
}
