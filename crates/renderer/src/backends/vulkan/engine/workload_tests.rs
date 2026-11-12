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
        .get(&MeshVersionKey { mesh, revision: 1 })
        .unwrap();
    let vertices = resident.buffers.vertices;
    let indices = resident.buffers.indices;
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
