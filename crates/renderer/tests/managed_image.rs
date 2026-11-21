#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]

use std::time::Duration;

use renderer::{
    capacity::{ResourceClass, WorkingSet},
    graph::{GraphCapacity, GraphPool, ImageDesc},
    runtime::{ExecutionErrorKind, Runtime, RuntimeConfig, RuntimeLimits},
};

#[test]
#[ignore = "requires a hardware GPU for each selected backend"]
fn dropped_frame_image_and_persistent_target_have_distinct_registration_lifetimes() {
    let configurations = [
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(Default::default()),
        #[cfg(all(windows, feature = "d3d12"))]
        RuntimeConfig::D3D12(Default::default()),
    ];
    for config in configurations {
        let desc = ImageDesc::rgba8(1, 1);
        let capacity = GraphCapacity::default();
        let mut rt = Runtime::new(
            config,
            RuntimeLimits {
                graph: capacity,
                ..Default::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Image { desc, count: 1 }],
                ..Default::default()
            },
        )
        .unwrap();
        let mut pool = GraphPool::new(capacity, 2).unwrap();
        let mut seed = pool.begin().unwrap();
        let target = seed.image("display", desc).unwrap();
        seed.upload_image("pixels", target, &[1, 2, 3, 255])
            .unwrap();
        seed.export_frame_image(target).unwrap();
        let seed = seed.compile().unwrap();
        let wait = Duration::from_secs(10);

        // Drop without extracting the image, including before Graph completion.
        let abandoned = rt.submit(&seed).unwrap();
        let old = abandoned.export_image(target).unwrap().key();
        let id = abandoned.id();
        drop(abandoned);
        drop(rt.wait_for_frame(id, wait).unwrap().unwrap());
        assert_eq!(
            rt.refresh_image(old).unwrap_err().kind(),
            ExecutionErrorKind::StaleResource
        );

        // An explicit transfer keeps the registration after the receipt returns.
        let (receipt, owned) = rt.submit_with_image(&seed, target).unwrap();
        let persistent = owned.into_registered();
        let id = receipt.id();
        drop(receipt);
        drop(rt.wait_for_frame(id, wait).unwrap().unwrap());
        {
            let mut graph = pool.begin().unwrap();
            let imported = graph.import_image("retained output", &persistent).unwrap();
            graph
                .upload_image("replace contents", imported, &[4, 5, 6, 255])
                .unwrap();
            graph.export_frame_image(imported).unwrap();
            let read = graph
                .readback_image("read retained output", imported)
                .unwrap();
            let mut receipt = rt.submit(&graph.compile().unwrap()).unwrap();
            let owned = receipt.take_frame_image().unwrap();
            assert!(receipt.take_frame_image().is_none());
            assert_eq!(owned.snapshot().key(), persistent.key());
            let id = receipt.id();
            drop((receipt, owned));
            let mut result = rt.wait_for_frame(id, wait).unwrap().unwrap();
            let mut bytes = [0; 4];
            result.readback_into(read, &mut bytes).unwrap();
            assert_eq!(bytes, [4, 5, 6, 255]);
        }
        assert!(rt.refresh_image(persistent.key()).is_ok());
        rt.delete(persistent.key()).unwrap();

        // Explicit manual deletion and reuse cannot let an old owner discard
        // the new registration at the same physical slot.
        let (receipt, first) = rt.submit_with_image(&seed, target).unwrap();
        let id = receipt.id();
        rt.delete(first.snapshot().key()).unwrap();
        drop(receipt);
        drop(rt.wait_for_frame(id, wait).unwrap().unwrap());
        let (receipt, second) = rt.submit_with_image(&seed, target).unwrap();
        assert_ne!(first.snapshot().key(), second.snapshot().key());
        drop(first);
        rt.collect_released_images().unwrap();
        assert!(rt.refresh_image(second.snapshot().key()).is_ok());
        let id = receipt.id();
        drop((receipt, second));
        drop(rt.wait_for_frame(id, wait).unwrap().unwrap());

        #[cfg(feature = "vulkan")]
        if let Some(vk) = rt.as_vulkan() {
            let monitor = vk.validation_monitor();
            drop(rt);
            let report = monitor.report();
            assert!(report.is_clean(), "{report:?}");
            if cfg!(feature = "development-validation") {
                assert_eq!(report.core_validation, Some(true));
                assert_eq!(report.synchronization_validation, Some(true));
            }
            continue;
        }
        #[cfg(all(windows, feature = "d3d12"))]
        if let Some(dx) = rt.as_d3d12() {
            let monitor = dx.validation_monitor();
            drop(rt);
            let report = monitor.snapshot();
            assert!(report.is_clean(), "{report:?}");
            if cfg!(feature = "development-validation") {
                assert!(
                    report.debug_layer_enabled
                        && report.gpu_based_validation_enabled
                        && report.teardown_checked
                );
            }
        }
    }
}
