//! Standard linear RGBA8 rendering for engine snapshots.

use crate::unlit_forward::{FrameSnapshot, View};

use sashiko_renderer_core::geometry::SnapshotInputs;
use sashiko_renderer_core::graph::FrameGraph;
use sashiko_renderer_core::graph::{Graph, GraphError, GraphPool, ImageDesc, ImageHandle};

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
    let output = append_view(&mut graph, snapshot.view(), inputs, extension)?;
    FrameGraph::select_output(graph, output, readback.then_some("view readback"))
}

sashiko_renderer_core::runtime::with_vulkan! {
    mod native_snapshot;
    pub use native_snapshot::{build_vulkan, render_vulkan};
}

fn append_view(
    graph: &mut Graph,
    view: &View,
    inputs: SnapshotInputs<'_>,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<ImageHandle, GraphError> {
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
