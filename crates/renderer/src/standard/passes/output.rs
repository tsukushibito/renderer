use crate::ImageFormat;
use crate::graph::{
    Attachment, Binding, BindingResource, Graph, GraphError, GraphErrorKind, ImageHandle,
    ImageView, LoadOp, PassHandle, RasterAttachments, RasterConstants, SampleCount, StoreOp,
};
use crate::program::{
    BindingLayout, BindingType, BlendState, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterPipelineRequest, RasterProgramDescriptor, SampledImageSamplerPair,
    SamplerDescriptor, ShaderStages, embedded_raster_shaders,
};

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
        let descriptor = match (self.encoding, self.fxaa) {
            (SdrEncoding::Linear, false) => &SDR_LINEAR,
            (SdrEncoding::Linear, true) => &SDR_FXAA_LINEAR,
            (SdrEncoding::Srgb, false) => &SDR_SRGB,
            (SdrEncoding::Srgb, true) => &SDR_FXAA_SRGB,
        };
        RasterPipelineRequest {
            // SAFETY: The checked-in modules use vertex IDs 0..2 and one
            // matching 2D float image/sampler. All samples are normalized,
            // use LOD zero and clamp at the edges. FXAA uses guarded arithmetic
            // and a bounded search. They write only color target zero.
            program: unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) },
            sample_count: SampleCount::X1,
        }
    }
}

/// The 1× pipeline request used by [`tone_map_neutral`] for any exposure.
pub const fn tone_map_neutral_pipeline() -> RasterPipelineRequest {
    RasterPipelineRequest {
        // SAFETY: The reviewed shaders use vertex IDs 0..2, the declared
        // fragment word, and bounded normalized LOD-zero sampling. Arithmetic
        // and the inline word do not affect the resource footprint. The
        // fragment writes only one float4 color.
        program: unsafe { ManagedRasterProgram::from_trusted_descriptor(&TONE_NEUTRAL) },
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
        return Err(GraphError::new(
            GraphErrorKind::InvalidInput,
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
        || !output.is_single_subresource()
        || output.format != format
        || input.mip_extent(source.mip_level()) != Some([output.width, output.height])
    {
        return Err(named(GraphError::new(
            GraphErrorKind::InvalidInput,
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

macro_rules! output_shaders {
    ($spirv:literal, $dxil:literal, $inline_constants:expr) => {
        embedded_raster_shaders!(
            "../../../shaders/composite.vert.spv",
            Some(include_bytes!($spirv)),
            "../../../shaders/dxil/composite.vert.dxil",
            Some(include_bytes!($dxil)),
            $inline_constants,
        )
    };
}

static SDR_SRGB: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 0,
    vertex_attributes: &[],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
    },
    color_outputs: &[ImageFormat::Rgba8Unorm],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
    ],
    binding_bytes: &[0, 0],
    sampling_pairs: &[SampledImageSamplerPair {
        image_group: 0,
        image_binding: 0,
        sampler_group: 0,
        sampler_binding: 1,
    }],
    blend: BlendState::Replace,
    depth: DepthState::Disabled,
    shaders: output_shaders!(
        "../../../shaders/sdr.frag.spv",
        "../../../shaders/dxil/sdr.frag.dxil",
        None
    ),
};
static SDR_LINEAR: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!(
        "../../../shaders/sdr_linear.frag.spv",
        "../../../shaders/dxil/sdr_linear.frag.dxil",
        None
    ),
    ..SDR_SRGB
};
static SDR_FXAA_SRGB: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!(
        "../../../shaders/sdr_fxaa.frag.spv",
        "../../../shaders/dxil/sdr_fxaa.frag.dxil",
        None
    ),
    ..SDR_SRGB
};
static SDR_FXAA_LINEAR: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!(
        "../../../shaders/sdr_fxaa_linear.frag.spv",
        "../../../shaders/dxil/sdr_fxaa_linear.frag.dxil",
        None
    ),
    ..SDR_SRGB
};
static TONE_NEUTRAL: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 4,
        stages: ShaderStages::FRAGMENT,
    },
    color_outputs: &[ImageFormat::Rgba16Float],
    shaders: output_shaders!(
        "../../../shaders/tone_neutral.frag.spv",
        "../../../shaders/dxil/tone_neutral.frag.dxil",
        Some(crate::program::D3D12InlineConstantBinding {
            register: 0,
            space: 0
        })
    ),
    ..SDR_SRGB
};
