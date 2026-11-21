//! D3D12 single-queue native ownership. Native types stay backend-private.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use std::sync::Arc;
use std::{borrow::Cow, fmt};

mod compute;
mod device;
mod diagnostics;
mod frame;
mod frames;
mod graph;
mod memory;
mod metrics;
#[cfg(feature = "presentation")]
mod presentation;
mod program;
mod raster;
mod raster_clear;
mod raster_storage;
mod registry;
pub use registry::D3D12ExternalDevice;
mod resources;
mod sampler;
pub use device::D3D12DeviceInfo;
use device::DeviceOwner;
pub use diagnostics::{D3D12ValidationMessage, D3D12ValidationMonitor, D3D12ValidationReport};
pub use frame::{D3D12BufferClass, D3D12FrameCapacity};
pub(crate) use frames::{QueuedFrameCompletion, QueuedFrameSubmission};
pub use graph::{D3D12PreparedGraph, D3D12Submission, D3D12SubmissionResult};
pub use memory::{
    D3D12CommittedHeapStats, D3D12CommittedMemoryStats, D3D12MemoryStats, D3D12StagingMemoryStats,
    D3D12VideoMemoryInfo,
};
#[cfg(feature = "presentation")]
pub use presentation::D3D12Presenter;
pub use raster_storage::D3D12ImageClass;

/// Native validation is independent of optimization and Renderer diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum D3D12ValidationMode {
    Disabled,
    /// Require the Debug Layer and GPU-Based Validation before Device creation.
    Required,
}

impl Default for D3D12ValidationMode {
    fn default() -> Self {
        if cfg!(feature = "development-validation") {
            Self::Required
        } else {
            Self::Disabled
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct D3D12RuntimeConfig {
    pub validation: D3D12ValidationMode,
    /// Explicitly prepared execution, transfer and result capacities.
    pub capacity: D3D12FrameCapacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum D3D12RuntimeErrorKind {
    InvalidInput,
    Unsupported,
    CapacityExhausted,
    WrongOwner,
    StaleResource,
    DeviceRemoved,
    DeviceUnavailable,
    Native,
}

/// Native operation context and the original HRESULT remain in the error chain.
#[derive(Debug)]
pub struct D3D12RuntimeError {
    pub message: Cow<'static, str>,
    kind: D3D12RuntimeErrorKind,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    capacity: Option<RuntimeCapacityError>,
}

impl D3D12RuntimeError {
    pub fn kind(&self) -> D3D12RuntimeErrorKind {
        self.kind
    }
    /// Identifies the prepared resource and the action required before retrying.
    /// These scalar details do not allocate or retain runtime resources.
    pub fn capacity(&self) -> Option<RuntimeCapacityError> {
        self.capacity
    }
    pub(super) fn classified(
        kind: D3D12RuntimeErrorKind,
        message: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            message: message.into(),
            kind,
            source: None,
            capacity: None,
        }
    }
    pub(super) fn invalid(message: impl Into<Cow<'static, str>>) -> Self {
        Self::classified(D3D12RuntimeErrorKind::InvalidInput, message)
    }
    pub(super) fn unsupported(message: impl Into<Cow<'static, str>>) -> Self {
        Self::classified(D3D12RuntimeErrorKind::Unsupported, message)
    }
    pub(super) fn capacity_with(
        message: impl Into<Cow<'static, str>>,
        capacity: RuntimeCapacityError,
    ) -> Self {
        Self {
            capacity: Some(capacity),
            ..Self::classified(D3D12RuntimeErrorKind::CapacityExhausted, message)
        }
    }
    pub(super) fn wrong_owner(message: impl Into<Cow<'static, str>>) -> Self {
        Self::classified(D3D12RuntimeErrorKind::WrongOwner, message)
    }
    pub(super) fn native(
        operation: &'static str,
        device: &str,
        error: windows_core::Error,
    ) -> Self {
        let code = error.code().0 as u32;
        let kind = if matches!(code, 0x887a0005 | 0x887a0006 | 0x887a0007 | 0x887a0020) {
            D3D12RuntimeErrorKind::DeviceRemoved
        } else {
            D3D12RuntimeErrorKind::Native
        };
        let cause = NativeCause {
            operation,
            device: device.to_owned(),
            error,
        };
        Self {
            message: cause.to_string().into(),
            kind,
            source: Some(Box::new(cause)),
            capacity: None,
        }
    }
}
impl fmt::Display for D3D12RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(capacity) = self.capacity {
            write!(f, " ({capacity})")?;
        }
        Ok(())
    }
}
impl std::error::Error for D3D12RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}
#[derive(Debug)]
struct NativeCause {
    operation: &'static str,
    device: String,
    error: windows_core::Error,
}
impl fmt::Display for NativeCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on {}: {} (HRESULT {:#010x})",
            self.operation,
            self.device,
            self.error,
            self.error.code().0 as u32
        )
    }
}
impl std::error::Error for NativeCause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// A hardware D3D12 Device and one DIRECT queue.
///
/// The first runtime fixes native validation for this process. Subsequent clients
/// share its native owner while it is live; incompatible validation requests are
/// rejected before touching the process-wide debug controls. When requesting
/// validation, create this runtime before creating D3D12 devices through other
/// libraries: enabling native debug controls after any Device exists removes it.
pub struct D3D12Runtime {
    // Drop pending frames before the last runtime pool owner. Their existing
    // Drop path detaches any unresolved work for conservative pool teardown.
    frames: frames::QueuedFrames,
    device: Arc<DeviceOwner>,
    pool: Arc<frame::FramePool>,
}
impl D3D12Runtime {
    pub fn new(config: D3D12RuntimeConfig) -> Result<Self, D3D12RuntimeError> {
        let device = DeviceOwner::new(config.validation)?;
        let pool = frame::FramePool::new(Arc::clone(&device), config.capacity)?;
        Self::from_pool(device, pool)
    }
    /// Creates a runtime with exact global physical buffer/image classes and
    /// prepared pipelines, without first allocating a uniform buffer pool.
    /// `capacity.buffers` limits per-execution metadata, including imports;
    /// class counts reserve physical instances once across all executions.
    pub fn new_with_working_set(
        config: D3D12RuntimeConfig,
        buffers: &[D3D12BufferClass],
        images: &[D3D12ImageClass],
        programs: &[crate::program::RasterPipelineRequest],
        compute_programs: &[crate::program::ManagedProgram],
    ) -> Result<Self, D3D12RuntimeError> {
        let device = DeviceOwner::new(config.validation)?;
        Self::with_device(
            device,
            config.capacity,
            buffers,
            images,
            programs,
            compute_programs,
        )
    }

