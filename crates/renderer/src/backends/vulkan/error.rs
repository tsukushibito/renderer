//! Backend execution errors, preserving native causes without exposing native types.

use std::fmt;

use ash::vk;

use super::buffer::BufferError;
use super::device::{DeviceInitError, DeviceState};
use super::instance::{InitError, PrerequisiteFailure};
use super::memory::AllocatorInitError;
use super::transfer::TransferError;
use super::transfer::staging::StagingError;

/// Stable, native-free categories for handling an execution failure.
/// Native allocation OOM is `Native`; bounded Renderer pool exhaustion is
/// `CapacityExhausted`, so applications can distinguish these recovery choices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeErrorKind {
    InvalidInput,
    Unsupported,
    CapacityExhausted,
    StaleResource,
    WrongOwner,
    DeviceLost,
    DeviceUnavailable,
    Native,
}

/// Context and its original error chain, with native API types erased.
#[derive(Debug)]
pub struct RuntimeError {
    pub message: String,
    kind: RuntimeErrorKind,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl RuntimeError {
    pub(super) fn is_staging_exhausted(&self) -> bool {
        matches!(
            self.source
                .as_deref()
                .and_then(|cause| cause.downcast_ref::<StagingError>()),
            Some(StagingError::CapacityExhausted { .. })
        )
    }
    pub fn kind(&self) -> RuntimeErrorKind {
        self.kind
    }

    pub(super) fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    fn classified(kind: RuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            source: None,
        }
    }
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::InvalidInput, message)
    }
    pub(super) fn unsupported(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::Unsupported, message)
    }
    pub(super) fn capacity(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::CapacityExhausted, message)
    }
    pub(super) fn stale(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::StaleResource, message)
    }
    pub(super) fn wrong_owner(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::WrongOwner, message)
    }
    pub(super) fn with_source<E: std::error::Error + Send + Sync + 'static>(
        kind: RuntimeErrorKind,
        source: E,
    ) -> Self {
        Self {
            message: source.to_string(),
            kind,
            source: Some(Box::new(source)),
        }
    }
    pub(super) fn device_unavailable(state: DeviceState) -> Self {
        Self::classified(
            device_kind(state),
            format!("runtime Device is {state:?}; normal use is stopped"),
        )
    }
    pub(super) fn native(
        operation: impl Into<String>,
        device: impl Into<String>,
        result: vk::Result,
    ) -> Self {
        Self::with_source(
            native_kind(result),
            NativeRuntimeCause {
                operation: operation.into(),
                device: device.into(),
                result,
            },
        )
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug)]
struct NativeRuntimeCause {
    operation: String,
    device: String,
    result: vk::Result,
}
impl fmt::Display for NativeRuntimeCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on {} failed: {:?}",
            self.operation, self.device, self.result
        )
    }
}
impl std::error::Error for NativeRuntimeCause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.result)
    }
}

