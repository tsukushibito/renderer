#![cfg(all(windows, feature = "vulkan", feature = "d3d12"))]

#[path = "../fixtures/indexed_instancing_fixture.rs"]
mod fixture;

#[test]
#[ignore = "requires same-adapter Vulkan and D3D12 hardware and requested validation layers"]
fn indexed_instancing_gpu() {
    if cfg!(debug_assertions) {
        panic!("run the acceptance in release");
    }
    fixture::run().unwrap();
}
