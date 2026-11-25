//! A host owns the graph, its target, submission and capture. A reusable output
//! pass can record into that target without owning a scene or an executor.

use sashiko_renderer::passes::output::{SdrOutput, write_sdr};
use sashiko_renderer_core::graph::{
    Graph, GraphCapacity, GraphError, GraphPool, ImageDesc, ImageHandle,
};

const EXTENT: ImageDesc = ImageDesc::rgba8(2, 1);
const PIXELS: [u8; 8] = [64, 0, 255, 255, 0, 128, 0, 255];

fn record_output(graph: &mut Graph, target: ImageHandle) -> Result<(), GraphError> {
    let source = graph.image("display-linear input", EXTENT)?;
    graph.upload_image("host pixels", source, &PIXELS)?;
    let view = graph.image_view(source, 0, 0)?;
    write_sdr(
        graph,
        "output transfer",
        view,
        target,
        SdrOutput::capture(false),
    )?;
    Ok(())
}

#[test]
fn host_can_abandon_a_bad_target_and_record_again() -> Result<(), GraphError> {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1)?;
    {
        let mut graph = pool.begin()?;
        let target = graph.image("wrong extent", ImageDesc::rgba8(1, 1))?;
        assert!(record_output(&mut graph, target).is_err());
    }
    let mut graph = pool.begin()?;
    let target = graph.image("host target", EXTENT)?;
    record_output(&mut graph, target)?;
    graph.readback_image("host capture", target)?;
    let compiled = graph.compile()?;
    assert_eq!(
        compiled.ordered_pass_names().collect::<Vec<_>>(),
        ["host pixels", "output transfer", "host capture"]
    );
    Ok(())
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn capture(
    config: sashiko_renderer_core::runtime::RuntimeConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use sashiko_renderer_core::{
        capacity::{ResourceClass, WorkingSet},
        runtime::Runtime,
    };
    use std::time::Duration;

    let mut runtime = Runtime::new(
        config,
        Default::default(),
        WorkingSet {
            samplers: &[SdrOutput::capture(false).sampler()],
            resources: &[ResourceClass::Image {
                desc: EXTENT,
                count: 2,
            }],
            raster: &[SdrOutput::capture(false).pipeline()],
            compute: &[],
        },
    )?;
    let mut pool = GraphPool::new(GraphCapacity::default(), 1)?;
    let mut graph = pool.begin()?;
    let target = graph.image("host target", EXTENT)?;
    record_output(&mut graph, target)?;
    let readback = graph.readback_image("host capture", target)?;
    let accepted = runtime.submit(&graph.compile()?)?;
    let mut completed = runtime
        .wait_for_frame(accepted.id(), Duration::from_secs(15))?
        .ok_or("host capture timed out")?;
    let mut pixels = [0; 8];
    completed.readback_into(readback, &mut pixels)?;
    let expected = [137, 0, 255, 255, 0, 188, 0, 255];
    assert!(
        pixels
            .iter()
            .zip(expected)
            .all(|(byte, expected)| byte.abs_diff(expected) <= 1),
        "{pixels:?}"
    );
    Ok(())
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_host_capture() -> Result<(), Box<dyn std::error::Error>> {
    capture(sashiko_renderer_core::runtime::RuntimeConfig::Vulkan(
        Default::default(),
    ))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_host_capture() -> Result<(), Box<dyn std::error::Error>> {
    capture(sashiko_renderer_core::runtime::RuntimeConfig::D3D12(
        Default::default(),
    ))
}
