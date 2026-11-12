//! RenderGraph-centered renderer for game engine integration.
//!
//! No engine-facing rendering API is implemented yet.
//! The default configuration has no GPU, window-system, or native dependencies.
//! The `vulkan` feature includes a private, validation-required Vulkan 1.3 instance
//! bootstrap. Only explicitly selected ignored tests initialize Vulkan at this stage.
//!
//! Engine-facing and graph APIs will remain independent of Vulkan and VMA types.

#[cfg(feature = "vulkan")]
mod backends;
