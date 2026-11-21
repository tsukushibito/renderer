use std::{error::Error, fmt};

use crate::capacity::RuntimeCapacityError;

/// Native-independent classification of a runtime failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExecutionErrorKind {
    InvalidInput,
    Unsupported,
    CapacityExhausted,
    WrongOwner,
    StaleResource,
    /// The host can close this presenter and attach a fresh window to the runtime.
    SurfaceLost,
    DeviceLost,
    DeviceUnavailable,
    Native,
}

/// A runtime failure with the original backend error available as its source.
#[derive(Debug)]
pub struct ExecutionError(ExecutionCause);

#[derive(Debug)]
enum ExecutionCause {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::vulkan::RuntimeError),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(crate::d3d12::D3D12RuntimeError),
    Local {
        kind: ExecutionErrorKind,
        message: &'static str,
        capacity: Option<RuntimeCapacityError>,
    },
}

impl ExecutionError {
    pub fn kind(&self) -> ExecutionErrorKind {
        use ExecutionErrorKind as Kind;
        match &self.0 {
            #[cfg(feature = "vulkan")]
            ExecutionCause::Vulkan(error) => {
                use crate::vulkan::RuntimeErrorKind as Native;
                match error.kind() {
                    Native::InvalidInput => Kind::InvalidInput,
                    Native::Unsupported => Kind::Unsupported,
                    Native::CapacityExhausted => Kind::CapacityExhausted,
                    Native::WrongOwner => Kind::WrongOwner,
                    Native::StaleResource => Kind::StaleResource,
                    Native::SurfaceLost => Kind::SurfaceLost,
                    Native::DeviceLost => Kind::DeviceLost,
                    Native::DeviceUnavailable => Kind::DeviceUnavailable,
                    Native::Native => Kind::Native,
                }
            }
            #[cfg(all(feature = "d3d12", windows))]
            ExecutionCause::D3D12(error) => {
                use crate::d3d12::D3D12RuntimeErrorKind as Native;
                match error.kind() {
                    Native::InvalidInput => Kind::InvalidInput,
                    Native::Unsupported => Kind::Unsupported,
                    Native::CapacityExhausted => Kind::CapacityExhausted,
                    Native::WrongOwner => Kind::WrongOwner,
                    Native::StaleResource => Kind::StaleResource,
                    Native::DeviceRemoved => Kind::DeviceLost,
                    Native::DeviceUnavailable => Kind::DeviceUnavailable,
                    Native::Native => Kind::Native,
                }
            }
            ExecutionCause::Local { kind, .. } => *kind,
        }
    }

    /// The exhausted bound and recovery action, when applicable.
    pub fn capacity(&self) -> Option<RuntimeCapacityError> {
        match &self.0 {
            #[cfg(feature = "vulkan")]
            ExecutionCause::Vulkan(error) => error.capacity(),
            #[cfg(all(feature = "d3d12", windows))]
            ExecutionCause::D3D12(error) => error.capacity(),
            ExecutionCause::Local { capacity, .. } => *capacity,
        }
    }

    pub(crate) fn invalid(message: &'static str) -> Self {
        Self::local(ExecutionErrorKind::InvalidInput, message, None)
    }

    #[cfg(all(
        windows,
        feature = "vulkan",
        feature = "d3d12",
        feature = "presentation"
    ))]
    pub(crate) fn wrong_owner(message: &'static str) -> Self {
        Self::local(ExecutionErrorKind::WrongOwner, message, None)
    }

    pub(crate) fn with_capacity(message: &'static str, details: RuntimeCapacityError) -> Self {
        Self::local(
            ExecutionErrorKind::CapacityExhausted,
            message,
            Some(details),
        )
    }

    fn local(
        kind: ExecutionErrorKind,
        message: &'static str,
        capacity: Option<RuntimeCapacityError>,
    ) -> Self {
        Self(ExecutionCause::Local {
            kind,
            message,
            capacity,
        })
    }
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            #[cfg(feature = "vulkan")]
            ExecutionCause::Vulkan(error) => error.fmt(f),
            #[cfg(all(feature = "d3d12", windows))]
            ExecutionCause::D3D12(error) => error.fmt(f),
            ExecutionCause::Local { message, .. } => f.write_str(message),
        }
    }
}

impl Error for ExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.0 {
            #[cfg(feature = "vulkan")]
            ExecutionCause::Vulkan(error) => Some(error),
            #[cfg(all(feature = "d3d12", windows))]
            ExecutionCause::D3D12(error) => Some(error),
            ExecutionCause::Local { .. } => None,
        }
    }
}

#[cfg(feature = "vulkan")]
impl From<crate::vulkan::RuntimeError> for ExecutionError {
    fn from(error: crate::vulkan::RuntimeError) -> Self {
        Self(ExecutionCause::Vulkan(error))
    }
}

#[cfg(all(feature = "d3d12", windows))]
impl From<crate::d3d12::D3D12RuntimeError> for ExecutionError {
    fn from(error: crate::d3d12::D3D12RuntimeError) -> Self {
        Self(ExecutionCause::D3D12(error))
    }
}