    pub(crate) fn from_queue(
        queue: &windows_core::IUnknown,
        config: D3D12RuntimeConfig,
        buffers: &[D3D12BufferClass],
        images: &[D3D12ImageClass],
        programs: &[crate::program::RasterPipelineRequest],
        compute_programs: &[crate::program::ManagedProgram],
    ) -> Result<Self, D3D12RuntimeError> {
        let device = DeviceOwner::adopt_queue(queue, config.validation)?;
        Self::with_device(
            device,
            config.capacity,
            buffers,
            images,
            programs,
            compute_programs,
        )
    }

    fn with_device(
        device: Arc<DeviceOwner>,
        capacity: D3D12FrameCapacity,
        buffers: &[D3D12BufferClass],
        images: &[D3D12ImageClass],
        programs: &[crate::program::RasterPipelineRequest],
        compute_programs: &[crate::program::ManagedProgram],
    ) -> Result<Self, D3D12RuntimeError> {
        let pool = frame::FramePool::with_graph(
            Arc::clone(&device),
            capacity,
            buffers,
            images,
            &[],
            programs,
            compute_programs,
        )?;
        Self::from_pool(device, pool)
    }
    fn from_pool(
        device: Arc<DeviceOwner>,
        pool: Arc<frame::FramePool>,
    ) -> Result<Self, D3D12RuntimeError> {
        let frames = frames::QueuedFrames::new(pool.capacity().executions)?;
        Ok(Self {
            frames,
            device,
            pool,
        })
    }
    /// Reserves exact image classes, descriptor arenas and immutable Raster and
    /// Compute pipelines at an explicit change boundary. All execution and result
    /// leases and completed submissions with exports must have returned.
    /// Registered resources retain their keys, contents, and import snapshots.
    /// Registered images get view slots
    /// even when omitted from `images`. Ordinary Graph preparation never grows storage.
    /// `buffers: None` retains the current prepared buffer-class budget;
    /// `Some(classes)` replaces it, including `Some(&[])` for no free buffers.
    /// Registered resources remain in addition to this free working set.
    /// Physical class counts are global, independent of execution-slot count.
    /// Raster requests select the exact Program and attachment sample count.
    /// Empty image and Program lists are allowed. Image descriptors reserve
    /// `operations` Raster slots plus `operations * max_compute_image_bindings`
    /// Compute slots; the entire shader-visible heap must fit its native limit.
    pub fn prepare_graph_working_set(
        &mut self,
        buffers: Option<&[D3D12BufferClass]>,
        images: &[D3D12ImageClass],
        programs: &[crate::program::RasterPipelineRequest],
        compute_programs: &[crate::program::ManagedProgram],
    ) -> Result<(), D3D12RuntimeError> {
        self.prepare_graph_with_capacity(
            self.pool.capacity(),
            buffers,
            images,
            programs,
            compute_programs,
        )
    }

