//! A prior explicit unload must not cause delayed destruction in normal frames.

use super::test_input::{build_snapshot, render_snapshot};
use super::*;
use crate::{
    FrameCapacity, ResourceClass, RuntimeConfig,
    engine::{
        Draw, Engine, EngineCapacity, IDENTITY, Material, MeshData, SnapshotPool, Vertex, View,
    },
    graph::ImageDesc,
    measurement::Snapshot,
};
use std::time::Instant;

#[test]
#[ignore = "explicit isolated real GPU allocation-counter regression; run shipping and strict separately"]
fn vulkan_engine_retired_pending_mesh_is_collected_only_at_maintenance() {
    const WAIT: Duration = Duration::from_secs(30);
    if cfg!(debug_assertions) {
        panic!("allocation acceptance requires --release");
    }
    let mut assets = Engine::new(EngineCapacity {
        meshes: 2,
        materials: 1,
        retired_mesh_versions: 1,
    })
    .unwrap();
    let input = || {
        MeshData::new(
            &[[-0.4, -0.4, 0.25], [0.4, -0.4, 0.25], [0.0, 0.4, 0.25]].map(|position| Vertex {
                position,
                color: [1.0; 4],
            }),
            &[0, 1, 2],
        )
        .unwrap()
    };
    let old = assets.create_mesh(input()).unwrap();
    let current = assets.create_mesh(input()).unwrap();
    let material = assets.create_material(Material::default()).unwrap();
    let snapshots = SnapshotPool::new(2, 3).unwrap();
    let view = View {
        width: 8,
        height: 8,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    };
    let draw = |mesh| Draw {
        mesh,
        material,
        world_from_model: IDENTITY,
    };
    let old_snapshot = assets.snapshot(&snapshots, &view, &[draw(old)]).unwrap();
    let current_snapshot = assets
        .snapshot(&snapshots, &view, &[draw(current)])
        .unwrap();
    let runtime = VulkanRuntime::new(RuntimeConfig {
        execution_slots: 3,
        resource_capacity: 16,
        ..RuntimeConfig::default()
    })
    .unwrap();
    assert_ne!(
        runtime.device_info().device_type,
        "CPU",
        "real GPU required"
    );
    let monitor = runtime.validation_monitor();
    let mut renderer = VulkanEngine::new(
        runtime,
        MeshCacheConfig {
            max_meshes: 2,
            max_bytes: 1024,
            max_upload_submissions: 2,
            max_retired_sources: 0,
        },
        EngineFrameCapacity::default(),
    )
    .unwrap();
    renderer
        .runtime_mut()
        .prepare_working_set(
            FrameCapacity::default(),
            &[
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(8, 8),
                    count: 3,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(8, 8),
                    count: 3,
                },
            ],
        )
        .unwrap();
    for snapshot in [&old_snapshot, &current_snapshot] {
        let deadline = Instant::now() + WAIT;
        let mut upload = renderer.prepare_snapshot(snapshot, 2).unwrap();
        while upload.state != MeshUploadState::Ready || upload.pending_submissions != 0 {
            upload = renderer
                .wait_uploads(
                    deadline
                        .checked_duration_since(Instant::now())
                        .expect("mesh preparation timed out"),
                )
                .unwrap();
        }
    }
    let consume = |renderer: &mut VulkanEngine, mut frame: RenderedFrame| {
        let mut completion = renderer.runtime_mut().wait_frame(WAIT).unwrap().unwrap();
        assert_eq!(completion.id(), frame.submission.id());
        let mut pixels = [0; 8 * 8 * 4];
        completion
            .result
            .readback_into(frame.readback.unwrap(), &mut pixels)
            .unwrap();
        assert_eq!(&pixels[..4], &[0, 0, 0, 255]);
        renderer.release_output(&mut frame).unwrap();
        drop((frame, completion));
    };
    // Warm the ordinary frame resource/result paths before the old version is
    // marked obsolete. The test still rebuilds and compiles every measured frame.
    for _ in 0..3 {
        let frame =
            render_snapshot(&mut renderer, &current_snapshot, true, |_, color| Ok(color)).unwrap();
        consume(&mut renderer, frame);
    }
    let pinned = build_snapshot(&mut renderer, &old_snapshot, false, |_, color| Ok(color)).unwrap();
    let pending =
        render_snapshot(&mut renderer, &old_snapshot, true, |_, color| Ok(color)).unwrap();
    renderer.unload_mesh(old);
    assets.delete_mesh(old).unwrap();
    assert_eq!(renderer.mesh_cache_stats().prepared_retained_meshes, 1);
    let old_vertices = renderer.residents[&MeshVersionKey::new(old, 1)]
        .buffers
        .as_ref()
        .unwrap()
        .vertices;

    let before = Snapshot::now();
    drop((pinned, old_snapshot));
    renderer.poll_uploads().unwrap();
    consume(&mut renderer, pending);
    for count in [0, 1, 2, 1] {
        let draws = [draw(current); 2];
        let snapshot = assets.snapshot(&snapshots, &view, &draws[..count]).unwrap();
        renderer.prepare_snapshot(&snapshot, 2).unwrap();
        let frame = render_snapshot(&mut renderer, &snapshot, true, |_, color| Ok(color)).unwrap();
        consume(&mut renderer, frame);
        drop(snapshot);
    }
    let measured = Snapshot::now().since(before);
    assert!(measured.is_allocation_free(), "{measured:#?}");
    assert_eq!(renderer.mesh_cache_stats().resident_meshes, 2);
    assert_eq!(renderer.mesh_cache_stats().prepared_retained_meshes, 0);
    // A zero-capacity source pool cannot silently grow or partially unregister
    // this version. The exact CPU owner can accept the anchor at maintenance.
    assert!(matches!(
        renderer.collect_retired_meshes(None),
        Err(EngineRenderError::Preparation(
            MeshPreparationError::SourceRetirementCapacityExhausted
        ))
    ));
    assert_eq!(renderer.mesh_cache_stats().resident_meshes, 2);
    renderer.runtime().refresh_buffer(old_vertices).unwrap();
    assert_eq!(renderer.collect_retired_meshes(Some(&assets)).unwrap(), 1);
    assert_eq!(renderer.mesh_cache_stats().resident_meshes, 1);
    assert!(renderer.runtime().refresh_buffer(old_vertices).is_err());
    assert_eq!(assets.collect_retired_meshes(), 1);
    drop((renderer, current_snapshot, snapshots, assets));
    let report = monitor.report();
    assert!(report.is_clean(), "{report:#?}");
    if cfg!(feature = "development-validation") {
        assert_eq!(report.core_validation, Some(true));
        assert_eq!(report.synchronization_validation, Some(true));
    }
}
