#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
use renderer_core::{
    graph::{GraphPool, ImageDesc, LoadOp, RasterRect, SampleCount},
    program::RasterPipelineRequest,
    runtime::{ResourceClass, Runtime, RuntimeConfig, WorkingSet},
};
use training_yard::canvas::{self, ImageLayer};

fn nested_canvas(config: RuntimeConfig) -> Result<(), Box<dyn std::error::Error>> {
    let icon_desc = ImageDesc::rgba8(1, 1);
    let preview_desc = ImageDesc::rgba8(2, 1);
    let screen_desc = ImageDesc::rgba8(4, 1);
    let mut runtime = Runtime::new(
        config,
        Default::default(),
        WorkingSet {
            resources: &[icon_desc, preview_desc, screen_desc]
                .map(|desc| ResourceClass::Image { desc, count: 1 }),
            raster: &[RasterPipelineRequest {
                program: canvas::program(),
                sample_count: SampleCount::X1,
            }],
            compute: &[],
        },
    )?;
    let mut pool = GraphPool::new(Default::default(), 1)?;
    let mut graph = pool.begin()?;
    let icon = graph.image("premultiplied icon", icon_desc)?;
    graph.upload_image("half red", icon, &[128, 0, 0, 128])?;
    let preview = graph.image("transparent UI panel", preview_desc)?;
    let layer = ImageLayer {
        source: graph.image_view(icon, 0, 0)?,
        rect: RasterRect::new(0, 0, 2, 1),
        clip: Some(RasterRect::new(1, 0, 1, 1)),
    };
    canvas::record(&mut graph, preview, LoadOp::Clear([0.; 4]), &[layer])?;
    let screen = graph.image("host background", screen_desc)?;
    graph.upload_image("blue", screen, &[0, 0, 255, 255].repeat(4))?;
    let layer = ImageLayer {
        source: graph.image_view(preview, 0, 0)?,
        rect: RasterRect::new(1, 0, 2, 1),
        clip: None,
    };
    canvas::record(&mut graph, screen, LoadOp::Load, &[layer])?;
    let capture = graph.readback_image("screen", screen)?;
    let frame = runtime.submit(&graph.compile()?)?;
    let mut result = runtime
        .wait_for_frame(frame.id(), std::time::Duration::from_secs(15))?
        .ok_or("canvas frame timed out")?;
    let mut pixels = [0; 16];
    result.readback_into(capture, &mut pixels)?;
    assert_eq!(
        pixels,
        [
            0, 0, 255, 255, 0, 0, 255, 255, 128, 0, 127, 255, 0, 0, 255, 255
        ]
    );
    Ok(())
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_nested_canvas() -> Result<(), Box<dyn std::error::Error>> {
    nested_canvas(RuntimeConfig::Vulkan(Default::default()))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_nested_canvas() -> Result<(), Box<dyn std::error::Error>> {
    nested_canvas(RuntimeConfig::D3D12(Default::default()))
}
