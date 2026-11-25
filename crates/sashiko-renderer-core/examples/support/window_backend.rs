//! Windowed sample setup and diagnostics.

use super::window_scene as scene;
use sashiko_renderer_core::{
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::{BufferDesc, ImageDesc},
    presentation::PresentationConfig,
    program::ManagedRasterProgram,
    runtime::{
        ExecutionError, ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, WindowPresenter,
        WorkingSet,
    },
};
use std::{error::Error, sync::Arc};
use winit::window::Window;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub const IN_FLIGHT: usize = 3;
const D3D12_READBACK_BYTES: u64 = 16 * 1024 * 1024;

/// Reject a request that cannot fit even before native row-pitch padding.
/// The exact native footprint is still checked by the runtime before recording.
pub fn verify_readback_len(api: Api, extent: (u32, u32)) -> Result<usize> {
    let bytes = u64::from(extent.0)
        .checked_mul(u64::from(extent.1))
        .and_then(|pixels| pixels.checked_mul(4));
    if api == Api::D3d12 && bytes.is_none_or(|bytes| bytes > D3D12_READBACK_BYTES) {
        return Err(RuntimeCapacityError::new(
            RuntimeCapacityKind::ReadbackBytes,
            CapacityRecovery::Reconfigure,
            Some(D3D12_READBACK_BYTES),
            bytes,
        )
        .into());
    }
    Ok(usize::try_from(
        bytes.ok_or("verification image byte size overflow")?,
    )?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Api {
    Vulkan,
    D3d12,
}
impl Api {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Vulkan => "vulkan",
            Self::D3d12 => "d3d12",
        }
    }

    pub fn config(self) -> Result<RuntimeConfig> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan => Ok(RuntimeConfig::Vulkan(
                sashiko_renderer_core::runtime::VulkanConfig {
                    staging_bytes: 32 * 1024 * 1024,
                    staging_ranges: 64,
                    max_shader_bindings: 8,
                    ..Default::default()
                },
            )),
            #[cfg(feature = "d3d12")]
            Self::D3d12 => Ok(RuntimeConfig::D3D12(
                sashiko_renderer_core::runtime::D3D12Config {
                    buffer_count: 2,
                    max_buffer_bytes: 512,
                    upload_bytes: 512,
                    readback_bytes: D3D12_READBACK_BYTES,
                    scratch_bytes: 512,
                    ..Default::default()
                },
            )),
            #[cfg(not(all(feature = "vulkan", feature = "d3d12")))]
            _ => Err(format!("{} backend was not compiled", self.name()).into()),
        }
    }
}

pub fn open(
    api: Api,
    window: Arc<Window>,
    vsync: bool,
    profiling: bool,
) -> Result<(Runtime, WindowPresenter)> {
    let limits = RuntimeLimits {
        graph: scene::capacity(),
        frames: IN_FLIGHT,
        results: IN_FLIGHT,
        resource_capacity: 64,
        range_snapshots: 64,
        initialized_ranges_per_resource: 8,
        provenance_segments_per_resource: 16,
        index_prefixes_per_resource: 16,
    };
    let presentation = PresentationConfig {
        max_frames: IN_FLIGHT,
        max_retired_swapchains: 4,
        desired_images: 3,
        vsync,
    };
    let (mut runtime, mut presenter) = Runtime::for_window(
        api.config()?,
        limits,
        WorkingSet::default(),
        window,
        presentation,
    )?;
    runtime.set_profiling(profiling)?;
    presenter.set_profiling(profiling)?;
    Ok((runtime, presenter))
}

pub fn prepare_resources(runtime: &mut Runtime, extent: (u32, u32)) -> Result<()> {
    runtime.prepare_working_set(WorkingSet {
        resources: &[
            ResourceClass::Buffer {
                desc: BufferDesc {
                    size: scene::VERTEX_BYTES,
                },
                count: 1,
            },
            ResourceClass::Buffer {
                desc: BufferDesc {
                    size: scene::INDEX_BYTES,
                },
                count: 1,
            },
            ResourceClass::Image {
                desc: ImageDesc::rgba8(extent.0, extent.1),
                count: 3 * IN_FLIGHT,
            },
            ResourceClass::Image {
                desc: ImageDesc::depth32(extent.0, extent.1),
                count: IN_FLIGHT,
            },
        ],
        raster: &[ManagedRasterProgram::mesh().into()],
        ..Default::default()
    })?;
    Ok(())
}

