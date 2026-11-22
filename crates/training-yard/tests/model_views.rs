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
    training_yard::replay::host(RuntimeConfig::Vulkan(Default::default()), None)?;
    #[cfg(all(windows, feature = "d3d12"))]
    training_yard::replay::host(RuntimeConfig::D3D12(Default::default()), None)?;
    Ok(())
}

#[test]
#[ignore = "requires real devices for each enabled backend"]
fn changing_1024_targets_and_recovering_capacity() -> PipelineResult<()> {
    #[cfg(feature = "vulkan")]
    training_yard::targets::run(RuntimeConfig::Vulkan(Default::default()))?;
    #[cfg(all(windows, feature = "d3d12"))]
    training_yard::targets::run(RuntimeConfig::D3D12(Default::default()))?;
    Ok(())
}

#[test]
fn prepared_draw_count_tracks_simultaneous_parts() -> PipelineResult<()> {
    let mut yard = training_yard::Yard::new()?;
    let camera = training_yard::camera(1, [48, 24])?;
    let setup = renderer::pipeline::RendererSetup::standard(camera).prepare(yard.assets())?;
    // Two actors with two near parts or one far part, plus a preloaded material variant.
    assert_eq!(setup.scene_limits().max_draws, 4);
    Ok(())
}

#[test]
fn failed_equipment_preparation_preserves_current_draw_and_allows_retry() -> PipelineResult<()> {
    use training_yard::{Action, EquipmentAsset, Yard};
    let mut yard = Yard::new()?;
    let camera = training_yard::camera(1, [48, 24])?;
    let mut objects = Vec::new();
    let current = yard.extract(&camera, &mut objects).objects[1].mesh;
    let asset = EquipmentAsset::Wide;
    yard.apply(Action::Request(asset), |_| Ok(()))?;
    assert!(
        yard.apply(Action::Deliver(asset), |_| Err("preparation failed".into()))
            .is_err()
    );
    assert_eq!(yard.extract(&camera, &mut objects).objects[1].mesh, current);
    yard.apply(Action::Request(asset), |_| Ok(()))?;
    yard.apply(Action::Deliver(asset), |_| Ok(()))?;
    assert_ne!(yard.extract(&camera, &mut objects).objects[1].mesh, current);
    Ok(())
}
