#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]

#[path = "support/validation.rs"]
mod validation;

use std::time::Duration;

use sashiko_renderer_core::{
    capacity::{ResourceClass, WorkingSet},
    graph::{GraphCapacity, GraphPool, ImageDesc},
    runtime::{ExecutionErrorKind, Runtime, RuntimeConfig, RuntimeLimits},
};

#[test]
#[ignore = "requires a hardware GPU for each selected backend"]
fn dropped_frame_image_and_persistent_target_have_distinct_registration_lifetimes() {
    for config in configurations() {
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

        validation::check_after_teardown(rt);
    }
}

fn configurations() -> impl Iterator<Item = RuntimeConfig> {
    [
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(Default::default()),
        #[cfg(all(windows, feature = "d3d12"))]
        RuntimeConfig::D3D12(Default::default()),
    ]
    .into_iter()
}

#[test]
#[ignore = "requires a hardware GPU for each selected backend"]
fn uploaded_texture_is_available_to_a_batched_submission_before_waiting() {
    for config in configurations() {
        let desc = ImageDesc::rgba8(1, 1);
        let mut rt = Runtime::new(
            config,
            RuntimeLimits {
                resource_capacity: 3,
                ..Default::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Image { desc, count: 3 }],
                ..Default::default()
            },
        )
        .unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 2).unwrap();
        assert!(matches!(
            rt.upload_texture(&mut pool, "short", desc, &[0; 3]),
            Err(sashiko_renderer_core::runtime::TextureUploadError::Graph(_))
        ));
        assert_eq!(rt.pending_frames(), 0);

        let (first_frame, first) = rt
            .upload_texture(&mut pool, "albedo", desc, &[1; 4])
            .unwrap();
        let mut graph = pool.begin().unwrap();
        let albedo = graph.import_image("albedo", first.snapshot()).unwrap();
        let normal = graph
            .upload_texture_for_export("normal", desc, &[2; 4])
            .unwrap();
        let orm = graph
            .upload_texture_for_export("orm", desc, &[3; 4])
            .unwrap();
        let reads = [albedo, normal, orm]
            .map(|image| graph.readback_image("material pixel", image).unwrap());
        let plan = graph.compile().unwrap();
        let error = rt
            .submit_with_images(&plan, [normal, normal])
            .err()
            .expect("duplicate ownership");
        assert_eq!(error.kind(), ExecutionErrorKind::InvalidInput);
        let (second, [normal_owner, orm_owner]) =
            rt.submit_with_images(&plan, [normal, orm]).unwrap();
        let second_frame = second.id();
        let old_key = first.snapshot().key();
        let normal_key = normal_owner.snapshot().key();
        assert_eq!(
            second.export_image(orm).unwrap().key(),
            orm_owner.snapshot().key()
        );

        // Early owner drops unregister independently, even while accepted readers
        // or transfer work still hold the underlying textures.
        drop((first, normal_owner, second, plan));
        let wait = Duration::from_secs(10);
        let mut pixels = rt.wait_for_frame(second_frame, wait).unwrap().unwrap();
        for (read, expected) in reads.into_iter().zip([1, 2, 3]) {
            let mut bytes = [0; 4];
            pixels.readback_into(read, &mut bytes).unwrap();
            assert_eq!(bytes, [expected; 4]);
        }
        drop(pixels);
        drop(rt.wait_for_frame(first_frame, wait).unwrap().unwrap());
        for retired in [old_key, normal_key] {
            assert_eq!(
                rt.refresh_image(retired).unwrap_err().kind(),
                ExecutionErrorKind::StaleResource
            );
        }
        assert_eq!(
            rt.refresh_image(orm_owner.snapshot().key()).unwrap(),
            *orm_owner.snapshot()
        );
        drop(orm_owner);
        rt.collect_released_images().unwrap();
        validation::check_after_teardown(rt);
    }
}

#[test]
#[ignore = "requires a hardware GPU for each selected backend"]
fn immutable_mip_promotion_preserves_old_image_and_copies_valid_levels() {
    for config in configurations() {
        let desc = ImageDesc {
            mip_levels: 3,
            ..ImageDesc::rgba8(4, 4)
        };
        let mut rt = Runtime::new(
            config,
            RuntimeLimits::default(),
            WorkingSet {
                resources: &[ResourceClass::Image { desc, count: 2 }],
                ..Default::default()
            },
        )
        .unwrap();
        rt.check_texture_support(desc, true).unwrap();
        assert_eq!(
            rt.check_texture_support(
                ImageDesc {
                    mip_levels: 8,
                    ..desc
                },
                true
            )
            .unwrap_err()
            .kind(),
            ExecutionErrorKind::InvalidInput
        );
        let mut pool = GraphPool::new(GraphCapacity::default(), 3).unwrap();
        let wait = Duration::from_secs(20);
        let (seed, old) = rt
            .upload_texture_mips(&mut pool, "old", desc, None, 2..3, &[7; 4])
            .unwrap();
        assert!(old.snapshot().contains_mips(2..3));
        assert!(!old.snapshot().contains_mips(1..3));
        assert!(
            rt.upload_texture_mips(&mut pool, "bad", desc, Some(&old), 1..2, &[0; 13])
                .is_err()
        );
        let (promote, new) = rt
            .upload_texture_mips(&mut pool, "new", desc, Some(&old), 1..2, &[8; 16])
            .unwrap();
        assert!(!new.snapshot().is_mip_initialized(0));
        assert!(new.snapshot().contains_mips(1..3));
        assert_ne!(old.snapshot().key(), new.snapshot().key());
        assert_eq!(
            rt.refresh_image(old.snapshot().key()).unwrap(),
            *old.snapshot()
        );
        drop(rt.wait_for_frame(seed, wait).unwrap().unwrap());
        drop(rt.wait_for_frame(promote, wait).unwrap().unwrap());

        // Read both versions after publication. Copy selected mips to a small
        // single-level image since ordinary CPU readback takes an entire image.
        for (owner, mip, expected, extent) in [(&old, 2, 7, 1), (&new, 2, 7, 1), (&new, 1, 8, 2)] {
            let dst = ImageDesc::rgba8(extent, extent);
            rt.prepare_working_set(WorkingSet {
                resources: &[ResourceClass::Image {
                    desc: dst,
                    count: 1,
                }],
                ..Default::default()
            })
            .unwrap();
            let mut graph = pool.begin().unwrap();
            let source = graph.import_image("texture", owner.snapshot()).unwrap();
            let tile = graph.image("tile", dst).unwrap();
            graph
                .copy_image_region(
                    "sample mip contents",
                    source,
                    sashiko_renderer_core::graph::ImageRegion::mip(desc, mip).unwrap(),
                    tile,
                    0,
                    [0, 0],
                )
                .unwrap();
            let read = graph.readback_image("pixels", tile).unwrap();
            let id = rt.submit(&graph.compile().unwrap()).unwrap().id();
            let mut result = rt.wait_for_frame(id, wait).unwrap().unwrap();
            let mut bytes = vec![0; (extent * extent * 4) as usize];
            result.readback_into(read, &mut bytes).unwrap();
            assert_eq!(bytes, vec![expected; bytes.len()]);
        }
        drop((old, new));
        rt.collect_released_images().unwrap();
        validation::check_after_teardown(rt);
    }
}
