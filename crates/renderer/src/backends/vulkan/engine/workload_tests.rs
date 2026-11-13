//! Internal resident-buffer verification complements the public workload example.
//! It observes actual engine-uploaded bytes without exposing a testing-only API.

use std::time::{Duration, Instant};

use super::{MeshCacheConfig, MeshUploadState, MeshVersionKey, VulkanEngine};
use crate::{
    RuntimeConfig, ValidationMode, VulkanRuntime,
    engine::{Draw, Engine, IDENTITY, Material, View},
    graph::{Graph, ResourceKey},
};

#[path = "../../../../examples/support/engine_geometry.rs"]
mod engine_geometry;

const TIMEOUT: Duration = Duration::from_secs(30);
const STAGING: u64 = 256 * 1024;

#[test]
#[ignore = "explicit real GPU upload cancellation/readmission test; run shipping and strict validation separately"]
fn vulkan_engine_cancelled_upload_can_readmit_same_snapshot() {
    let runtime = VulkanRuntime::new(RuntimeConfig {
        execution_slots: 2,
        resource_capacity: 4,
        staging_bytes: STAGING,
        max_shader_bindings: 1,
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
            max_meshes: 1,
            max_bytes: 2 * 1024 * 1024,
            max_upload_submissions: 2,
        },
    )
    .unwrap();
    let mut assets = Engine::new(1, 1).unwrap();
    let generated = engine_geometry::grid(112);
    assert!(generated.vertices.len() as u64 > STAGING);
    let mesh = assets.create_mesh(generated.mesh).unwrap();
    let material = assets.create_material(Material::default()).unwrap();
    let snapshot = assets
        .snapshot(
            &View {
                width: 8,
                height: 8,
                clip_from_world: IDENTITY,
                clear_color: [0.0, 0.0, 0.0, 1.0],
            },
            &[Draw {
                mesh,
                material,
                world_from_model: IDENTITY,
            }],
        )
        .unwrap();
    let partial = renderer.prepare_snapshot(&snapshot).unwrap();
    assert!(partial.submitted_bytes > 0 && partial.remaining_bytes > 0);
    renderer.cancel_uploads().unwrap();
    assert_eq!(renderer.mesh_cache_stats().resident_meshes, 0);
    let cancelled = renderer.wait_uploads(TIMEOUT).unwrap();
    assert_eq!(cancelled.remaining_bytes, 0);
    assert_eq!(cancelled.pending_submissions, 0);
    assert!(renderer.retiring.is_empty());

    let mut progress = renderer.prepare_snapshot(&snapshot).unwrap();
    let started = Instant::now();
    while progress.state != MeshUploadState::Ready || progress.pending_submissions != 0 {
        assert!(
            started.elapsed() < TIMEOUT,
            "readmitted upload must complete"
        );
        progress = renderer
            .wait_uploads(TIMEOUT.saturating_sub(started.elapsed()))
            .unwrap();
    }
    let buffers = renderer.residents[&MeshVersionKey::new(mesh, 1)]
        .buffers
        .as_ref()
        .unwrap();
    let vertices = buffers.vertices;
    let indices = buffers.indices;
    read_all(&mut renderer, vertices, &generated.vertices);
    read_all(&mut renderer, indices, &generated.indices);
    renderer.unload_all_meshes().unwrap();
    assert!(renderer.residents.is_empty() && renderer.retiring.is_empty());
    drop(renderer);
    let report = monitor.report();
    #[cfg(feature = "development-validation")]
    {
        assert_eq!(report.core_validation, Some(true), "{report:#?}");
        assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
    }
    assert!(
        report.is_clean(),
        "validation through native destruction: {report:#?}"
    );
}

fn read_all(renderer: &mut VulkanEngine, key: ResourceKey, expected: &[u8]) {
    for (part, bytes) in expected.chunks(STAGING as usize).enumerate() {
        let offset = part as u64 * STAGING;
        let mut graph = Graph::new();
        let source = graph
            .import_buffer(
                "resident mesh byte verification",
                &renderer.runtime().refresh_buffer(key).unwrap(),
            )
            .unwrap();
        let pass = graph
            .readback_buffer(
                "mesh byte verification readback",
                source,
                offset,
                bytes.len() as u64,
            )
            .unwrap();
        let mut submitted = renderer
            .runtime_mut()
            .submit(&graph.compile().unwrap())
            .unwrap();
        let mut result = submitted
            .wait(TIMEOUT)
            .unwrap()
            .expect("resident mesh readback timed out");
        assert_eq!(
            result.readback(pass).unwrap(),
            bytes,
            "resident bytes at offset {offset}"
        );
    }
}

