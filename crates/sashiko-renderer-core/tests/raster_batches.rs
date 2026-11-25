#![cfg(all(windows, feature = "vulkan", feature = "d3d12"))]

#[path = "../fixtures/raster_batch_fixture.rs"]
mod fixture;

#[test]
#[ignore = "requires same-adapter Vulkan and D3D12 hardware and requested validation layers"]
fn ordered_mrt_gpu() {
    if cfg!(debug_assertions) {
        panic!("run the acceptance in release");
    }
    fixture::run().unwrap();
}

#[test]
#[ignore = "requires Vulkan and D3D12 hardware and requested native layers"]
fn native_batches_preserve_blend_order_and_cancelled_slot_reuse() {
    for budget in [1, 4] {
        fixture::run_with_recording(std::num::NonZeroUsize::new(budget)).unwrap();
    }
}
