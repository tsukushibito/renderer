//! Backend-independent RenderGraph execution and frame ownership.

mod error;
mod frames;
mod id;
#[cfg(test)]
pub(crate) mod native_image_tests;
mod preparation;
mod queue;
#[cfg(all(feature = "presentation", windows))]
mod window;

pub use crate::capacity::{ResourceClass, WorkingSet};
pub use error::{ExecutionError, ExecutionErrorKind};
pub use frames::{FrameCompletion, FrameSubmission};
pub use id::FrameId;
#[cfg(all(feature = "d3d12", windows))]
pub use preparation::D3D12Config;
#[cfg(feature = "vulkan")]
pub use preparation::VulkanConfig;
pub use preparation::{RuntimeConfig, RuntimeLimits};
pub(crate) use queue::{FrameQueue, Observation};
#[cfg(all(feature = "presentation", windows))]
pub use window::{WindowPresenter, WindowReadiness};

use std::time::{Duration, Instant};

use crate::graph::{CompiledGraph, ImportBuffer, ImportImage, ResourceKey};

/// Owns the selected backend and its bounded queue of accepted Graph frames.
///
/// Poll or wait explicitly to obtain completed frames. Dropping an accepted
/// frame's metadata leaves pending work in this runtime.
pub struct Runtime {
    inner: RuntimeInner,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one fixed owner per runtime, with both backends stored inline"
)]
enum RuntimeInner {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::vulkan::VulkanRuntime),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(crate::d3d12::D3D12Runtime),
}

impl Runtime {
    /// Accept a compiled Graph once, without waiting for earlier Graphs.
    ///
    /// A successful return exposes scheduled exports for same-runtime follow-up
    /// work. CPU readback becomes available only upon observed GPU completion.
    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<FrameSubmission, ExecutionError> {
        Ok(match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => FrameSubmission::from(runtime.submit_frame(graph)?),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => FrameSubmission::from(runtime.submit_frame(graph)?),
        })
    }

    /// Number of accepted frames whose completion has not been retired.
    pub fn pending_frames(&self) -> usize {
        match &self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => runtime.pending_frames(),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime.pending_frames(),
        }
    }

    /// Return one available completion without waiting for GPU work.
    pub fn poll_frame(&mut self) -> Result<Option<FrameCompletion>, ExecutionError> {
        self.wait_frame(Duration::ZERO)
    }

    /// Return one available completion, optionally waiting for the oldest
    /// pending frame within the requested backend wait budget. Empty and
    /// timed-out waits both return None.
    pub fn wait_frame(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<FrameCompletion>, ExecutionError> {
        Ok(match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => {
                runtime.wait_frame(timeout)?.map(FrameCompletion::from)
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime.wait_frame(timeout)?.map(FrameCompletion::from),
        })
    }

    /// Return the named pending frame's completion, leaving other results
    /// available. A timeout returns None. IDs from other runtimes or frames
    /// whose completion has already been consumed are errors.
    pub fn wait_for_frame(
        &mut self,
        id: FrameId,
        timeout: Duration,
    ) -> Result<Option<FrameCompletion>, ExecutionError> {
        Ok(match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => runtime
                .wait_for_frame(id, timeout)?
                .map(FrameCompletion::from),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime
                .wait_for_frame(id, timeout)?
                .map(FrameCompletion::from),
        })
    }

    /// Retire Graph frames within a shared wait budget, discarding their CPU
    /// results. Returns false on timeout and retains work whose completion is
    /// unproven. Presentation has its own retirement and close operations.
    pub fn drain_frames(&mut self, timeout: Duration) -> Result<bool, ExecutionError> {
        let start = Instant::now();
        while self.pending_frames() != 0 {
            if self
                .wait_frame(timeout.saturating_sub(start.elapsed()))?
                .is_none()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Capture the latest accepted scheduled contents for a registered buffer.
    pub fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer, ExecutionError> {
        Ok(match &self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => runtime.refresh_buffer(key)?,
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime.refresh_buffer(key)?,
        })
    }

    /// Capture the latest accepted scheduled contents for a registered image.
    pub fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, ExecutionError> {
        Ok(match &self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => runtime.refresh_image(key)?,
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime.refresh_image(key)?,
        })
    }

    /// Remove a registration while letting accepted GPU work retain its
    /// resources through retirement.
    pub fn delete(&mut self, key: ResourceKey) -> Result<(), ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => runtime.delete(key)?,
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime.delete(key)?,
        }
        Ok(())
    }

    /// Change profiling for subsequent Graph submissions. Enabling profiling
    /// may prepare native queries at this explicit configuration boundary.
    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => runtime.set_profiling(enabled)?,
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => runtime.set_profiling(enabled)?,
        }
        Ok(())
    }

    /// Access Vulkan-specific change-boundary operations.
    #[cfg(feature = "vulkan")]
    pub fn as_vulkan_mut(&mut self) -> Option<&mut crate::vulkan::VulkanRuntime> {
        match &mut self.inner {
            RuntimeInner::Vulkan(runtime) => Some(runtime),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(_) => None,
        }
    }

    /// Access D3D12-specific change-boundary operations.
    #[cfg(all(feature = "d3d12", windows))]
    pub fn as_d3d12_mut(&mut self) -> Option<&mut crate::d3d12::D3D12Runtime> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(_) => None,
            RuntimeInner::D3D12(runtime) => Some(runtime),
        }
    }

    /// Inspect Vulkan-specific device information, validation, or memory.
    #[cfg(feature = "vulkan")]
    pub fn as_vulkan(&self) -> Option<&crate::vulkan::VulkanRuntime> {
        match &self.inner {
            RuntimeInner::Vulkan(runtime) => Some(runtime),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(_) => None,
        }
    }

    /// Inspect D3D12-specific device information, validation, or memory.
    #[cfg(all(feature = "d3d12", windows))]
    pub fn as_d3d12(&self) -> Option<&crate::d3d12::D3D12Runtime> {
        match &self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(_) => None,
            RuntimeInner::D3D12(runtime) => Some(runtime),
        }
    }
}
