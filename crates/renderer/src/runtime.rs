//! Backend-independent RenderGraph execution and frame ownership.
//!
//! These contracts are available to plugins in every build. Enable a GPU backend
//! on the host application to construct a runtime or obtain a submission receipt.

mod error;
mod frames;
mod id;
mod managed_image;
#[cfg(all(test, any(feature = "vulkan", all(windows, feature = "d3d12"))))]
pub(crate) mod native_image_tests;
mod preparation;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod queue;
#[cfg(all(
    feature = "presentation",
    windows,
    any(feature = "vulkan", feature = "d3d12")
))]
mod window;

pub use crate::capacity::{ResourceClass, WorkingSet};
pub use error::{ExecutionError, ExecutionErrorKind};
pub use frames::{FrameCompletion, FrameSubmission};
pub use id::FrameId;
pub use managed_image::ManagedImage;
#[cfg(all(feature = "d3d12", windows))]
pub use preparation::D3D12Config;
#[cfg(feature = "vulkan")]
pub use preparation::VulkanConfig;
pub use preparation::{RuntimeConfig, RuntimeLimits};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(crate) use queue::{FrameQueue, Observation};
#[cfg(all(
    feature = "presentation",
    windows,
    any(feature = "vulkan", feature = "d3d12")
))]
pub use window::{WindowPresenter, WindowReadiness};

/// Include Vulkan-specific adapter items whenever the host enables Vulkan.
#[cfg(feature = "vulkan")]
#[doc(hidden)]
#[macro_export]
macro_rules! __renderer_with_vulkan {
    ($($item:item)*) => { $($item)* };
}
#[cfg(not(feature = "vulkan"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __renderer_with_vulkan {
    ($($item:item)*) => {};
}
/// Include Vulkan-specific items using the host's unified Renderer features.
#[doc(inline)]
pub use crate::__renderer_with_vulkan as with_vulkan;

use std::time::{Duration, Instant};

use crate::graph::{CompiledGraph, ImportBuffer, ImportImage, ResourceKey};

/// Owns the selected backend and its bounded queue of accepted Graph frames.
///
/// Poll or wait explicitly to obtain completed frames. Dropping an accepted
/// frame's metadata leaves pending work in this runtime.
pub struct Runtime {
    inner: RuntimeInner,
    image_releases: std::sync::Arc<managed_image::ImageReleases>,
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

#[cfg_attr(
    not(any(feature = "vulkan", all(feature = "d3d12", windows))),
    expect(
        unused_variables,
        reason = "without a backend, the dispatcher has no runtime variants"
    )
)]
impl Runtime {
    /// Accept a compiled Graph once, without waiting for earlier Graphs.
    ///
    /// A successful return exposes scheduled exports for same-runtime follow-up
    /// work. CPU readback becomes available only upon observed GPU completion.
    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<FrameSubmission, ExecutionError> {
        if let Some(image) = graph.frame_image {
            let (mut receipt, owner) = self.submit_with_image(graph, image)?;
            receipt.frame_image = Some(owner);
            return Ok(receipt);
        }
        self.collect_released_images()?;
        self.submit_native(graph)
    }

    fn submit_native(&mut self, graph: &CompiledGraph) -> Result<FrameSubmission, ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => {
                Ok(FrameSubmission::from(runtime.submit_frame(graph)?))
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => {
                Ok(FrameSubmission::from(runtime.submit_frame(graph)?))
            }
        }
    }

    /// Number of accepted frames whose completion has not been retired.
    pub fn pending_frames(&self) -> usize {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref runtime) => runtime.pending_frames(),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref runtime) => runtime.pending_frames(),
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
        self.collect_released_images()?;
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => {
                Ok(runtime.wait_frame(timeout)?.map(FrameCompletion::from))
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => {
                Ok(runtime.wait_frame(timeout)?.map(FrameCompletion::from))
            }
        }
    }

    /// Return the named pending frame's completion, leaving other results
    /// available. A timeout returns None. IDs from other runtimes or frames
    /// whose completion has already been consumed are errors.
    pub fn wait_for_frame(
        &mut self,
        id: FrameId,
        timeout: Duration,
    ) -> Result<Option<FrameCompletion>, ExecutionError> {
        self.collect_released_images()?;
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => Ok(runtime
                .wait_for_frame(id, timeout)?
                .map(FrameCompletion::from)),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => Ok(runtime
                .wait_for_frame(id, timeout)?
                .map(FrameCompletion::from)),
        }
    }

    /// Retire Graph frames within a shared wait budget, discarding their CPU
    /// results. Returns false on timeout and retains work whose completion is
    /// unproven. Presentation has its own retirement and close operations.
    pub fn drain_frames(&mut self, timeout: Duration) -> Result<bool, ExecutionError> {
        self.collect_released_images()?;
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

    /// Register an uninitialized device buffer from the prepared working set.
    /// Upload ranges through Graph imports; retain the key until explicit deletion.
    pub fn register_buffer(
        &mut self,
        name: &str,
        desc: crate::graph::BufferDesc,
    ) -> Result<ResourceKey, ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => {
                Ok(runtime.register_buffer(name, desc, false)?.key())
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => {
                let _ = name;
                Ok(runtime.register_buffer(desc)?)
            }
        }
    }

    /// Capture the latest accepted scheduled contents for a registered buffer.
    pub fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer, ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref runtime) => runtime.refresh_buffer(key).map_err(Into::into),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref runtime) => runtime.refresh_buffer(key).map_err(Into::into),
        }
    }

    /// Capture the latest accepted scheduled contents for a registered image.
    pub fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref runtime) => runtime.refresh_image(key).map_err(Into::into),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref runtime) => runtime.refresh_image(key).map_err(Into::into),
        }
    }

    /// Remove a registration while letting accepted GPU work retain its
    /// resources through retirement.
    pub fn delete(&mut self, key: ResourceKey) -> Result<(), ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => runtime.delete(key).map_err(Into::into),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => runtime.delete(key).map_err(Into::into),
        }
    }

    /// Change profiling for subsequent Graph submissions. Enabling profiling
    /// may prepare native queries at this explicit configuration boundary.
    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), ExecutionError> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => {
                runtime.set_profiling(enabled).map_err(Into::into)
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => {
                runtime.set_profiling(enabled).map_err(Into::into)
            }
        }
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
        match self.inner {
            RuntimeInner::Vulkan(ref runtime) => Some(runtime),
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(_) => None,
        }
    }

    /// Inspect D3D12-specific device information, validation, or memory.
    #[cfg(all(feature = "d3d12", windows))]
    pub fn as_d3d12(&self) -> Option<&crate::d3d12::D3D12Runtime> {
        match self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(_) => None,
            RuntimeInner::D3D12(ref runtime) => Some(runtime),
        }
    }
}
