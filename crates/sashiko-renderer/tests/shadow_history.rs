#![cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]

use std::{num::NonZeroUsize, time::Duration};

use sashiko_renderer::pbr::{CASCADE_COUNT, ShadowHistory, ShadowHistoryError, ShadowKey};
use sashiko_renderer::pbr_forward::add_shadow_pass;
use sashiko_renderer_core::{
    capacity::Preparation,
    graph::{GraphCapacity, GraphPool, ImageDesc},
    runtime::{ExecutionErrorKind, Runtime, RuntimeConfig, RuntimeLimits},
};

fn recover_from_a_rejected_submission(config: RuntimeConfig) {
    const SIZE: u32 = 8;
    const TIMEOUT: Duration = Duration::from_secs(10);
    let capacity = GraphCapacity::default();
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    preparation
        .reserve_image(ImageDesc::depth32(SIZE, SIZE), CASCADE_COUNT)
        .unwrap();
    let mut runtime = Runtime::new(
        config,
        RuntimeLimits {
            graph: capacity,
            frames: 1,
            results: 1,
            ..RuntimeLimits::default()
        },
        preparation.working_set(),
    )
    .unwrap();
    #[cfg(feature = "vulkan")]
    if let Some(vulkan) = runtime.as_vulkan() {
        assert_ne!(vulkan.device_info().device_type, "CPU");
    }
    let mut pool = GraphPool::new(capacity, 2).unwrap();
    let mut history = ShadowHistory::default();
    let keys = [ShadowKey::new([[0.0; 4]; 4], 0); CASCADE_COUNT];

    let mut graph = pool.begin().unwrap();
    let frame = history.begin_frame(&mut graph, SIZE, keys).unwrap();
    for cascade in 0..CASCADE_COUNT {
        let target = graph.image_view(frame.target(cascade), 0, 0).unwrap();
        add_shadow_pass(&mut graph, cascade, target, |_| Ok(())).unwrap();
    }
    let mut updates = frame.export(&mut graph).unwrap();
    let graph = graph.compile().unwrap();
    let other_plan = pool.begin().unwrap().compile().unwrap();
    assert!(matches!(
        history.submit(&mut runtime, &other_plan, &mut updates),
        Err(ShadowHistoryError::ForeignGraph)
    ));
    assert_eq!(runtime.pending_frames(), 0);
    drop(other_plan);

    let first = history.submit(&mut runtime, &graph, &mut updates).unwrap();
    assert_eq!(first.graph_id(), graph.graph_id());
    assert!(matches!(
        history.submit(&mut runtime, &graph, &mut updates),
        Err(ShadowHistoryError::AlreadyPublished)
    ));

    let mut next = pool.begin().unwrap();
    let frame = history.begin_frame(&mut next, SIZE, keys).unwrap();
    next.readback_image("reused shadow", frame.target(0))
        .unwrap();
    let mut updates = frame.export(&mut next).unwrap();
    assert_eq!(updates.reused(), [true; CASCADE_COUNT]);
    let next = next.compile().unwrap();
    assert!(matches!(
        history.accept(&first, &mut updates),
        Err(ShadowHistoryError::ForeignGraph)
    ));
    assert!(matches!(
        history.submit(&mut runtime, &next, &mut updates),
        Err(ShadowHistoryError::Execution(error)) if error.kind() == ExecutionErrorKind::CapacityExhausted
    ));
    runtime
        .wait_for_frame(first.id(), TIMEOUT)
        .unwrap()
        .unwrap();
    let second = history.submit(&mut runtime, &next, &mut updates).unwrap();
    history.clear(&mut runtime).unwrap();
    runtime
        .wait_for_frame(second.id(), TIMEOUT)
        .unwrap()
        .unwrap();
    drop((first, second));

    let strict = cfg!(feature = "development-validation");
    match config {
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(_) => {
            let monitor = runtime.as_vulkan().unwrap().validation_monitor();
            drop(runtime);
            let report = monitor.report();
            assert!(report.is_clean(), "{report:#?}");
            if strict {
                assert_eq!(report.core_validation, Some(true));
                assert_eq!(report.synchronization_validation, Some(true));
            }
        }
        #[cfg(all(feature = "d3d12", windows))]
        RuntimeConfig::D3D12(_) => {
            let monitor = runtime.as_d3d12().unwrap().validation_monitor();
            drop(runtime);
            let report = monitor.snapshot();
            assert!(report.is_clean(), "{report:#?}");
            assert_eq!(report.debug_layer_enabled, strict);
            assert_eq!(report.gpu_based_validation_enabled, strict);
        }
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a real Vulkan device"]
fn vulkan_shadow_history_rejects_foreign_graph_and_preserves_backpressure_retry() {
    recover_from_a_rejected_submission(RuntimeConfig::Vulkan(Default::default()));
}

#[cfg(all(feature = "d3d12", windows))]
#[test]
#[ignore = "requires a real D3D12 device"]
fn d3d12_shadow_history_rejects_foreign_graph_and_preserves_backpressure_retry() {
    recover_from_a_rejected_submission(RuntimeConfig::D3D12(Default::default()));
}
