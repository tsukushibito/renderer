//! Thin consumer dispatch; Graph construction and window input stay shared.

#[cfg(feature = "vulkan")]
use super::window_scene as scene;
#[cfg(feature = "vulkan")]
use renderer::graph::BufferDesc;
use renderer::{
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::{
        BufferHandle, CompiledGraph, ImageDesc, ImageHandle, ImportBuffer, ImportImage, PassHandle,
        ResourceKey,
    },
    presentation::{PresentOutcome, PresentationConfig},
    profiling::{PresentationMetrics, SubmissionMetrics},
    program::ManagedRasterProgram,
};
use std::{error::Error, sync::Arc, time::Duration};
use winit::window::Window;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

const D3D12_READBACK_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(feature = "d3d12")]
const D3D12_EXECUTIONS: usize = 3;

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
}

pub enum Backend {
    #[cfg(feature = "vulkan")]
    Vulkan(Box<Vulkan>),
    #[cfg(feature = "d3d12")]
    D3d12(Box<D3d12>),
}
#[cfg(feature = "vulkan")]
pub struct Vulkan {
    runtime: renderer::VulkanRuntime,
    presenter: renderer::Presenter,
}
#[cfg(feature = "d3d12")]
pub struct D3d12 {
    runtime: renderer::D3D12Runtime,
    presenter: renderer::D3D12Presenter,
}

// Frame records are fixed-capacity application storage. Keep native ownership
// inline: boxing the larger variant would allocate and free on every frame.
#[allow(clippy::large_enum_variant)]
pub enum Pending {
    #[cfg(feature = "vulkan")]
    Vulkan(renderer::Submission),
    #[cfg(feature = "d3d12")]
    D3d12(renderer::D3D12Submission),
}
// Completion is consumed immediately into caller-owned buffers; the same inline
// ownership avoids a heap allocation solely for backend dispatch.
#[allow(clippy::large_enum_variant)]
pub enum Completed {
    #[cfg(feature = "vulkan")]
    Vulkan(renderer::SubmissionResult),
    #[cfg(feature = "d3d12")]
    D3d12(renderer::D3D12SubmissionResult),
}
pub enum Monitor {
    #[cfg(feature = "vulkan")]
    Vulkan(renderer::ValidationMonitor),
    #[cfg(feature = "d3d12")]
    D3d12(renderer::D3D12ValidationMonitor),
}

impl Backend {
    pub fn new(api: Api, window: Arc<Window>, vsync: bool, profiling: bool) -> Result<Self> {
        let presentation = PresentationConfig {
            max_frames: 3,
            max_retired_swapchains: 4,
            desired_images: 3,
            vsync,
        };
        let mut backend = match api {
            #[cfg(feature = "vulkan")]
            Api::Vulkan => {
                let (runtime, presenter) = renderer::VulkanRuntime::for_window(
                    renderer::RuntimeConfig {
                        execution_slots: 6,
                        resource_capacity: 64,
                        staging_bytes: 32 * 1024 * 1024,
                        staging_ranges: 64,
                        max_shader_bindings: 8,
                    },
                    window,
                    presentation,
                )?;
                Self::Vulkan(Box::new(Vulkan { runtime, presenter }))
            }
            #[cfg(feature = "d3d12")]
            Api::D3d12 => {
                let (runtime, presenter) = renderer::D3D12Runtime::for_window(
                    renderer::D3D12RuntimeConfig {
                        capacity: renderer::D3D12FrameCapacity {
                            executions: D3D12_EXECUTIONS,
                            results: 3,
                            resources: 16,
                            resource_capacity: 64,
                            operations: 16,
                            provenance_segments_per_resource: 16,
                            index_prefixes_per_resource: 16,
                            initialized_ranges_per_resource: 8,
                            range_snapshots: 64,
                            buffers: 2,
                            buffer_bytes: 512,
                            upload_bytes: 512,
                            readback_bytes: D3D12_READBACK_BYTES,
                            scratch_bytes: 512,
                        },
                        ..Default::default()
                    },
                    window,
                    presentation,
                )?;
                Self::D3d12(Box::new(D3d12 { runtime, presenter }))
            }
            #[allow(unreachable_patterns)]
            _ => return Err(format!("{} backend was not compiled", api.name()).into()),
        };
        backend.set_profiling(profiling)?;
        Ok(backend)
    }

