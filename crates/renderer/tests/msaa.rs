#![cfg(all(windows, feature = "vulkan", feature = "d3d12"))]

#[path = "../examples/support/msaa.rs"]
mod msaa;

#[test]
#[ignore = "requires same-adapter Vulkan and D3D12 hardware and requested validation layers"]
fn resolves_color_and_preserves_depth_order() {
    msaa::run();
}