pub enum Monitor {
    #[cfg(feature = "vulkan")]
    Vulkan(sashiko_renderer_core::vulkan::ValidationMonitor),
    #[cfg(feature = "d3d12")]
    D3d12(sashiko_renderer_core::d3d12::D3D12ValidationMonitor),
}
impl Monitor {
    pub fn new(runtime: &Runtime) -> Self {
        #[cfg(feature = "vulkan")]
        if let Some(runtime) = runtime.as_vulkan() {
            return Self::Vulkan(runtime.validation_monitor());
        }
        #[cfg(feature = "d3d12")]
        if let Some(runtime) = runtime.as_d3d12() {
            return Self::D3d12(runtime.validation_monitor());
        }
        unreachable!("initialized runtime uses a compiled backend")
    }
    pub fn check(&self, final_teardown: bool) -> Result<serde_json::Value> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => {
                let report = value.report();
                if !report.is_clean() {
                    return Err(format!("Vulkan validation failed: {report:?}").into());
                }
                if cfg!(feature = "development-validation")
                    && (report.core_validation != Some(true)
                        || report.synchronization_validation != Some(true))
                {
                    return Err(
                        format!("Core/SyncVal effective settings unverified: {report:?}").into(),
                    );
                }
                Ok(
                    serde_json::json!({"requested":format!("{:?}",report.requested),
                    "core":report.core_validation,"sync":report.synchronization_validation,
                    "warnings":report.warnings,"errors":report.errors,"final_teardown":final_teardown}),
                )
            }
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => {
                let report = value.snapshot();
                if !report.is_clean() {
                    return Err(format!("D3D12 validation failed: {report:?}").into());
                }
                if cfg!(feature = "development-validation")
                    && (!report.debug_layer_enabled
                        || !report.gpu_based_validation_enabled
                        || (final_teardown && !report.teardown_checked))
                {
                    return Err(format!(
                        "D3D12 effective validation/teardown unverified: {report:?}"
                    )
                    .into());
                }
                Ok(serde_json::json!({"debug_layer":report.debug_layer_enabled,
                    "gbv":report.gpu_based_validation_enabled,"teardown_checked":report.teardown_checked,
                    "warnings":report.warnings,"errors":report.errors,"corruption":report.corruption}))
            }
        }
    }
}

pub fn device_info(runtime: &Runtime) -> serde_json::Value {
    #[cfg(feature = "vulkan")]
    if let Some(runtime) = runtime.as_vulkan() {
        let info = runtime.device_info();
        return serde_json::json!({"name":info.name,"luid":info.device_luid,
            "driver":info.driver_version_raw,"api":info.api_version});
    }
    #[cfg(feature = "d3d12")]
    if let Some(runtime) = runtime.as_d3d12() {
        let info = runtime.device_info();
        return serde_json::json!({"name":info.name,"luid":info.luid,
            "driver":info.driver_version,"shader_model":info.shader_model});
    }
    unreachable!("initialized runtime uses a compiled backend")
}

pub fn capacity_recovery(error: &(dyn Error + 'static)) -> Option<CapacityRecovery> {
    if let Some(value) = error.downcast_ref::<ExecutionError>() {
        return value.capacity().map(|capacity| capacity.recovery);
    }
    if let Some(value) = error.downcast_ref::<sashiko_renderer_core::graph::GraphError>() {
        use sashiko_renderer_core::graph::GraphCapacityKind;
        return value.capacity().map(|capacity| match capacity.kind {
            GraphCapacityKind::Slots
            | GraphCapacityKind::SnapshotInputs
            | GraphCapacityKind::ImportRangeSlots => CapacityRecovery::ReleaseOwners,
            _ => CapacityRecovery::Reconfigure,
        });
    }
    error
        .downcast_ref::<RuntimeCapacityError>()
        .map(|capacity| capacity.recovery)
}

/// The sample polls and consumes all of its frame/result owners each redraw.
/// It may therefore retry backpressure, but never missing preparation or a
/// request that exceeds a configured bound.
pub fn is_backpressure(error: &(dyn Error + 'static)) -> bool {
    matches!(
        capacity_recovery(error),
        Some(
            CapacityRecovery::Poll
                | CapacityRecovery::ReleaseResults
                | CapacityRecovery::ReleaseOwners
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_verification_is_a_permanent_capacity_failure_before_gpu_work() {
        let error = verify_readback_len(Api::D3d12, (3440, 1440)).unwrap_err();
        let capacity = error.downcast_ref::<RuntimeCapacityError>().unwrap();
        assert_eq!(capacity.resource, RuntimeCapacityKind::ReadbackBytes);
        assert_eq!(capacity.limit, Some(16_777_216));
        assert_eq!(capacity.required, Some(19_814_400));
        assert!(!is_backpressure(error.as_ref()));
        assert_eq!(
            verify_readback_len(Api::D3d12, (1920, 1080)).unwrap(),
            8_294_400
        );
        assert_eq!(
            verify_readback_len(Api::Vulkan, (3440, 1440)).unwrap(),
            19_814_400
        );
        for recovery in [
            CapacityRecovery::Poll,
            CapacityRecovery::ReleaseResults,
            CapacityRecovery::ReleaseOwners,
        ] {
            let error = RuntimeCapacityError::new(
                RuntimeCapacityKind::ResultSlots,
                recovery,
                Some(3),
                None,
            );
            assert!(is_backpressure(&error));
        }
        let unprepared = RuntimeCapacityError::new(
            RuntimeCapacityKind::Programs,
            CapacityRecovery::Prepare,
            None,
            None,
        );
        assert!(!is_backpressure(&unprepared));
    }
}
