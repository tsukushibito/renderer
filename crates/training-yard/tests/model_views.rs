//! Verify the same host replay used by the training-yard executable.
#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]

use renderer::{core::runtime::RuntimeConfig, pipeline::PipelineResult};
#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer::core::measurement::CountingAllocator<std::alloc::System> =
    renderer::core::measurement::CountingAllocator::new(std::alloc::System);

#[test]
#[ignore = "requires real devices for each enabled backend"]
fn posed_models_in_two_views() -> PipelineResult<()> {
    #[cfg(feature = "vulkan")]
    training_yard::replay::host(RuntimeConfig::Vulkan(Default::default()))?;
    #[cfg(all(windows, feature = "d3d12"))]
    training_yard::replay::host(RuntimeConfig::D3D12(Default::default()))?;
    Ok(())
}