#[test]
#[ignore = "explicit real GPU complete large mesh upload byte verification; requires effective Core+SyncVal"]
fn vulkan_engine_large_resident_mesh_matches_every_uploaded_byte() {
    let runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 4,
            resource_capacity: 32,
            staging_bytes: STAGING,
            max_shader_bindings: 8,
        },
        ValidationMode::Required,
    )
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
            max_bytes: 16 * 1024 * 1024,
            max_upload_submissions: 3,
        },
    )
    .unwrap();
    let mut assets = Engine::new(2, 1).unwrap();
    let generated = engine_geometry::grid(400);
    assert!(generated.vertices.len() > 4 * 1024 * 1024);
    let mesh = assets.create_mesh(generated.mesh).unwrap();
    let material = assets.create_material(Material::default()).unwrap();
    let snapshot = assets
        .snapshot(
            &View {
                width: 32,
                height: 32,
                clip_from_world: IDENTITY,
                clear_color: [0.0, 0.0, 0.0, 1.0],
            },
            &[Draw {
                mesh,
                material,
                world_from_model: IDENTITY,
            }],
        )
        .unwrap();
    let began = Instant::now();
    let mut progress = renderer.prepare_snapshot(&snapshot).unwrap();
    while progress.state != MeshUploadState::Ready || progress.pending_submissions != 0 {
        assert!(began.elapsed() < TIMEOUT, "large resident upload timed out");
        progress = renderer
            .wait_uploads(TIMEOUT.saturating_sub(began.elapsed()))
            .unwrap();
    }
    let resident = renderer
        .residents
        .get(&MeshVersionKey::new(mesh, 1))
        .unwrap();
    let buffers = resident.buffers.as_ref().unwrap();
    let vertices = buffers.vertices;
    let indices = buffers.indices;
    read_all(&mut renderer, vertices, &generated.vertices);
    read_all(&mut renderer, indices, &generated.indices);
    assert!(
        renderer
            .runtime()
            .memory_stats()
            .unwrap()
            .upload
            .peak_reserved_bytes
            <= STAGING
    );
    renderer.unload_mesh(mesh).unwrap();
    assert!(
        assets.mesh_revision(mesh).is_ok(),
        "GPU unload must preserve the CPU asset"
    );
    drop(renderer);
    let report = monitor.report();
    assert_eq!(report.core_validation, Some(true), "{report:#?}");
    assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
    assert!(
        report.is_clean(),
        "validation through native destruction: {report:#?}"
    );
}

#[test]
#[ignore = "explicit same-queue mesh readiness test; requires effective Core+SyncVal"]
fn vulkan_engine_ready_mesh_can_draw_before_upload_completion_observation() {
    let runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 4,
            resource_capacity: 16,
            staging_bytes: STAGING,
            max_shader_bindings: 8,
        },
        ValidationMode::Required,
    )
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
            max_bytes: 1024 * 1024,
            max_upload_submissions: 1,
        },
    )
    .unwrap();
    let mut assets = Engine::new(1, 1).unwrap();
    let mesh = assets.create_mesh(engine_geometry::grid(4).mesh).unwrap();
    let material = assets
        .create_material(Material {
            tint: [0.0, 1.0, 1.0, 1.0],
        })
        .unwrap();
    let snapshot = assets
        .snapshot(
            &View {
                width: 32,
                height: 32,
                clip_from_world: IDENTITY,
                clear_color: [0.0, 0.0, 0.0, 1.0],
            },
            &[Draw {
                mesh,
                material,
                world_from_model: IDENTITY,
            }],
        )
        .unwrap();
    let began = Instant::now();
    let mut progress = renderer.prepare_snapshot(&snapshot).unwrap();
    while progress.state != MeshUploadState::Ready {
        assert!(began.elapsed() < TIMEOUT, "mesh readiness timed out");
        progress = renderer
            .wait_uploads(TIMEOUT.saturating_sub(began.elapsed()))
            .unwrap();
    }
    assert!(
        progress.pending_submissions >= 1,
        "final upload must remain owned and unobserved"
    );
    // Do not wait/poll the final upload. A queue-accepted content version is
    // usable by the next Graph; native synchronization supplies visibility.
    let mut frame = renderer
        .render_snapshot(&snapshot, true, |_, image| Ok(image))
        .unwrap();
    let pass = frame.readback.unwrap();
    renderer.release_output(&mut frame).unwrap();
    let mut completion = renderer
        .runtime_mut()
        .wait_frame(TIMEOUT)
        .unwrap()
        .expect("ready mesh draw timed out");
    assert_eq!(completion.id(), frame.submission.id());
    assert_eq!(
        completion.result.readback(pass).unwrap(),
        engine_geometry::rectangle(32, 32, [4, 4, 28, 28], [0, 255, 255, 255], [0, 0, 0, 255],)
    );
    drop(completion);
    renderer.wait_uploads(TIMEOUT).unwrap();
    drop(renderer);
    let report = monitor.report();
    assert_eq!(report.core_validation, Some(true), "{report:#?}");
    assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
    assert!(
        report.is_clean(),
        "validation through native destruction: {report:#?}"
    );
}

