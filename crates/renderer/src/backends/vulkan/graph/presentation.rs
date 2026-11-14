//! Graph-to-WSI adapter: canonical source state, leases and public orchestration.
//! Native Window/Swapchain/slot/program owners live in the Vulkan WSI subtree.

use super::{MemoryOwner, PhysicalLease, RuntimeConfig, Snapshot, VulkanRuntime};
use crate::backends::vulkan::device::VulkanDevice;
use crate::backends::vulkan::diagnostics::Diagnostics;
use crate::backends::vulkan::error::RuntimeError;
use crate::backends::vulkan::instance::ValidationMode;
use crate::backends::vulkan::presentation::{
    Acquired, CLEANUP_BUDGET, PresentCommands, PresentResult, Program, SlotRecord, SourceScope,
    Swapchain, SwapchainPlan, WindowSurface, classify_present, native, query_present_mode,
    validate_config,
};
use crate::backends::vulkan::transfer::{PendingTransfer, TransferError, TransferPayload};
use crate::graph::{ImageFormat, ImportImage, ResourceDesc};
use ash::vk;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::window::Window;

pub use crate::backends::vulkan::presentation::{PresentOutcome, PresentationConfig};
mod state;
use state::Inner;
mod presenter;
pub use presenter::Presenter;
mod runtime;
mod work;
use work::PresentWork;

#[cfg(test)]
mod tests;
