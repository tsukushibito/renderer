//! Standard linear RGBA8 rendering for engine snapshots.

#[cfg(feature = "vulkan")]
use renderer::{EngineRenderError, PreparedEngineFrame, RenderedFrame, VulkanEngine};

use renderer::engine::{FrameGraph, FrameSnapshot, SnapshotInputs};
use renderer::graph::{Graph, GraphError, GraphPool, ImageDesc, ImageHandle};

use super::{ForwardConfig, forward};

/// Connect snapshot inputs, add the standard Forward recipe, and select one
/// exported output and optional readback. The extension can add work before
/// returning the original color or another image of the same description.
pub fn build_graph(
    snapshot: &FrameSnapshot,
    pool: &mut GraphPool,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<FrameGraph, GraphError> {
    let mut graph = pool.begin()?;
    let inputs = snapshot.connect_graph(&mut graph)?;
    let output = append_view(&mut graph, inputs, extension)?;
    FrameGraph::select_output(graph, output, readback.then_some("view readback"))
}

/// Build the same recipe with already resident mesh inputs. The prepared
/// frame retains the input pins through compilation and submission or drop.
#[cfg(feature = "vulkan")]
pub fn build_vulkan(
    engine: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<PreparedEngineFrame, EngineRenderError> {
    let mut prepared = engine.begin_graph()?;
    let inputs = engine.connect_snapshot(&mut prepared, snapshot)?;
    let output = append_view(&mut prepared.graph, inputs, extension)?;
    Ok(prepared.select_output(output, readback)?)
}

/// Build and submit the standard recipe with already resident mesh inputs.
#[cfg(feature = "vulkan")]
pub fn render_vulkan(
    engine: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<RenderedFrame, EngineRenderError> {
    let prepared = build_vulkan(engine, snapshot, readback, extension)?;
    engine.submit_frame(prepared.compile()?)
}

fn append_view(
    graph: &mut Graph,
    inputs: SnapshotInputs<'_>,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<ImageHandle, GraphError> {
    let view = inputs.view();
    let desc = ImageDesc::rgba8(view.width, view.height);
    let draws = inputs.draws();
    let name = if draws.len() == 0 {
        "view clear"
    } else {
        "view opaque batch"
    };
    let scene = forward(
        graph,
        name,
        draws,
        ForwardConfig {
            color: desc,
            clear_color: view.clear_color,
            depth: true,
        },
    )?;
    // Release connection scratch before the extension can reconnect the snapshot.
    drop(inputs);
    let output = extension(graph, scene.color)?;
    // Keep the source identity even when the extension returns a different image.
    let named = |error: GraphError| error.with_pass_name("view output");
    graph.image_desc(scene.color).map_err(named)?;
    if output != scene.color {
        graph.require_image_desc(output, desc).map_err(named)?;
    }
    Ok(output)
}
