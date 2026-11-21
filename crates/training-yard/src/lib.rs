//! Shared training-yard assets and game commands for native and headless hosts.
#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
mod yard;
pub use yard::*;
pub mod replay;
pub mod standalone;
pub mod targets;
#[path = "../../renderer/examples/support/validation.rs"]
mod validation;
#[cfg(all(windows, feature = "presentation"))]
pub mod window;
