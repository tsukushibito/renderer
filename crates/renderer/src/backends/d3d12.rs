//! D3D12 single-queue native ownership. Native types stay backend-private.

use std::fmt;
use std::sync::Arc;

mod device;
mod diagnostics;
pub use device::D3D12DeviceInfo;
use device::DeviceOwner;
pub use diagnostics::{D3D12ValidationMessage, D3D12ValidationMonitor, D3D12ValidationReport};

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum D3D12RuntimeErrorKind {
    InvalidInput,
    Unsupported,
    CapacityExhausted,
    WrongOwner,
    DeviceRemoved,
    DeviceUnavailable,
    Native,
}

/// Native operation context and the original HRESULT remain in the error chain.
#[derive(Debug)]
pub struct D3D12RuntimeError {
    pub message: String,
    kind: D3D12RuntimeErrorKind,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl D3D12RuntimeError {
    pub fn kind(&self) -> D3D12RuntimeErrorKind {
        self.kind
    }
    pub(super) fn classified(kind: D3D12RuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            source: None,
        }
    }
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::classified(D3D12RuntimeErrorKind::InvalidInput, message)
    }
    pub(super) fn unsupported(message: impl Into<String>) -> Self {
        Self::classified(D3D12RuntimeErrorKind::Unsupported, message)
    }
    pub(super) fn capacity(message: impl Into<String>) -> Self {
        Self::classified(D3D12RuntimeErrorKind::CapacityExhausted, message)
    }
    pub(super) fn wrong_owner(message: impl Into<String>) -> Self {
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
            message: cause.to_string(),
            kind,
            source: Some(Box::new(cause)),
        }
    }
}
impl fmt::Display for D3D12RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
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
    device: Arc<DeviceOwner>,
}
impl D3D12Runtime {
    pub fn new(config: D3D12RuntimeConfig) -> Result<Self, D3D12RuntimeError> {
        Ok(Self {
            device: DeviceOwner::new(config.validation)?,
        })
    }
    pub fn device_info(&self) -> D3D12DeviceInfo {
        self.device.info.clone()
    }
    pub fn validation_monitor(&self) -> D3D12ValidationMonitor {
        self.device.monitor.clone()
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
        })
        .unwrap_err();
        assert_eq!(incompatible.kind(), D3D12RuntimeErrorKind::Unsupported);
        assert!(!first.shutdown().unwrap().teardown_checked);
        let finished = second.shutdown().expect("explicit final owner shutdown");
        assert!(finished.teardown_checked, "{finished:?}");
        assert!(finished.is_clean(), "{finished:?}");
        assert!(monitor.snapshot().teardown_checked);
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
