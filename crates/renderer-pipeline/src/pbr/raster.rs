use renderer::graph::{
    Attachment, Graph, GraphError, ImageDesc, ImageHandle, ImageView, LoadOp, RasterAttachments,
    RasterPass, SampleCount, StoreOp,
};

pub(crate) const SHADOW_IMAGES: [&str; 4] = ["cascade_0", "cascade_1", "cascade_2", "cascade_3"];
const SHADOW_PASSES: [&str; 4] = ["shadow_0", "shadow_1", "shadow_2", "shadow_3"];

/// Add a depth-only clear and the draws for a single shadow cascade.
///
/// Use the target's full D32 view and bind the appropriate cascade index in
/// the draw's inline parameters. The resulting depth is stored for sampling.
pub fn add_shadow_pass(
    graph: &mut Graph,
    cascade: usize,
    depth: ImageView,
    draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    let name = SHADOW_PASSES
        .get(cascade)
        .ok_or_else(|| GraphError::invalid_input("PBR shadow cascade index must be below four"))?;
    graph.raster(
        name,
        RasterAttachments {
            color_resolves: &[],
            colors: &[],
            depth: Some(Attachment {
                view: depth,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            }),
        },
        draws,
    )?;
    Ok(())
}

/// Add Forward attachment operations and caller-provided Raster draws.
///
/// Return single-sample HDR and, at 1×, single-sample depth. Under 4× MSAA
/// the final scope resolves color and discards depth. Transparent draws must
/// already be ordered by the caller.
pub fn add_scene_passes(
    graph: &mut Graph,
    [width, height]: [u32; 2],
    clear_color: [f32; 4],
    samples: SampleCount,
    has_transparency: bool,
    opaque_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
    transparent_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
) -> Result<(ImageHandle, Option<ImageHandle>), GraphError> {
    let color_desc = ImageDesc::rgba16_float(width, height);
    let hdr = graph.image("hdr", color_desc)?;
    let depth = graph.image(
        "depth",
        ImageDesc::depth32(width, height).with_sample_count(samples),
    )?;
    let depth_view = graph.image_view(depth, 0, 0)?;
    let hdr_view = graph.image_view(hdr, 0, 0)?;
    let (color_view, resolve) = if samples == SampleCount::X4 {
        let msaa = graph.image("hdr_msaa", color_desc.with_sample_count(samples))?;
        (graph.image_view(msaa, 0, 0)?, Some(hdr_view))
    } else {
        (hdr_view, None)
    };
    let clear_color = Attachment {
        view: color_view,
        load: LoadOp::Clear(clear_color),
        store: StoreOp::Store,
    };
    let clear_depth = Attachment {
        view: depth_view,
        load: LoadOp::Clear(1.0),
        store: StoreOp::Store,
    };
    let load_color = Attachment {
        load: LoadOp::Load,
        ..clear_color
    };
    let load_depth = Attachment {
        load: LoadOp::Load,
        ..clear_depth
    };

    if let Some(resolve) = resolve {
        let final_opaque = !has_transparency;
        let store = if final_opaque {
            StoreOp::Discard
        } else {
            StoreOp::Store
        };
        let resolves = [Some(resolve)];
        graph.raster(
            if final_opaque {
                "scene_opaque_resolve"
            } else {
                "scene_opaque"
            },
            RasterAttachments {
                color_resolves: if final_opaque { &resolves } else { &[] },
                colors: &[Attachment {
                    store,
                    ..clear_color
                }],
                depth: Some(Attachment {
                    store,
                    ..clear_depth
                }),
            },
            opaque_draws,
        )?;
    } else {
        graph.attachment_pass(
            "scene_clears",
            RasterAttachments {
                color_resolves: &[],
                colors: &[clear_color],
                depth: Some(clear_depth),
            },
        )?;
        graph.raster(
            "scene_opaque",
            RasterAttachments {
                color_resolves: &[],
                colors: &[load_color],
                depth: Some(load_depth),
            },
            opaque_draws,
        )?;
    }
    if has_transparency {
        let store = if resolve.is_some() {
            StoreOp::Discard
        } else {
            StoreOp::Store
        };
        let resolves = resolve.map(Some);
        graph.raster(
            if resolve.is_some() {
                "scene_transparent_resolve"
            } else {
                "scene_transparent"
            },
            RasterAttachments {
                color_resolves: resolves.as_slice(),
                colors: &[Attachment {
                    store,
                    ..load_color
                }],
                depth: Some(Attachment {
                    store,
                    ..load_depth
                }),
            },
            transparent_draws,
        )?;
    }
    Ok((hdr, (samples == SampleCount::X1).then_some(depth)))
}