#[test]
#[ignore = "explicit frame output ownership test; requires effective Core+SyncVal"]
fn vulkan_engine_output_release_respects_original_registration_ownership() {
    let runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 4,
            resource_capacity: 16,
            staging_bytes: STAGING,
            max_shader_bindings: 8,
        },
        ValidationMode::Required,
    )
    .unwrap();
    assert_ne!(
        runtime.device_info().device_type,
        "CPU",
        "real GPU required"
    );
    let monitor = runtime.validation_monitor();
    let mut renderer = VulkanEngine::new(runtime, MeshCacheConfig::default()).unwrap();
    let mut graph = Graph::new();
    let caller_image = graph
        .image(
            "caller-owned extension output",
            crate::graph::ImageDesc::rgba8(8, 8),
        )
        .unwrap();
    let caller_bytes = [17u8, 34, 51, 255].repeat(8 * 8);
    graph
        .upload_image(
            "initialize caller output",
            caller_image,
            caller_bytes.clone(),
        )
        .unwrap();
    graph.export_image(caller_image).unwrap();
    let mut upload = renderer
        .runtime_mut()
        .submit(&graph.compile().unwrap())
        .unwrap();
    let caller = upload.export_image(caller_image).unwrap();
    drop(
        upload
            .wait(TIMEOUT)
            .unwrap()
            .expect("caller output upload timed out"),
    );
    let assets = Engine::new(1, 1).unwrap();
    let snapshot = assets
        .snapshot(
            &View {
                width: 8,
                height: 8,
                clip_from_world: IDENTITY,
                clear_color: [0.0, 0.0, 0.0, 1.0],
            },
            &[],
        )
        .unwrap();
    let mut borrowed = renderer
        .render_snapshot(&snapshot, true, |graph, _| {
            graph.import_image("external completed output", &caller)
        })
        .unwrap();
    assert_eq!(
        borrowed.output_ownership(),
        crate::engine::FrameOutputOwnership::Caller
    );
    let borrowed_pass = borrowed.readback.unwrap();
    renderer.release_output(&mut borrowed).unwrap();
    renderer.release_output(&mut borrowed).unwrap();
    assert!(renderer.runtime().refresh_image(caller.key()).is_ok());
    let mut completed = renderer
        .runtime_mut()
        .wait_frame(TIMEOUT)
        .unwrap()
        .expect("caller output frame timed out");
    assert_eq!(
        completed.result.readback(borrowed_pass).unwrap(),
        caller_bytes
    );
    drop(completed);
    let mut owned = renderer
        .render_snapshot(&snapshot, true, |_, image| Ok(image))
        .unwrap();
    assert_eq!(
        owned.output_ownership(),
        crate::engine::FrameOutputOwnership::Frame
    );
    let original_key = owned.output.key();
    let owned_pass = owned.readback.unwrap();
    // Public result metadata is mutable. Cleanup authority must remain tied to
    // the immutable original frame registration, never to this replaced field.
    owned.output = renderer.runtime().refresh_image(caller.key()).unwrap();
    renderer.release_output(&mut owned).unwrap();
    renderer.release_output(&mut owned).unwrap();
    assert!(renderer.runtime().refresh_image(original_key).is_err());
    assert!(renderer.runtime().refresh_image(caller.key()).is_ok());
    let mut completed = renderer
        .runtime_mut()
        .wait_frame(TIMEOUT)
        .unwrap()
        .expect("owned output frame timed out");
    assert_eq!(
        completed.result.readback(owned_pass).unwrap(),
        [0u8, 0, 0, 255].repeat(8 * 8)
    );
    drop(completed);
    renderer.runtime_mut().delete(caller.key()).unwrap();
    drop(renderer);
    let report = monitor.report();
    assert_eq!(report.core_validation, Some(true), "{report:#?}");
    assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
    assert!(
        report.is_clean(),
        "validation through native destruction: {report:#?}"
    );
}

