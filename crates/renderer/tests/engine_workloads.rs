use renderer::engine;

#[path = "../examples/support/engine_geometry.rs"]
mod engine_geometry;
#[cfg(feature = "vulkan")]
#[path = "../examples/support/engine_workloads.rs"]
mod engine_workloads;

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "explicit real GPU large mesh/streaming/multiple View acceptance; requires effective Core+SyncVal"]
fn vulkan_engine_game_workload_acceptance() {
    engine_workloads::run(engine_workloads::Mode::Verify).unwrap();
}