    pub(crate) fn prepare_graph_with_capacity(
        &mut self,
        capacity: D3D12FrameCapacity,
        buffers: Option<&[D3D12BufferClass]>,
        images: &[D3D12ImageClass],
        programs: &[crate::program::RasterPipelineRequest],
        compute_programs: &[crate::program::ManagedProgram],
    ) -> Result<(), D3D12RuntimeError> {
        self.device.ensure_active()?;
        self.pool.reap()?;
        if !self.pool.is_idle() {
            return Err(D3D12RuntimeError::capacity_with(
                "Graph working set cannot change while execution or result leases remain",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        let mut registry = self.pool.registry.lock().unwrap_or_else(|e| e.into_inner());
        if !registry.metadata_is_idle() {
            return Err(D3D12RuntimeError::capacity_with(
                "return submission and result metadata before changing the working set",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ExportMetadata,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        let registered_images = registry.registered_images()?;
        let mut pool = frame::FramePool::with_graph(
            Arc::clone(&self.device),
            capacity,
            buffers.unwrap_or_else(|| self.pool.buffer_classes()),
            images,
            &registered_images,
            programs,
            compute_programs,
        )?;
        if self.pool.profiling_enabled() {
            pool.set_profiling(true)?;
        }
        let replacement = Arc::get_mut(&mut pool)
            .expect("new unshared pool")
            .registry
            .get_mut()
            .unwrap_or_else(|e| e.into_inner());
        registry.replace_prepared(replacement)?;
        // Preserve the canonical registry, including its owner, registered native
        // resources and prepared token anchors, only after all preparation succeeds.
        std::mem::swap(&mut *registry, replacement);
        drop(registry);
        self.pool = pool;
        Ok(())
    }
    pub fn device_info(&self) -> D3D12DeviceInfo {
        self.device.info.clone()
    }
    pub fn validation_monitor(&self) -> D3D12ValidationMonitor {
        self.device.monitor.clone()
    }
    /// Retires detached submissions without waiting or changing capacity.
    pub fn poll(&mut self) -> Result<usize, D3D12RuntimeError> {
        self.pool.reap()
    }
    /// Release this client's native ownership. Escaped work and other clients
    /// retain their ownership; observe the independent monitor after dropping
    /// them to confirm `teardown_checked` through final native destruction.
    pub fn shutdown(self) -> Result<D3D12ValidationReport, D3D12RuntimeError> {
        let monitor = self.validation_monitor();
        drop(self);
        Ok(monitor.snapshot())
    }
}
impl fmt::Debug for D3D12Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Runtime")
            .field("device", &self.device.info)
            .finish_non_exhaustive()
    }
}

// Generated upstream code is reviewed as generated FFI, not hand-authored
// renderer code. Keep lint exceptions confined to this single private module.
#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unsafe_op_in_unsafe_fn,
    clippy::all,
    clippy::undocumented_unsafe_blocks
)]
#[rustfmt::skip]
mod bindings;

#[cfg(test)]
mod preparation;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn native_failure_preserves_operation_hresult_and_original_cause() {
        let error = D3D12RuntimeError::native(
            "CreateCommandQueue",
            "test adapter",
            windows_core::Error::from_hresult(windows_core::HRESULT(0x8007000e_u32 as i32)),
        );
        assert_eq!(error.kind(), D3D12RuntimeErrorKind::Native);
        assert!(
            error
                .to_string()
                .contains("CreateCommandQueue on test adapter")
        );
        assert!(error.to_string().contains("0x8007000e"));
        let original = error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<windows_core::Error>()
            .unwrap();
        assert_eq!(original.code().0 as u32, 0x8007000e);
        let removed = D3D12RuntimeError::native(
            "Signal",
            "test adapter",
            windows_core::Error::from_hresult(windows_core::HRESULT(0x887a0005_u32 as i32)),
        );
        assert_eq!(removed.kind(), D3D12RuntimeErrorKind::DeviceRemoved);
    }

    #[test]
    fn validation_default_is_independent_of_optimization() {
        assert_eq!(
            D3D12ValidationMode::default(),
            if cfg!(feature = "development-validation") {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            }
        );
    }

    #[cfg(not(feature = "d3d12-diagnostics"))]
    #[test]
    fn required_without_native_diagnostics_rejects_before_device_creation() {
        let error = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Required,
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(error.kind(), D3D12RuntimeErrorKind::Unsupported);
    }

    #[cfg(feature = "d3d12-diagnostics")]
    #[test]
    #[ignore = "explicit D3D12 hardware runtime; requires Debug Layer and GBV"]
    fn d3d12_runtime_hardware() {
        let config = D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Required,
            ..Default::default()
        };
        let first = D3D12Runtime::new(config).expect("hardware Device and DIRECT queue");
        let info = first.device_info();
        assert!(!info.name.is_empty());
        assert_ne!(info.vendor_id, 0);
        let monitor = first.validation_monitor();
        let active = monitor.snapshot();
        assert!(
            active.debug_layer_enabled && active.gpu_based_validation_enabled,
            "{active:?}"
        );
        assert!(active.is_clean(), "{active:?}");
        assert!(!active.teardown_checked);
        let second = D3D12Runtime::new(config).expect("shared native singleton");
        assert!(Arc::ptr_eq(&first.device, &second.device));
        let incompatible = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Disabled,
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(incompatible.kind(), D3D12RuntimeErrorKind::Unsupported);
        assert!(!first.shutdown().unwrap().teardown_checked);
        let finished = second.shutdown().expect("explicit final owner shutdown");
        assert!(finished.teardown_checked, "{finished:?}");
        assert!(finished.is_clean(), "{finished:?}");
        assert!(monitor.snapshot().teardown_checked);
        // The debug configuration remains process-wide after final Release.
        // Recreate in this same process to exercise the non-first initialization
        // path, removed callback registration and retained global DXGI history.
        let recreated = D3D12Runtime::new(config).expect("same-mode native owner recreation");
        let recreated_monitor = recreated.validation_monitor();
        assert_eq!(recreated.device_info().luid, info.luid);
        assert!(!recreated_monitor.snapshot().teardown_checked);
        let recreated_finished = recreated.shutdown().expect("recreated owner shutdown");
        assert!(
            recreated_finished.debug_layer_enabled
                && recreated_finished.gpu_based_validation_enabled
        );
        assert!(
            recreated_finished.teardown_checked && recreated_finished.is_clean(),
            "{recreated_finished:?}"
        );
        // The first monitor neither retains the Device nor starts collecting a
        // later owner's messages after its own callback identity was removed.
        assert_eq!(monitor.snapshot().messages.len(), finished.messages.len());
        println!("D3D12_RUNTIME_INFO {info:?}");
        println!("D3D12_RUNTIME_VALIDATION {finished:?}");
    }

    #[cfg(not(feature = "d3d12-diagnostics"))]
    #[test]
    #[ignore = "explicit D3D12 hardware shipping runtime without diagnostics"]
    fn d3d12_runtime_shipping_hardware() {
        let runtime = D3D12Runtime::new(D3D12RuntimeConfig::default())
            .expect("shipping hardware Device and DIRECT queue");
        let owner = Arc::downgrade(&runtime.device);
        let info = runtime.device_info();
        let monitor = runtime.validation_monitor();
        runtime.shutdown().unwrap();
        assert!(owner.upgrade().is_none());
        let report = monitor.snapshot();
        assert!(
            !report.debug_layer_enabled
                && !report.gpu_based_validation_enabled
                && !report.teardown_checked
        );
        assert!(report.is_clean());
        println!("D3D12_SHIPPING_RUNTIME_INFO {info:?}");
    }
}
