//! Optional forward recipes assembled from public standard passes.

use std::borrow::Borrow;

use crate::graph::{
    Attachment, Graph, GraphError, ImageDesc, ImageFormat, ImageHandle, LoadOp, RasterDraw, StoreOp,
};
use crate::standard::passes::{self, OpaqueDepth};

/// Per-construction target and clear choices. The recipe creates a single
/// subresource RGBA8 or RGBA16F color target and, when requested, matching D32
/// depth. Device-specific resources and pipelines must be prepared separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ForwardConfig {
    pub color: ImageDesc,
    pub clear_color: [f32; 4],
    pub depth: bool,
}

/// Unlit scene-linear color, before exposure, tone mapping or presentation.
/// Values follow the caller's linear vertex colors and tints: RGBA16F preserves
/// HDR values, while RGBA8 converts to UNORM. Optional depth uses normal-Z device
/// depth in 0..1, cleared to 1. No normal buffer or lighting is implied.
///
/// Handles belong to the current Graph; returning one does not export its
/// contents, execute GPU work, or establish GPU completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneColor {
    pub color: ImageHandle,
    pub depth: Option<ImageHandle>,
}

/// Add standard opaque drawing and return its color/depth without a post stage.
/// The name identifies the Raster/clear pass. No export, readback, submission or
/// redundant final copy is added.
pub fn forward<'a, I, D>(
    graph: &mut Graph,
    name: &str,
    draws: I,
    config: ForwardConfig,
) -> Result<SceneColor, GraphError>
where
    I: IntoIterator<Item = D>,
    D: Borrow<RasterDraw<'a>>,
{
    forward_with_post(graph, name, draws, config, |_, scene| Ok(scene.color))
}

/// Add standard opaque drawing, then immediately call a scene-linear post
/// builder. The closure may borrow frame inputs and add multiple ordinary Graph
/// operations. It must return color of the same complete description; contents
/// validity is checked by Graph compilation when a subsequent operation or
/// export consumes that output. Returned depth is the original optional depth.
///
/// This is one semantic insertion point, not a deferred GPU callback. To change
/// the pass structure, compose public passes directly. Failed composition may
/// leave earlier successful declarations in the Graph; drop/rebuild as needed.
/// Standard-specific contract failures use [`GraphError::custom`].
pub fn forward_with_post<'a, I, D>(
    graph: &mut Graph,
    name: &str,
    draws: I,
    config: ForwardConfig,
    post: impl FnOnce(&mut Graph, SceneColor) -> Result<ImageHandle, GraphError>,
) -> Result<SceneColor, GraphError>
where
    I: IntoIterator<Item = D>,
    D: Borrow<RasterDraw<'a>>,
{
    if !matches!(
        config.color.format,
        ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float
    ) || config.color.mip_levels != 1
        || config.color.array_layers != 1
    {
        return Err(GraphError::custom(format!(
            "standard forward '{name}': color must be a single-subresource RGBA8 or RGBA16F image"
        )));
    }
    let color = graph.image("forward color", config.color)?;
    let depth = if config.depth {
        Some(graph.image(
            "forward depth",
            ImageDesc::depth32(config.color.width, config.color.height),
        )?)
    } else {
        None
    };
    let color_attachment = Attachment {
        view: graph.image_view(color, 0, 0)?,
        load: LoadOp::Clear(config.clear_color),
        store: StoreOp::Store,
    };
    let depth_attachment = if let Some(depth) = depth {
        Some(Attachment {
            view: graph.image_view(depth, 0, 0)?,
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        })
    } else {
        None
    };
    passes::opaque(
        graph,
        name,
        draws,
        color_attachment,
        depth_attachment,
        OpaqueDepth::Write,
    )?;
    let scene = SceneColor { color, depth };
    let output = post(graph, scene)?;
    // A post builder must extend this Graph, including when it returns a new
    // target. Replacing it would leave the original depth and scene handles
    // attached to a different Graph identity.
    graph.image_desc(scene.color)?;
    if output != scene.color && graph.image_desc(output)? != config.color {
        return Err(GraphError::custom(format!(
            "standard forward '{name}': post output must preserve scene color format and extent"
        )));
    }
    Ok(SceneColor {
        color: output,
        depth,
    })
}
