use raw_window_handle::HasWindowHandle;
use std::{num::NonZeroUsize, time::Duration};

use crate::graph::ImportImage;
use crate::{PresentOutcome, PresentationConfig, PresentationMetrics};

use super::{ExecutionError, Runtime, RuntimeConfig, RuntimeInner, RuntimeLimits, WorkingSet};

/// State to use when deciding whether to build a Window's next Graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowReadiness {
    Ready {
        extent: (u32, u32),
        recreated: bool,
    },
    /// Required graphics or presentation work has not retired yet.
    Pending,
    /// The requested Window extent is zero.
    Suspended,
}

/// A Window's presentation state. The runtime and presenter share a Device and
/// queue. The application owns its event loop and requests resize and VSync.
pub struct WindowPresenter {
    inner: WindowPresenterInner,
}

enum WindowPresenterInner {
    #[cfg(feature = "vulkan")]
    Vulkan(crate::vulkan::Presenter),
    #[cfg(feature = "d3d12")]
    D3D12(crate::d3d12::D3D12Presenter),
}

impl Runtime {
    /// Create a runtime, its prepared Graph working set, and a presenter for
    /// the window. Move in a host window owner (for example an Arc to a winit
    /// window, or an Rc around an engine window implementing HasWindowHandle).
    /// The presenter retains that owner on the calling thread through native teardown.
    /// Presentation execution slots are additional to the Graph frame limit.
    pub fn for_window(
        config: RuntimeConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
        window: impl HasWindowHandle + 'static,
        presentation: PresentationConfig,
    ) -> Result<(Self, WindowPresenter), ExecutionError> {
        limits.validate()?;
        match config {
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(settings) => {
                let capacity = limits.vulkan_capacity()?;
                let (mut runtime, presenter) =
                    crate::vulkan::VulkanRuntime::for_window_with_validation(
                        limits.vulkan_config(settings, presentation.max_frames)?,
                        window,
                        presentation,
                        settings.validation,
                    )?;
                runtime.set_frame_limit(limits.frames)?;
                runtime.prepare_graph_working_set(Some(capacity), working_set)?;
                Ok((
                    Self {
                        inner: RuntimeInner::Vulkan(runtime),
                    },
                    WindowPresenter {
                        inner: WindowPresenterInner::Vulkan(presenter),
                    },
                ))
            }
            #[cfg(feature = "d3d12")]
            RuntimeConfig::D3D12(settings) => {
                let runtime = super::preparation::new_d3d12(settings, limits, working_set)?;
                let presenter = runtime.create_presenter(window, presentation)?;
                Ok((
                    Self {
                        inner: RuntimeInner::D3D12(runtime),
                    },
                    WindowPresenter {
                        inner: WindowPresenterInner::D3D12(presenter),
                    },
                ))
            }
        }
    }

    /// Prepare a runtime for host windows that will be attached later. Reserve
    /// the total number of additional Vulkan presentation executions across
    /// those windows. D3D12 reserves slots separately when each window attaches.
    pub fn new_with_presentation(
        config: RuntimeConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
        presentation_slots: NonZeroUsize,
    ) -> Result<Self, ExecutionError> {
        #[cfg(not(feature = "vulkan"))]
        let _ = presentation_slots;
        match config {
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(settings) => {
                limits.validate()?;
                let capacity = limits.vulkan_capacity()?;
                let mut runtime = crate::vulkan::VulkanRuntime::new_with_presentation(
                    limits.vulkan_config(settings, presentation_slots.get())?,
                    settings.validation,
                )?;
                runtime.set_frame_limit(limits.frames)?;
                runtime.prepare_graph_working_set(Some(capacity), working_set)?;
                Ok(Self {
                    inner: RuntimeInner::Vulkan(runtime),
                })
            }
            #[cfg(feature = "d3d12")]
            RuntimeConfig::D3D12(_) => Self::new(config, limits, working_set),
        }
    }

