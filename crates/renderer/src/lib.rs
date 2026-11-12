//! RenderGraph-centered renderer for game engine integration.
//!
//! M0 establishes the build and test entry points; no rendering API is implemented yet.
//! The default configuration has no GPU, window-system, or native dependencies.
//! The `vulkan` feature currently verifies that ash and vk-mem (including VMA's C++
//! implementation) build together. It does not initialize Vulkan or render anything.
//!
//! Engine-facing and graph APIs will remain independent of Vulkan and VMA types.
