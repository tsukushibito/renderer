//! Explicit measurement gates; use one test thread and an optimized build.
//! The standalone frame_memory example excludes libtest itself from the process.

#![cfg(feature = "memory-profiling")]

use renderer::measurement::CountingAllocator;
use std::alloc::System;

#[path = "../fixtures/image_transform.rs"]
mod image_transform;

#[path = "../fixtures/frame_memory.rs"]
mod frame_memory;

#[cfg(feature = "vulkan")]
#[path = "../fixtures/frame_memory_image.rs"]
mod frame_memory_image;

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

#[test]
#[ignore = "explicit optimized full CPU lifecycle allocation gate; RENDERER_MEMORY_BASELINE=1 records a nonzero baseline"]
fn frame_memory_cpu_full_lifecycle() {
    frame_memory::cpu_lifecycle(baseline());
    indirect_capacity_retries_are_allocation_free();
}

fn indirect_capacity_retries_are_allocation_free() {
    use renderer::graph::{
        BufferDesc, GraphCapacity, GraphCapacityKind, GraphPool, IndexedIndirectTable,
        IndexedIndirectTemplate,
    };
    use renderer::measurement::Snapshot;

    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 5,
            passes: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let table = IndexedIndirectTable::new(&[IndexedIndirectTemplate {
        first_index: 0,
        index_count: 3,
    }])
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    let visibility = graph
        .buffer("visibility", BufferDesc { size: 256 })
        .unwrap();

    let before = Snapshot::now();
    for _ in 0..64 {
        let error = graph
            .encode_indexed_indirect("encode", &table, vertices, indices, visibility, 0)
            .unwrap_err();
        assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Passes);
    }
    let output = graph.buffer("output", BufferDesc { size: 4 }).unwrap();
    graph.buffer("second slot", BufferDesc { size: 4 }).unwrap();
    graph.upload_buffer("recover", output, 0, &[0; 4]).unwrap();
    graph.export_buffer(output).unwrap();
    let compiled = graph.compile().unwrap();
    assert!(compiled.ordered_pass_names().eq(["recover"]));
    drop(compiled);
    let events = Snapshot::now().since(before);
    assert!(
        events.is_allocation_free(),
        "indirect retry events: {events:?}"
    );
}

fn baseline() -> bool {
    match std::env::var("RENDERER_MEMORY_BASELINE").as_deref() {
        Ok("1") => true,
        Err(std::env::VarError::NotPresent) => false,
        _ => panic!("RENDERER_MEMORY_BASELINE must be unset or 1"),
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "explicit Vulkan public buffer full-lifecycle memory gate, including held readbacks; native hooks required"]
fn frame_memory_vulkan_buffer_full_lifecycle() {
    frame_memory::vulkan_buffer_lifecycle(baseline());
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "explicit optimized Vulkan Engine/Raster/Compute image full-lifecycle memory gate; native hooks required"]
fn frame_memory_vulkan_engine_image_full_lifecycle() {
    frame_memory_image::lifecycle(baseline());
}

#[cfg(all(feature = "d3d12", windows))]
#[test]
#[ignore = "explicit D3D12 public buffer full-lifecycle memory gate, including held readbacks; native hooks required"]
fn frame_memory_d3d12_buffer_full_lifecycle() {
    frame_memory::d3d12_buffer_lifecycle(baseline());
}