#[test]
#[ignore = "explicit physical resource residency/backpressure test; requires effective Core+SyncVal"]
fn vulkan_engine_streams_cpu_assets_with_a_four_resource_runtime() {
    let runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 4,
            resource_capacity: 4,
            staging_bytes: STAGING,
            max_shader_bindings: 8,
        },
        ValidationMode::Required,
    )
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
            max_meshes: 16,
            max_bytes: 1024 * 1024,
            max_upload_submissions: 1,
        },
    )
    .unwrap();
    let mut assets = Engine::new(3, 1).unwrap();
    let material = assets.create_material(Material::default()).unwrap();
    let colors = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];
    let mut snapshots = Vec::new();
    let mut handles = Vec::new();
    for color in colors {
        let tint = color.map(|component| component as f32 / 255.0);
        let vertices = [
            [-0.75, -0.75, 0.25],
            [0.75, -0.75, 0.25],
            [-0.75, 0.75, 0.25],
            [0.75, 0.75, 0.25],
        ]
        .map(|position| crate::engine::Vertex {
            position,
            color: tint,
        });
        let mesh = assets
            .create_mesh(crate::engine::MeshData::new(&vertices, &[0, 1, 2, 1, 3, 2]).unwrap())
            .unwrap();
        handles.push(mesh);
        snapshots.push(
            assets
                .snapshot(
                    &View {
                        width: 32,
                        height: 32,
                        clip_from_world: IDENTITY,
                        clear_color: [0.0, 0.0, 0.0, 1.0],
                    },
                    &[Draw {
                        mesh,
                        material,
                        world_from_model: IDENTITY,
                    }],
                )
                .unwrap(),
        );
    }
    let ready = |renderer: &mut VulkanEngine, snapshot: &crate::engine::FrameSnapshot| {
        let began = Instant::now();
        let mut progress = renderer.prepare_snapshot(snapshot).unwrap();
        while progress.state != MeshUploadState::Ready || progress.pending_submissions != 0 {
            assert!(
                began.elapsed() < TIMEOUT,
                "bounded residency made no upload progress"
            );
            progress = renderer
                .wait_uploads(TIMEOUT.saturating_sub(began.elapsed()))
                .unwrap();
        }
    };
    for (snapshot, color) in snapshots.iter().zip(colors) {
        ready(&mut renderer, snapshot);
        let mut frame = renderer
            .render_snapshot(snapshot, true, |_, image| Ok(image))
            .unwrap();
        let pass = frame.readback.unwrap();
        renderer.release_output(&mut frame).unwrap();
        let mut completed = renderer
            .runtime_mut()
            .wait_frame(TIMEOUT)
            .unwrap()
            .expect("four-resource View timed out");
        assert_eq!(
            completed.result.readback(pass).unwrap(),
            engine_geometry::rectangle(32, 32, [4, 4, 28, 28], color, [0, 0, 0, 255],)
        );
        drop(completed);
        assert!(renderer.mesh_cache_stats().resident_meshes <= 1);
        assert_eq!(renderer.runtime().stats().registered_resources, 2);
        // Identical descriptors must reuse the four native resources instead
        // of needing more physical slots when CPU assets stay alive.
        assert_eq!(renderer.runtime().stats().resources_created, 4);
        for &mesh in &handles {
            assert!(assets.mesh_revision(mesh).is_ok());
        }
    }
    ready(&mut renderer, &snapshots[0]);
    let pinned = renderer
        .build_snapshot(&snapshots[0], false, |_, image| Ok(image))
        .unwrap();
    assert!(matches!(
        renderer.prepare_snapshot(&snapshots[1]),
        Err(super::EngineRenderError::Preparation(
            super::MeshPreparationError::ResidencyPinned
        ))
    ));
    drop(pinned);
    ready(&mut renderer, &snapshots[1]);
    drop(renderer);
    let report = monitor.report();
    assert_eq!(report.core_validation, Some(true), "{report:#?}");
    assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
    assert!(
        report.is_clean(),
        "validation through native destruction: {report:#?}"
    );
}
