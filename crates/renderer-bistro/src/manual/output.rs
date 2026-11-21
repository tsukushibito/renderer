//! Bistro's fullscreen passes assembled from the public Renderer APIs.

use renderer_core::graph::{
    Attachment, Binding, BindingResource, Graph, GraphError, ImageHandle, ImageView, LoadOp,
    RasterAttachments, RasterConstants, StoreOp,
};
use renderer_core::program::{ManagedRasterProgram, SamplerDescriptor};

use renderer::passes::output::tone_map_neutral_pipeline;
pub(super) use renderer::passes::output::{SdrEncoding, SdrOutput};

pub(super) fn tone_map_neutral(
    graph: &mut Graph,
    scene_hdr: ImageView,
    display_linear: ImageHandle,
) -> Result<(), GraphError> {
    const EXPOSURE: f32 = 2.0;
    fullscreen(
        graph,
        "fixed exposure and PBR Neutral",
        scene_hdr,
        display_linear,
        tone_map_neutral_pipeline().program,
        SamplerDescriptor::NearestClamp,
        RasterConstants::new(&[EXPOSURE.to_bits()])?,
    )
}

pub(super) fn write_sdr(
    graph: &mut Graph,
    name: &str,
    display_linear: ImageView,
    target: ImageHandle,
    output: SdrOutput,
) -> Result<(), GraphError> {
    let sampler = if output.fxaa {
        SamplerDescriptor::LinearClamp
    } else {
        SamplerDescriptor::NearestClamp
    };
    fullscreen(
        graph,
        name,
        display_linear,
        target,
        output.pipeline().program,
        sampler,
        RasterConstants::default(),
    )
}

fn fullscreen(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    target: ImageHandle,
    program: ManagedRasterProgram,
    sampler: SamplerDescriptor,
    constants: RasterConstants,
) -> Result<(), GraphError> {
    let color = Attachment {
        view: graph.image_view(target, 0, 0)?,
        load: LoadOp::Clear([0.0; 4]),
        store: StoreOp::Store,
    };
    let bindings = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(source),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(sampler),
        },
    ];
    graph.raster(
        name,
        RasterAttachments {
            colors: &[color],
            color_resolves: &[],
            depth: None,
        },
        |pass| pass.draw_fullscreen(&program, &bindings, constants),
    )?;
    Ok(())
}
