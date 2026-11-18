//! Reusable mesh drawing and linear color-copy declarations.

use crate::ImageFormat;
use std::borrow::Borrow;

use crate::graph::{
    Attachment, Graph, GraphError, ImageHandle, PassHandle, RasterAttachments, RasterDraw,
};
use crate::program::ManagedRasterProgram;

/// Normal-Z depth behavior for standard opaque drawing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpaqueDepth {
    /// Use LESS and write passing depth values when a depth target is present.
    Write,
    /// Use LESS_OR_EQUAL without depth writes, loading a preceding depth pass.
    /// The caller must provide a depth attachment with [`crate::graph::LoadOp::Load`].
    Prepassed,
}

/// Draw unlit vertex colors multiplied by each draw's linear tint, in input
/// order, with blending disabled. The color attachment chooses RGBA8 or RGBA16F.
/// Attachment Load/Store operations and target selection belong to the caller.
/// Empty input retains those attachment operations without issuing draws.
///
/// Prepassed depth is required to load existing contents even for empty input.
/// Standard-specific contract failures use [`GraphError::custom`]; Graph handle,
/// attachment and capacity errors keep the ordinary Graph error categories.
pub fn opaque<I, D>(
    graph: &mut Graph,
    name: &str,
    draws: I,
    color: Attachment<[f32; 4]>,
    depth: Option<Attachment<f32>>,
    depth_mode: OpaqueDepth,
) -> Result<PassHandle, GraphError>
where
    I: IntoIterator<Item = D>,
    D: Borrow<RasterDraw>,
{
    let program = match (graph.image_desc(color.view.image())?.format, depth_mode) {
        (ImageFormat::Rgba8Unorm, OpaqueDepth::Write) => ManagedRasterProgram::mesh(),
        (ImageFormat::Rgba16Float, OpaqueDepth::Write) => ManagedRasterProgram::mesh_hdr(),
        (ImageFormat::Rgba8Unorm, OpaqueDepth::Prepassed) => ManagedRasterProgram::mesh_prepassed(),
        (ImageFormat::Rgba16Float, OpaqueDepth::Prepassed) => {
            ManagedRasterProgram::mesh_hdr_prepassed()
        }
        (ImageFormat::Depth32Float | ImageFormat::Rgba8Srgb, _) => {
            return Err(GraphError::custom(format!(
                "standard opaque '{name}': color target must use RGBA8 or RGBA16F"
            )));
        }
    };
    graph.raster(
        name,
        &program,
        draws,
        RasterAttachments {
            color_resolves: &[],
            colors: &[color],
            depth,
        },
    )
}

/// Draw only normal-Z depth with the standard mesh transform, LESS comparison
/// and depth writes. The caller chooses Clear/Load/Discard and Store/Discard.
/// Use matching transforms and compatible depth contents for subsequent opaque
/// drawing with [`OpaqueDepth::Prepassed`].
pub fn depth_prepass<I, D>(
    graph: &mut Graph,
    name: &str,
    draws: I,
    depth: Attachment<f32>,
) -> Result<PassHandle, GraphError>
where
    I: IntoIterator<Item = D>,
    D: Borrow<RasterDraw>,
{
    graph.raster(
        name,
        &ManagedRasterProgram::depth_only(),
        draws,
        RasterAttachments {
            color_resolves: &[],
            colors: &[],
            depth: Some(depth),
        },
    )
}

/// Copy complete linear color into an independent output of the same format and
/// extent. Supports RGBA8 and RGBA16F, without blending, exposure, tone mapping or
/// transfer-function conversion. This declares an ordinary [`Graph::copy_image`]
/// operation; the caller owns output allocation and any export or readback.
pub fn output(
    graph: &mut Graph,
    name: &str,
    source: ImageHandle,
    destination: ImageHandle,
) -> Result<PassHandle, GraphError> {
    graph.copy_image(name, source, destination)
}