    /// Attach an independent window to the same runtime and resource registry.
    /// Create the Vulkan runtime with `for_window` or `new_with_presentation`. Its queue
    /// must support the new surface. Presenters share the execution slots.
    /// Poll each presenter when several windows have work in flight.
    pub fn create_presenter(
        &self,
        window: impl HasWindowHandle + 'static,
        presentation: PresentationConfig,
    ) -> Result<WindowPresenter, ExecutionError> {
        let inner = match &self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => {
                WindowPresenterInner::Vulkan(runtime.create_presenter(window, presentation)?)
            }
            #[cfg(feature = "d3d12")]
            RuntimeInner::D3D12(runtime) => {
                WindowPresenterInner::D3D12(runtime.create_presenter(window, presentation)?)
            }
        };
        Ok(WindowPresenter { inner })
    }

    /// Queue an exported linear RGBA8 image for display. Same-queue ordering
    /// handles its producer; this operation adds no CPU wait for the Graph.
    pub fn present(
        &mut self,
        presenter: &mut WindowPresenter,
        image: &ImportImage,
    ) -> Result<PresentOutcome, ExecutionError> {
        match (&mut self.inner, &mut presenter.inner) {
            #[cfg(feature = "vulkan")]
            (RuntimeInner::Vulkan(runtime), WindowPresenterInner::Vulkan(presenter)) => {
                Ok(runtime.present(presenter, image)?)
            }
            #[cfg(feature = "d3d12")]
            (RuntimeInner::D3D12(runtime), WindowPresenterInner::D3D12(presenter)) => {
                Ok(runtime.present(presenter, image)?)
            }
            #[cfg(all(feature = "vulkan", feature = "d3d12"))]
            _ => Err(ExecutionError::wrong_owner(
                "Presenter belongs to another backend",
            )),
        }
    }
}

impl WindowPresenter {
    /// Resolve a changed extent or VSync setting before building a new Graph.
    /// A successful ready response supplies the actual drawable extent.
    pub fn prepare(&mut self) -> Result<WindowReadiness, ExecutionError> {
        let (outcome, extent) = match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => match presenter.prepare() {
                Ok(outcome) => (outcome, presenter.extent()),
                Err(error)
                    if error.capacity().is_some_and(|shortage| {
                        shortage.resource == crate::capacity::RuntimeCapacityKind::RetiredSwapchains
                            && shortage.recovery == crate::capacity::CapacityRecovery::Poll
                    }) =>
                {
                    return Ok(WindowReadiness::Pending);
                }
                Err(error) => return Err(error.into()),
            },
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => (presenter.prepare()?, presenter.extent()),
        };
        match outcome {
            None | Some(PresentOutcome::Recreated) => {
                let extent = extent.ok_or_else(|| {
                    ExecutionError::invalid("ready Presenter has no drawable extent")
                })?;
                Ok(WindowReadiness::Ready {
                    extent,
                    recreated: outcome.is_some(),
                })
            }
            Some(PresentOutcome::NotReady | PresentOutcome::RecreateRequired) => {
                Ok(WindowReadiness::Pending)
            }
            Some(PresentOutcome::Suspended) => Ok(WindowReadiness::Suspended),
            Some(PresentOutcome::Presented { .. }) => Err(ExecutionError::invalid(
                "preparing a Presenter reported a present",
            )),
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => presenter.resize(width, height),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => presenter.resize(width, height),
        }
    }

    pub fn set_vsync(&mut self, enabled: bool) -> Result<(), ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => Ok(presenter.set_vsync(enabled)?),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => Ok(presenter.set_vsync(enabled)?),
        }
    }

    /// Observe window work completion without waiting for GPU or display work.
    pub fn poll(&mut self) -> Result<usize, ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => Ok(presenter.poll()?),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => Ok(presenter.poll()?),
        }
    }

    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => Ok(presenter.set_profiling(enabled)?),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => Ok(presenter.set_profiling(enabled)?),
        }
    }

    pub fn take_profile(&mut self) -> Option<PresentationMetrics> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => presenter.take_profile(),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => presenter.take_profile(),
        }
    }

    /// Wait only for presentation ownership to return. Graph submissions and
    /// CPU readback results have separate lifetimes.
    pub fn wait_idle(&mut self, timeout: Duration) -> Result<bool, ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => Ok(presenter.wait_idle(timeout)?),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => Ok(presenter.wait_idle(timeout)?),
        }
    }

    /// Close a Window's presentation, preserving outstanding work if the wait
    /// budget expires. Call the runtime's frame completion operations separately.
    pub fn close(&mut self, timeout: Duration) -> Result<bool, ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            WindowPresenterInner::Vulkan(presenter) => Ok(presenter.close(timeout)?),
            #[cfg(feature = "d3d12")]
            WindowPresenterInner::D3D12(presenter) => Ok(presenter.close(timeout)?),
        }
    }
}
