use renderer::ImageFormat;
use renderer::graph::{
    Attachment, Binding, BindingResource, Graph, GraphError, ImageHandle, ImageView, LoadOp,
    PassHandle, RasterAttachments, RasterConstants, SampleCount, StoreOp,
};
use renderer::program::{ManagedRasterProgram, RasterPipelineRequest, SamplerDescriptor};

/// Encoding written to the RGBA8 UNORM output image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdrEncoding {
    /// Store linear RGB for a Presenter that encodes into its sRGB target.
    Linear,
    /// Store sRGB-encoded bytes for capture or transfer to an sRGB-aware consumer.
    Srgb,
}

/// Select the SDR output pipeline. Prepare each variant used by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SdrOutput {
    pub encoding: SdrEncoding,
    pub fxaa: bool,
}

impl SdrOutput {
    /// The 1× pipeline request used by [`write_sdr`] for this selection.
    pub const fn pipeline(self) -> RasterPipelineRequest {
        let program = match self.encoding {
            SdrEncoding::Linear => ManagedRasterProgram::sdr_linear(self.fxaa),
            SdrEncoding::Srgb => ManagedRasterProgram::sdr_srgb(self.fxaa),
        };
        RasterPipelineRequest {
            program,
            sample_count: SampleCount::X1,
        }
    }
}

/// The 1× pipeline request used by [`tone_map_neutral`] for any exposure.
pub const fn tone_map_neutral_pipeline() -> RasterPipelineRequest {
    RasterPipelineRequest {
        program: ManagedRasterProgram::tone_map_neutral(),
        sample_count: SampleCount::X1,
    }
}

/// Apply exposure and Khronos PBR Neutral to scene-linear Rec.709 RGB.
///
/// The source is one 1× RGBA16F mip. The destination is a distinct 1× RGBA16F
/// image with one mip and the same extent. This writes display-linear RGB in
/// 0..1 for finite input and alpha 1. Exposure is a positive, normal linear
/// multiplier at most `f32::MAX / 65536.0`. Prepare
/// [`tone_map_neutral_pipeline`] before normal frame processing.
pub fn tone_map_neutral(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    exposure: f32,
) -> Result<PassHandle, GraphError> {
    if !exposure.is_normal() || exposure <= 0.0 || exposure > f32::MAX / 65536.0 {
        return Err(GraphError::invalid_input(
            "Neutral exposure must be a positive normal multiplier at most f32::MAX / 65536",
        )
        .with_pass_name(name));
    }
    let color = output_view(graph, name, source, destination, ImageFormat::Rgba16Float)?;
    append(
        graph,
        name,
        source,
        color,
        tone_map_neutral_pipeline().program,
        SamplerDescriptor::NearestClamp,
        RasterConstants::new(&[exposure.to_bits()])?,
    )
}

/// Write display-linear RGB with optional FXAA and the selected SDR encoding.
///
/// The source is one 1× color mip. RGBA8 sRGB sources decode on sampling. The
/// destination is a distinct 1× RGBA8 UNORM image with one mip and the same
/// extent. Alpha is set to 1. Choose [`SdrEncoding::Linear`] for presentation
/// through the Renderer and prepare [`SdrOutput::pipeline`] for each selection.
pub fn write_sdr(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    output: SdrOutput,
) -> Result<PassHandle, GraphError> {
    let color = output_view(graph, name, source, destination, ImageFormat::Rgba8Unorm)?;
    let sampler = if output.fxaa {
        SamplerDescriptor::LinearClamp
    } else {
        SamplerDescriptor::NearestClamp
    };
    append(
        graph,
        name,
        source,
        color,
        output.pipeline().program,
        sampler,
        RasterConstants::default(),
    )
}

fn output_view(
    graph: &Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    format: ImageFormat,
) -> Result<ImageView, GraphError> {
    let named = |error: GraphError| error.with_pass_name(name);
    let input = graph.image_desc(source.image()).map_err(named)?;
    let output = graph.image_desc(destination).map_err(named)?;
    if source.image() == destination
        || source.mip_level_count() != 1
        || input.array_layers != 1
        || input.sample_count != SampleCount::X1
        || input.format == ImageFormat::Depth32Float
        || (format == ImageFormat::Rgba16Float && input.format != ImageFormat::Rgba16Float)
        || output.sample_count != SampleCount::X1
        || output.mip_levels != 1
        || output.array_layers != 1
        || output.format != format
        || input.mip_extent(source.mip_level()) != Some([output.width, output.height])
    {
        return Err(named(GraphError::invalid_input(
            "SDR output passes require distinct, matching-extent 1× color inputs and outputs of the supported formats",
        )));
    }
    graph.image_view(destination, 0, 0).map_err(named)
}

fn append(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    output: ImageView,
    program: ManagedRasterProgram,
    sampler: SamplerDescriptor,
    parameters: RasterConstants,
) -> Result<PassHandle, GraphError> {
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
            color_resolves: &[],
            colors: &[Attachment {
                view: output,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
        },
        |raster| raster.draw_fullscreen(&program, &bindings, parameters),
    )
}