fn native_kind(result: vk::Result) -> RuntimeErrorKind {
    match result {
        vk::Result::ERROR_DEVICE_LOST => RuntimeErrorKind::DeviceLost,
        vk::Result::ERROR_FORMAT_NOT_SUPPORTED
        | vk::Result::ERROR_FEATURE_NOT_PRESENT
        | vk::Result::ERROR_EXTENSION_NOT_PRESENT
        | vk::Result::ERROR_LAYER_NOT_PRESENT
        | vk::Result::ERROR_INCOMPATIBLE_DRIVER => RuntimeErrorKind::Unsupported,
        _ => RuntimeErrorKind::Native,
    }
}
fn device_kind(state: DeviceState) -> RuntimeErrorKind {
    if state == DeviceState::Lost {
        RuntimeErrorKind::DeviceLost
    } else {
        RuntimeErrorKind::DeviceUnavailable
    }
}
fn buffer_error_kind(error: &BufferError) -> RuntimeErrorKind {
    match error {
        BufferError::DeviceUnavailable(state) => device_kind(*state),
        BufferError::Vulkan { result, .. } => native_kind(*result),
        _ => RuntimeErrorKind::InvalidInput,
    }
}
impl From<TransferError> for RuntimeError {
    fn from(error: TransferError) -> Self {
        let kind = match &error {
            TransferError::CapacityExhausted { .. } => RuntimeErrorKind::CapacityExhausted,
            TransferError::DeviceUnavailable(state) => device_kind(*state),
            TransferError::Vulkan { result, .. } => native_kind(*result),
            _ => RuntimeErrorKind::InvalidInput,
        };
        Self::with_source(kind, error)
    }
}
impl From<StagingError> for RuntimeError {
    fn from(error: StagingError) -> Self {
        let kind = match &error {
            StagingError::CapacityExhausted { .. } => RuntimeErrorKind::CapacityExhausted,
            StagingError::DeviceUnavailable(state) => device_kind(*state),
            StagingError::Buffer(error) => buffer_error_kind(error),
            StagingError::Vulkan { result, .. } => native_kind(*result),
            _ => RuntimeErrorKind::InvalidInput,
        };
        Self::with_source(kind, error)
    }
}
impl From<InitError> for RuntimeError {
    fn from(error: InitError) -> Self {
        let kind = match &error {
            InitError::Loader(_) => RuntimeErrorKind::DeviceUnavailable,
            InitError::Prerequisites(failures) => failures
                .iter()
                .find_map(|failure| match failure {
                    PrerequisiteFailure::Query { result, .. } => Some(native_kind(*result)),
                    _ => None,
                })
                .unwrap_or(RuntimeErrorKind::Unsupported),
            #[cfg(not(feature = "vulkan-diagnostics"))]
            InitError::DiagnosticsNotCompiled => RuntimeErrorKind::Unsupported,
            #[cfg(feature = "vulkan-diagnostics")]
            InitError::MissingEntryPoint(_) => RuntimeErrorKind::Unsupported,
            InitError::Vulkan { result, .. } => native_kind(*result),
        };
        Self::with_source(kind, error)
    }
}
impl From<DeviceInitError> for RuntimeError {
    fn from(error: DeviceInitError) -> Self {
        let kind = match &error {
            DeviceInitError::NoSuitableDevice(_) => RuntimeErrorKind::Unsupported,
            DeviceInitError::Vulkan { result, .. } => native_kind(*result),
        };
        Self::with_source(kind, error)
    }
}
impl From<AllocatorInitError> for RuntimeError {
    fn from(error: AllocatorInitError) -> Self {
        // The allocator's structured source is its VkResult; preserve both
        // layers rather than replacing its operation with a formatted string.
        let kind = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<vk::Result>())
            .map_or(RuntimeErrorKind::Native, |result| native_kind(*result));
        Self::with_source(kind, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn transfer_native_failures_preserve_operation_device_result_and_classification() {
        for (result, expected) in [
            (
                vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
                RuntimeErrorKind::Native,
            ),
            (
                vk::Result::ERROR_OUT_OF_HOST_MEMORY,
                RuntimeErrorKind::Native,
            ),
            (vk::Result::ERROR_DEVICE_LOST, RuntimeErrorKind::DeviceLost),
        ] {
            let error = RuntimeError::from(TransferError::Vulkan {
                operation: "vkQueueSubmit2",
                device: "test adapter".to_owned(),
                result,
            });
            assert_eq!(error.kind(), expected);
            assert!(error.message.contains("vkQueueSubmit2"));
            assert!(error.message.contains("test adapter"));
            let source = error.source().unwrap();
            assert!(source.downcast_ref::<TransferError>().is_some());
            assert_eq!(
                source.source().unwrap().downcast_ref::<vk::Result>(),
                Some(&result)
            );
        }
    }

    #[test]
    fn native_helper_keeps_original_result_and_support_failure_category() {
        let error = RuntimeError::native(
            "vkCreateImageView",
            "test adapter",
            vk::Result::ERROR_DEVICE_LOST,
        );
        assert_eq!(error.kind(), RuntimeErrorKind::DeviceLost);
        assert_eq!(
            error
                .source()
                .unwrap()
                .source()
                .unwrap()
                .downcast_ref::<vk::Result>(),
            Some(&vk::Result::ERROR_DEVICE_LOST)
        );
        let error = RuntimeError::native(
            "vkGetPhysicalDeviceImageFormatProperties2",
            "test adapter",
            vk::Result::ERROR_FORMAT_NOT_SUPPORTED,
        );
        assert_eq!(error.kind(), RuntimeErrorKind::Unsupported);
        assert!(
            error
                .message
                .contains("vkGetPhysicalDeviceImageFormatProperties2")
        );
    }

    #[test]
    fn bounded_capacity_and_terminal_device_states_have_distinct_kinds() {
        assert_eq!(
            RuntimeError::from(TransferError::CapacityExhausted { capacity: 2 }).kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            RuntimeError::from(StagingError::CapacityExhausted {
                capacity: 16,
                requested: 12,
                reserved: 8
            })
            .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            RuntimeError::from(TransferError::DeviceUnavailable(DeviceState::Lost)).kind(),
            RuntimeErrorKind::DeviceLost
        );
        assert_eq!(
            RuntimeError::from(StagingError::DeviceUnavailable(DeviceState::Stopped)).kind(),
            RuntimeErrorKind::DeviceUnavailable
        );
        assert_eq!(
            RuntimeError::from(StagingError::Buffer(BufferError::DeviceUnavailable(
                DeviceState::Lost
            )))
            .kind(),
            RuntimeErrorKind::DeviceLost
        );
        // Words in a diagnostic never determine its category.
        assert_eq!(
            RuntimeError::invalid("capacity unsupported lost").kind(),
            RuntimeErrorKind::InvalidInput
        );
        assert_eq!(
            RuntimeError::stale("snapshot changed").kind(),
            RuntimeErrorKind::StaleResource
        );
        assert_eq!(
            RuntimeError::wrong_owner("different runtime").kind(),
            RuntimeErrorKind::WrongOwner
        );
    }
}
