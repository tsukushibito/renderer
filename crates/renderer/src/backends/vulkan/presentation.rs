//! Native Window presentation owners, capability selection and recording.
//! Canonical Graph state and submission publication stay in graph/presentation.

use super::error::RuntimeError;
use super::memory::MemoryOwner;
use ash::vk;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod surface;
pub(super) use surface::WindowSurface;
mod swapchain;
pub(super) use swapchain::{Swapchain, SwapchainPlan, query_present_mode};
mod program;
use program::Binding;
pub(super) use program::Program;
mod slots;
use slots::Slot;
pub(super) use slots::{Acquired, SlotRecord};
mod commands;
pub(super) use commands::{PresentCommands, SourceScope};

pub(super) const CLEANUP_BUDGET: Duration = Duration::from_secs(10);

pub(super) fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}
pub(super) fn timeout_ns(timeout: Duration) -> u64 {
    timeout.as_nanos().min((u64::MAX - 1) as u128) as u64
}
/// Explicit bounds for WSI resources. They are independent of the actual
/// Swapchain image count and the runtime's native execution-slot capacity.
#[derive(Clone, Copy, Debug)]
pub struct PresentationConfig {
    pub max_frames: usize,
    pub max_retired_swapchains: usize,
    pub desired_images: u32,
    /// FIFO when true; IMMEDIATE when false. Unsupported requests fail rather
    /// than silently falling back to a synchronized presentation mode.
    pub vsync: bool,
}
impl Default for PresentationConfig {
    fn default() -> Self {
        Self {
            max_frames: 2,
            max_retired_swapchains: 2,
            desired_images: 3,
            vsync: true,
        }
    }
}
pub(super) fn validate_config(config: PresentationConfig) -> Result<(), RuntimeError> {
    if config.max_frames == 0
        || config.max_frames > u32::MAX as usize
        || config.max_retired_swapchains == 0
        || config.max_retired_swapchains > u32::MAX as usize
        || config.desired_images == 0
    {
        return Err(RuntimeError::invalid(
            "presentation capacities must be nonzero and fit u32",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    /// The presentation request was accepted. This does not prove that the
    /// image has reached the display or that presentation resources retired.
    Presented { suboptimal: bool },
    /// No image or WSI slot is currently available; no CPU/GPU wait was added.
    NotReady,
    /// The Swapchain was recreated. Rebuild for `Presenter::extent()`.
    Recreated,
    /// Presentation was rejected because the Swapchain is out of date. Call
    /// `Presenter::prepare` before rebuilding; `extent()` may still be old.
    /// A rendering submit and presentation semaphore waits may remain pending.
    RecreateRequired,
    /// The requested window extent is zero; no acquire is attempted.
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PresentResult {
    Enqueued(QueuedPresent),
    Unchanged,
    Uncertain,
}

/// Enqueued semaphore waits do not imply an accepted presentation request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QueuedPresent {
    Accepted { suboptimal: bool },
    OutOfDate,
    SurfaceLost,
}

impl QueuedPresent {
    pub(super) fn finish(
        self,
        acquired_suboptimal: bool,
        dirty: &mut bool,
        surface_lost: &mut bool,
    ) -> Result<PresentOutcome, RuntimeError> {
        *dirty |= acquired_suboptimal;
        match self {
            Self::Accepted { suboptimal } => {
                *dirty |= suboptimal;
                Ok(PresentOutcome::Presented {
                    suboptimal: acquired_suboptimal || suboptimal,
                })
            }
            Self::OutOfDate => {
                *dirty = true;
                Ok(PresentOutcome::RecreateRequired)
            }
            Self::SurfaceLost => {
                *surface_lost = true;
                Err(RuntimeError::unsupported(
                    "Surface lost during present; pending presentation retained",
                ))
            }
        }
    }
}

pub(super) fn classify_present(result: vk::Result) -> PresentResult {
    match result {
        vk::Result::SUCCESS => {
            PresentResult::Enqueued(QueuedPresent::Accepted { suboptimal: false })
        }
        vk::Result::SUBOPTIMAL_KHR => {
            PresentResult::Enqueued(QueuedPresent::Accepted { suboptimal: true })
        }
        vk::Result::ERROR_OUT_OF_DATE_KHR => PresentResult::Enqueued(QueuedPresent::OutOfDate),
        vk::Result::ERROR_SURFACE_LOST_KHR => PresentResult::Enqueued(QueuedPresent::SurfaceLost),
        vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => {
            PresentResult::Unchanged
        }
        _ => PresentResult::Uncertain,
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn presentation_errors_preserve_their_actual_enqueue_semantics() {
        for result in [
            vk::Result::SUCCESS,
            vk::Result::SUBOPTIMAL_KHR,
            vk::Result::ERROR_OUT_OF_DATE_KHR,
            vk::Result::ERROR_SURFACE_LOST_KHR,
        ] {
            assert!(matches!(
                classify_present(result),
                PresentResult::Enqueued(_)
            ));
        }
        assert_eq!(
            classify_present(vk::Result::ERROR_OUT_OF_DATE_KHR),
            PresentResult::Enqueued(QueuedPresent::OutOfDate)
        );
        assert_eq!(
            classify_present(vk::Result::ERROR_SURFACE_LOST_KHR),
            PresentResult::Enqueued(QueuedPresent::SurfaceLost)
        );
        for result in [
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        ] {
            assert_eq!(classify_present(result), PresentResult::Unchanged);
        }
        assert_eq!(
            classify_present(vk::Result::ERROR_DEVICE_LOST),
            PresentResult::Uncertain
        );
        assert_eq!(
            classify_present(vk::Result::ERROR_UNKNOWN),
            PresentResult::Uncertain
        );
    }

    #[test]
    fn only_accepted_present_requests_report_presented() {
        for (result, acquired_suboptimal, expected, recreate) in [
            (
                vk::Result::SUCCESS,
                false,
                PresentOutcome::Presented { suboptimal: false },
                false,
            ),
            (
                vk::Result::SUCCESS,
                true,
                PresentOutcome::Presented { suboptimal: true },
                true,
            ),
            (
                vk::Result::SUBOPTIMAL_KHR,
                false,
                PresentOutcome::Presented { suboptimal: true },
                true,
            ),
            (
                vk::Result::ERROR_OUT_OF_DATE_KHR,
                false,
                PresentOutcome::RecreateRequired,
                true,
            ),
            (
                vk::Result::ERROR_OUT_OF_DATE_KHR,
                true,
                PresentOutcome::RecreateRequired,
                true,
            ),
        ] {
            let PresentResult::Enqueued(queued) = classify_present(result) else {
                panic!("semaphore waits must remain enqueued for {result:?}");
            };
            let mut dirty = false;
            let mut surface_lost = false;
            assert_eq!(
                queued
                    .finish(acquired_suboptimal, &mut dirty, &mut surface_lost)
                    .unwrap(),
                expected,
                "native result {result:?}"
            );
            assert_eq!(dirty, recreate);
            assert!(!surface_lost);
        }
        let PresentResult::Enqueued(queued) = classify_present(vk::Result::ERROR_SURFACE_LOST_KHR)
        else {
            panic!("Surface loss does not cancel enqueued semaphore waits");
        };
        let mut dirty = false;
        let mut surface_lost = false;
        assert_eq!(
            queued
                .finish(false, &mut dirty, &mut surface_lost)
                .unwrap_err()
                .kind(),
            crate::backends::vulkan::error::RuntimeErrorKind::Unsupported
        );
        assert!(surface_lost);
        assert!(!dirty);

        // Successful presentation must not cancel an independently queued
        // resize or VSync request.
        let mut dirty = true;
        QueuedPresent::Accepted { suboptimal: false }
            .finish(false, &mut dirty, &mut surface_lost)
            .unwrap();
        assert!(dirty);
    }

    #[test]
    fn presentation_rejects_zero_capacity() {
        assert!(
            validate_config(PresentationConfig {
                max_frames: 0,
                ..Default::default()
            })
            .is_err()
        );
    }
}
