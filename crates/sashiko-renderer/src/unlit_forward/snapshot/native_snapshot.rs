use super::{FrameSnapshot, Graph, GraphError, ImageHandle, append_view};
use sashiko_renderer_core::vulkan::{EngineRenderError, PreparedEngineFrame, RenderedFrame, VulkanEngine};

/// Build the same recipe with already resident mesh inputs. The prepared
/// frame retains the input pins through compilation and submission or drop.
pub fn build_vulkan(
    engine: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<PreparedEngineFrame, EngineRenderError> {
    let mut prepared = engine.begin_graph()?;
    let inputs = engine.connect_snapshot(&mut prepared, snapshot)?;
    let output = append_view(&mut prepared.graph, snapshot.view(), inputs, extension)?;
    Ok(prepared.select_output(output, readback)?)
}

/// Build and submit the standard recipe with already resident mesh inputs.
pub fn render_vulkan(
    engine: &mut VulkanEngine,
    snapshot: &FrameSnapshot,
    readback: bool,
    extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
) -> Result<RenderedFrame, EngineRenderError> {
    let prepared = build_vulkan(engine, snapshot, readback, extension)?;
    engine.submit_frame(prepared.compile()?)
}
