//! Public asset streaming, version publication and read-only mesh ownership.
#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "../examples/support/validation.rs"]
mod validation;

use renderer::{
    capacity::Preparation,
    graph::{GraphCapacity, GraphPool},
    runtime::{FrameId, Runtime, RuntimeConfig, RuntimeLimits},
};
use renderer_pipeline::{
    material::{Materials, UnlitParams},
    mesh::{MeshCapacity, MeshData, MeshHandle, Meshes},
    pipeline::RenderObject,
    unlit_forward::Vertex,
};
use std::{error::Error, num::NonZeroUsize, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn wait(runtime: &mut Runtime, frame: FrameId) -> Result<()> {
    runtime
        .wait_for_frame(frame, Duration::from_secs(15))?
        .ok_or("mesh frame timed out")?;
    Ok(())
}
fn upload(
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    meshes: &mut Meshes,
    mesh: MeshHandle,
) -> Result<()> {
    loop {
        let part = meshes.stream_gpu(runtime, pool, &[mesh], 128)?;
        assert!(part.submitted_bytes > 0 && part.submitted_bytes <= 128);
        wait(runtime, part.submission.ok_or("missing transfer")?)?;
        if part.remaining_bytes == 0 {
            assert_eq!(part.published_meshes, 1);
            return Ok(());
        }
        assert_eq!(part.published_meshes, 0);
    }
}
fn first_coordinate(
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    meshes: &Meshes,
    object: RenderObject,
) -> Result<f32> {
    let mut graph = pool.begin()?;
    let selected = object.import_mesh(&mut graph, meshes)?;
    assert_eq!((selected.first_index, selected.index_count), (3, 3));
    assert!(
        graph
            .upload_buffer("overwrite", selected.vertices, 0, &[0; 4])
            .is_err()
    );
    assert!(graph.export_buffer(selected.indices).is_err());
    let pass = graph.readback_buffer("first vertex x", selected.vertices, 0, 4)?;
    let frame = runtime.submit(&graph.compile()?)?;
    let mut completion = runtime
        .wait_for_frame(frame.id(), Duration::from_secs(15))?
        .ok_or("read timeout")?;
    let mut bytes = [0; 4];
    completion.readback_into(pass, &mut bytes)?;
    Ok(f32::from_le_bytes(bytes))
}
fn check(api: RuntimeConfig) -> Result<()> {
    let data = |x| {
        MeshData::unlit(
            &[Vertex {
                position: [x, 0., 0.],
                color: [1.; 4],
            }; 64],
            &[0, 1, 2, 3, 4, 5],
        )
    };
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 2,
        retired_mesh_versions: 4,
    })?;
    let mesh = meshes.create(data(1.)?)?;
    let other = meshes.create(data(9.)?)?;
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    meshes.reserve(&mut preparation)?;
    meshes.reserve_replacement(mesh, &mut preparation)?;
    let graph = preparation.graph_capacity(GraphCapacity::default())?;
    let mut runtime = Runtime::new(
        api,
        RuntimeLimits {
            graph,
            ..Default::default()
        },
        preparation.working_set(),
    )?;
    let mut pool = GraphPool::new(graph, 1)?;
    let object = RenderObject::new(
        mesh,
        Materials::default().create_unlit(UnlitParams::default())?,
    )
    .with_index_range(3..6)?;
    let bytes = meshes
        .stream_gpu(&mut runtime, &mut pool, &[mesh, mesh], 0)?
        .remaining_bytes;
    assert!(bytes > 1024);
    upload(&mut runtime, &mut pool, &mut meshes, mesh)?;
    assert_eq!(
        first_coordinate(&mut runtime, &mut pool, &meshes, object)?,
        1.
    );
    assert!(meshes.import(&mut pool.begin()?, other).is_err());

    meshes.update(mesh, data(2.)?)?;
    let partial = meshes.stream_gpu(&mut runtime, &mut pool, &[mesh], 129)?;
    assert_eq!(partial.submitted_bytes, 128);
    assert_eq!(partial.published_meshes, 0);
    wait(&mut runtime, partial.submission.unwrap())?;
    assert_eq!(
        first_coordinate(&mut runtime, &mut pool, &meshes, object)?,
        1.
    );
    // Replacing an in-progress upload restarts progress but keeps the drawable.
    meshes.update(mesh, data(3.)?)?;
    meshes.collect_gpu(&mut runtime)?;
    assert_eq!(
        meshes
            .stream_gpu(&mut runtime, &mut pool, &[mesh], 0)?
            .remaining_bytes,
        bytes
    );
    upload(&mut runtime, &mut pool, &mut meshes, mesh)?;
    assert_eq!(
        first_coordinate(&mut runtime, &mut pool, &meshes, object)?,
        3.
    );
    meshes.release_gpu(&mut runtime)?;
    validation::check_validation(runtime)
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a real Vulkan device"]
fn vulkan_streams_meshes_across_frames() -> Result<()> {
    check(RuntimeConfig::Vulkan(renderer::runtime::VulkanConfig {
        staging_bytes: 1024,
        ..Default::default()
    }))
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a real D3D12 device"]
fn d3d12_streams_meshes_across_frames() -> Result<()> {
    check(RuntimeConfig::D3D12(renderer::runtime::D3D12Config {
        upload_bytes: 1024,
        ..Default::default()
    }))
}