    pub fn set_profiling(&mut self, enabled: bool) -> Result<()> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => {
                value.runtime.set_profiling(enabled)?;
                value.presenter.set_profiling(enabled)?;
            }
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => {
                value.runtime.set_profiling(enabled)?;
                value.presenter.set_profiling(enabled)?;
            }
        }
        Ok(())
    }

    pub fn monitor(&self) -> Monitor {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => Monitor::Vulkan(value.runtime.validation_monitor()),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => Monitor::D3d12(value.runtime.validation_monitor()),
        }
    }

    pub fn device(&self) -> serde_json::Value {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => {
                let info = value.runtime.device_info();
                serde_json::json!({"name":info.name,"luid":info.device_luid,
                    "driver":info.driver_version_raw,"api":info.api_version})
            }
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => {
                let info = value.runtime.device_info();
                serde_json::json!({"name":info.name,"luid":info.luid,
                    "driver":info.driver_version,"shader_model":info.shader_model})
            }
        }
    }

    pub fn prepare_resources(&mut self, extent: (u32, u32)) -> Result<()> {
        let color = ImageDesc::rgba8(extent.0, extent.1);
        let depth = ImageDesc::depth32(extent.0, extent.1);
        let programs = [ManagedRasterProgram::mesh()];
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => {
                value.runtime.trim_idle_resources();
                value.runtime.prepare_working_set(
                    renderer::FrameCapacity {
                        graph: scene::capacity(),
                        work_slots: 3,
                        result_slots: 3,
                        metadata_slots: 6,
                        range_snapshots: 64,
                        initialized_ranges_per_resource: 8,
                        provenance_segments_per_resource: 16,
                        index_prefixes_per_resource: 16,
                    },
                    &[
                        renderer::ResourceClass::Buffer {
                            desc: BufferDesc {
                                size: scene::VERTEX_BYTES,
                            },
                            count: 1,
                        },
                        renderer::ResourceClass::Buffer {
                            desc: BufferDesc {
                                size: scene::INDEX_BYTES,
                            },
                            count: 1,
                        },
                        renderer::ResourceClass::Image {
                            desc: color,
                            count: 9,
                        },
                        renderer::ResourceClass::Image {
                            desc: depth,
                            count: 3,
                        },
                    ],
                )?;
                value.runtime.prepare_raster_programs(&programs)?;
            }
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.runtime.prepare_graph_working_set(
                None,
                &[
                    renderer::D3D12ImageClass {
                        desc: color,
                        count: 3 * D3D12_EXECUTIONS,
                    },
                    renderer::D3D12ImageClass {
                        desc: depth,
                        count: D3D12_EXECUTIONS,
                    },
                ],
                &programs,
                &[],
            )?,
        }
        Ok(())
    }

    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<Pending> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => Pending::Vulkan(value.runtime.submit(graph)?),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => Pending::D3d12(value.runtime.submit(graph)?),
        })
    }
    pub fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.runtime.refresh_buffer(key)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.runtime.refresh_buffer(key)?,
        })
    }
    pub fn delete(&mut self, key: ResourceKey) -> Result<()> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.runtime.delete(key)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.runtime.delete(key)?,
        }
        Ok(())
    }
    pub fn present(&mut self, image: &ImportImage) -> Result<PresentOutcome> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.runtime.present(&mut value.presenter, image)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.runtime.present(&mut value.presenter, image)?,
        })
    }
    pub fn prepare(&mut self) -> Result<Option<PresentOutcome>> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.prepare()?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.prepare()?,
        })
    }
    pub fn extent(&self) -> Option<(u32, u32)> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.extent(),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.extent(),
        }
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.resize(width, height),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.resize(width, height),
        }
    }
    pub fn set_vsync(&mut self, enabled: bool) -> Result<()> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.set_vsync(enabled)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.set_vsync(enabled)?,
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Result<()> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => {
                value.presenter.poll()?;
            }
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => {
                value.runtime.poll()?;
                value.presenter.poll()?;
            }
        }
        Ok(())
    }
    pub fn take_profile(&mut self) -> Option<PresentationMetrics> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.take_profile(),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.take_profile(),
        }
    }
    pub fn presentation_idle(&mut self) -> Result<bool> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.wait_idle(Duration::ZERO)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.wait_idle(Duration::ZERO)?,
        })
    }
    pub fn close(&mut self, timeout: Duration) -> Result<bool> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.presenter.close(timeout)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.presenter.close(timeout)?,
        })
    }
}

impl Pending {
    pub fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.export_buffer(handle)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.export_buffer(handle)?,
        })
    }
    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.export_image(handle)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.export_image(handle)?,
        })
    }
    pub fn wait(&mut self, timeout: Duration) -> Result<Option<Completed>> {
        Ok(match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.wait(timeout)?.map(Completed::Vulkan),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.wait(timeout)?.map(Completed::D3d12),
        })
    }
}
impl Completed {
    pub fn readback_into(&mut self, pass: PassHandle, bytes: &mut [u8]) -> Result<()> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.readback_into(pass, bytes)?,
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.readback_into(pass, bytes)?,
        }
        Ok(())
    }
    pub fn metrics(&self) -> Option<SubmissionMetrics> {
        match self {
            #[cfg(feature = "vulkan")]
            Self::Vulkan(value) => value.metrics().copied(),
            #[cfg(feature = "d3d12")]
            Self::D3d12(value) => value.metrics().copied(),
        }
    }
}
impl Monitor {
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

pub fn capacity_recovery(error: &(dyn Error + 'static)) -> Option<CapacityRecovery> {
    #[cfg(feature = "vulkan")]
    if let Some(value) = error.downcast_ref::<renderer::RuntimeError>() {
        return value.capacity().map(|capacity| capacity.recovery);
    }
    #[cfg(feature = "d3d12")]
    if let Some(value) = error.downcast_ref::<renderer::D3D12RuntimeError>() {
        return value.capacity().map(|capacity| capacity.recovery);
    }
    if let Some(value) = error.downcast_ref::<renderer::graph::GraphError>() {
        use renderer::graph::GraphCapacityKind;
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
