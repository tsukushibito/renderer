//! Application-defined pipelines are compiled as external crate consumers.
#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "../examples/support/toon_pipeline.rs"]
mod toon_pipeline;
#[path = "../examples/support/views.rs"]
mod views;
#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: sashiko_renderer_core::measurement::CountingAllocator<std::alloc::System> =
    sashiko_renderer_core::measurement::CountingAllocator::new(std::alloc::System);

use sashiko_renderer::pipeline::PipelineResult;
use sashiko_renderer_core::runtime::RuntimeConfig;

#[path = "view_pipelines/shared.rs"]
mod shared;

fn compare(api: RuntimeConfig) -> PipelineResult<()> {
    shared::check(api).map_err(|e| format!("heterogeneous reference: {e}"))?;
    views::run_on(api, views::Example::Textured, views::standard)
        .map_err(|e| format!("textured: {e}"))?;
    views::run_on(api, views::Example::MixedPost, views::standard)
        .map_err(|e| format!("mixed: {e}"))?;
    views::run_on(api, views::Example::TwoViews, views::standard)?;
    views::run_on(api, views::Example::TwoViews, toon_pipeline::prepare)
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan hardware"]
fn vulkan_interchangeable_pipelines() -> PipelineResult<()> {
    compare(RuntimeConfig::Vulkan(Default::default()))
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 hardware"]
fn d3d12_interchangeable_pipelines() -> PipelineResult<()> {
    compare(RuntimeConfig::D3D12(Default::default()))
}

#[path = "view_pipelines/publication.rs"]
mod publication;
use views::validation;

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan hardware"]
fn vulkan_publication() -> PipelineResult<()> {
    publication::check(RuntimeConfig::Vulkan(Default::default()))
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 hardware"]
fn d3d12_publication() -> PipelineResult<()> {
    publication::check(RuntimeConfig::D3D12(Default::default()))
}

#[path = "view_pipelines/setup.rs"]
mod setup;

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan hardware"]
fn vulkan_setup_lifecycle() -> PipelineResult<()> {
    setup::check(RuntimeConfig::Vulkan(Default::default()))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 hardware"]
fn d3d12_setup_lifecycle() -> PipelineResult<()> {
    setup::check(RuntimeConfig::D3D12(Default::default()))
}
