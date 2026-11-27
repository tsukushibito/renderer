//! Tone mapping and SDR output passes and recipe.

#[allow(unsafe_code)]
mod programs;
pub(crate) use programs::film_composite as film_program;

/// Perspective ray basis in light space and a linear RGB camera response.
/// Ray coordinates are top-origin pixel centres; the scale is in cd/m²
/// per unit equal-area light texel. The sensor contains three matrix rows.
pub struct FilmOutput<'a> {
    pub sky: &'a sashiko_renderer_core::graph::ImportImage,
    pub ray_corner: [f32; 3],
    pub ray_step_x: [f32; 3],
    pub ray_step_y: [f32; 3],
    pub light_scale: f32,
    pub sensor: [[f32; 3]; 3],
}

pub(crate) fn film_composite(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    film: FilmOutput<'_>,
) -> Result<PassHandle, GraphError> {
    let color = output_view(graph, name, source, destination, ImageFormat::Rgba16Float)?;
    let sky = crate::material::texture_view(graph, film.sky)?;
    let bindings = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(source),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::ImageView(sky),
        },
    ];
    let rows = [
        film.ray_corner,
        film.ray_step_x,
        film.ray_step_y,
        film.sensor[0],
        film.sensor[1],
        film.sensor[2],
    ];
    let mut words = [0; 24];
    for (src, dst) in rows.into_iter().zip(words.as_chunks_mut::<4>().0) {
        dst[..3].copy_from_slice(&src.map(f32::to_bits));
    }
    words[3] = film.light_scale.to_bits();
    let parameters = RasterConstants::new(&words)?;
    graph.raster(
        name,
        RasterAttachments {
            colors: &[Attachment {
                view: color,
                load: LoadOp::Clear([0., 0., 0., 1.]),
                store: StoreOp::Store,
            }],
            depth: None,
            color_resolves: &[],
        },
        |pass| pass.draw_fullscreen(&film_program(), &bindings, parameters),
    )
}

use sashiko_renderer_core::ImageFormat;
use sashiko_renderer_core::capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet};
use sashiko_renderer_core::graph::{
    Attachment, Binding, BindingResource, Graph, GraphError, ImageDesc, ImageHandle, ImageView,
    LoadOp, PassHandle, RasterAttachments, RasterConstants, SampleCount, StoreOp,
};
use sashiko_renderer_core::program::{
    ManagedRasterProgram, RasterPipelineRequest, SamplerDescriptor,
};
const DISPLAY_LINEAR: &str = "display linear";
const SDR_COLOR: &str = "sdr";
const TONE_MAP: &str = "tone_map";
const WRITE_SDR: &str = "write_sdr";

/// Sampler used by the Neutral tone-mapping pass.
pub const TONE_MAP_SAMPLER: SamplerDescriptor = SamplerDescriptor::NearestClamp;

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
    /// Store display-linear bytes for the window Presenter.
    pub const fn display(fxaa: bool) -> Self {
        Self {
            encoding: SdrEncoding::Linear,
            fxaa,
        }
    }

    /// Store sRGB-encoded bytes for readback.
    pub const fn capture(fxaa: bool) -> Self {
        Self {
            encoding: SdrEncoding::Srgb,
            fxaa,
        }
    }

    /// The 1× pipeline request used by [`write_sdr`] for this selection.
    pub const fn pipeline(self) -> RasterPipelineRequest {
        RasterPipelineRequest {
            program: programs::sdr(self),
            sample_count: SampleCount::X1,
        }
    }

    /// Sampler used by [`write_sdr`] for this selection.
    pub const fn sampler(self) -> SamplerDescriptor {
        if self.fxaa {
            SamplerDescriptor::LinearClamp
        } else {
            SamplerDescriptor::NearestClamp
        }
    }
}

/// The 1× pipeline request used by [`tone_map_neutral`] for any exposure.
pub const fn tone_map_neutral_pipeline() -> RasterPipelineRequest {
    RasterPipelineRequest {
        program: programs::tone_map_neutral(),
        sample_count: SampleCount::X1,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Exposure(f32);

impl Exposure {
    fn new(value: f32, pass: &str) -> Result<Self, GraphError> {
        if !value.is_normal() || value <= 0.0 || value > f32::MAX / 65536.0 {
            return Err(GraphError::invalid_input(
                "Neutral exposure must be a positive normal multiplier at most f32::MAX / 65536",
            )
            .with_pass_name(pass));
        }
        Ok(Self(value))
    }
}

/// Apply exposure and Khronos PBR Neutral to scene-linear Rec.709 RGB.
///
/// The source is one 1× RGBA16F mip. The destination is a distinct 1× RGBA16F
/// image with one mip and the same extent. This writes display-linear RGB in
/// 0..1 for finite input and alpha 1. Exposure is a positive, normal linear
/// multiplier at most `f32::MAX / 65536.0`. Prepare
/// [`tone_map_neutral_pipeline`] and [`TONE_MAP_SAMPLER`] before normal frame processing.
pub fn tone_map_neutral(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    exposure: f32,
) -> Result<PassHandle, GraphError> {
    let exposure = Exposure::new(exposure, name)?;
    tone_map(graph, name, source, destination, exposure)
}

fn tone_map(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    exposure: Exposure,
) -> Result<PassHandle, GraphError> {
    let color = output_view(graph, name, source, destination, ImageFormat::Rgba16Float)?;
    append(
        graph,
        name,
        source,
        color,
        tone_map_neutral_pipeline().program,
        TONE_MAP_SAMPLER,
        RasterConstants::new(&[exposure.0.to_bits()])?,
    )
}

/// Write display-linear RGB with optional FXAA and the selected SDR encoding.
///
/// The source is one 1× color mip. RGBA8 sRGB sources decode on sampling. The
/// destination is a distinct 1× RGBA8 UNORM image with one mip and the same
/// extent. Alpha is set to 1. Choose [`SdrEncoding::Linear`] for presentation
/// through the Renderer and prepare [`SdrOutput::pipeline`] and [`SdrOutput::sampler`] for each selection.
pub fn write_sdr(
    graph: &mut Graph,
    name: &str,
    source: ImageView,
    destination: ImageHandle,
    output: SdrOutput,
) -> Result<SdrImage, GraphError> {
    let color = output_view(graph, name, source, destination, ImageFormat::Rgba8Unorm)?;
    append(
        graph,
        name,
        source,
        color,
        output.pipeline().program,
        output.sampler(),
        RasterConstants::default(),
    )?;
    Ok(match output.encoding {
        SdrEncoding::Linear => SdrImage::Display(DisplaySdr(destination)),
        SdrEncoding::Srgb => SdrImage::Capture(CaptureSdr(destination)),
    })
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
                // Fullscreen output always writes alpha one. This clear can
                // also use the existing D3D12 native attachment clear.
                load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                store: StoreOp::Store,
            }],
            depth: None,
        },
        |raster| raster.draw_fullscreen(&program, &bindings, parameters),
    )
}

/// Graph-local SDR output with its transfer function preserved in the type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdrImage {
    /// Display-linear RGB for a Presenter with an sRGB target.
    Display(DisplaySdr),
    /// sRGB-encoded bytes for capture.
    Capture(CaptureSdr),
}

impl SdrImage {
    pub const fn display(self) -> Option<DisplaySdr> {
        match self {
            Self::Display(image) => Some(image),
            Self::Capture(_) => None,
        }
    }

    pub const fn capture(self) -> Option<CaptureSdr> {
        match self {
            Self::Capture(image) => Some(image),
            Self::Display(_) => None,
        }
    }

    /// Access the image for custom Graph operations. Both variants use
    /// RGBA8 UNORM, so the caller must preserve its encoding.
    pub const fn raw_image(self) -> ImageHandle {
        match self {
            Self::Display(image) => image.raw_image(),
            Self::Capture(image) => image.raw_image(),
        }
    }
}

/// Graph-local display-linear SDR. Select it for presentation before submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplaySdr(ImageHandle);

impl DisplaySdr {
    /// Access the linear image for custom Graph operations.
    pub const fn raw_image(self) -> ImageHandle {
        self.0
    }

    /// Select the frame image for WindowPresenter::queue_frame after submission.
    /// A transient output is released automatically when discarded or presented.
    pub fn export_for_display(self, graph: &mut Graph) -> Result<(), GraphError> {
        graph.export_frame_image(self.0)
    }
}

/// Graph-local sRGB-encoded SDR for readback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureSdr(ImageHandle);

impl CaptureSdr {
    /// Access the encoded image for custom Graph operations.
    pub const fn raw_image(self) -> ImageHandle {
        self.0
    }

    pub fn readback(self, graph: &mut Graph, name: &str) -> Result<PassHandle, GraphError> {
        graph.readback_image(name, self.0)
    }
}

/// Output images belonging to the Graph that produced them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputImages {
    pub display_linear: ImageHandle,
    pub sdr: SdrImage,
}

/// Tone mapping and SDR output with fixed extent, encoding and FXAA choice.
#[derive(Clone, Copy)]
pub struct PreparedOutput {
    display_linear: ImageDesc,
    sdr: ImageDesc,
    output: SdrOutput,
}

impl PreparedOutput {
    pub fn prepare(
        preparation: &mut Preparation,
        [width, height]: [u32; 2],
        output: SdrOutput,
    ) -> Result<Self, GraphError> {
        let display_linear = ImageDesc::rgba16_float(width, height);
        let sdr = ImageDesc::rgba8(width, height);
        let count = preparation.frames_in_flight().get();
        let resources = [display_linear, sdr].map(|desc| ResourceClass::Image { desc, count });
        let tone_map = tone_map_neutral_pipeline();
        let write_sdr = output.pipeline();
        preparation.add_requirements(
            GraphRequirements {
                resources: 2,
                passes: 2,
                accesses: 4,
                draws: 2,
                raster_constant_words: (tone_map.program.parameter_size()
                    + write_sdr.program.parameter_size())
                    as usize
                    / size_of::<u32>(),
                bindings: tone_map.program.bindings().len() + write_sdr.program.bindings().len(),
                attachments: 2,
                name_bytes: TONE_MAP.len()
                    + WRITE_SDR.len()
                    + DISPLAY_LINEAR.len()
                    + SDR_COLOR.len(),
                ..GraphRequirements::default()
            },
            WorkingSet {
                resources: &resources,
                raster: &[tone_map, write_sdr],
                samplers: &[TONE_MAP_SAMPLER, output.sampler()],
                ..WorkingSet::default()
            },
        )?;
        Ok(Self {
            display_linear,
            sdr,
            output,
        })
    }

    /// Append Neutral tone mapping and SDR encoding to the same Graph.
    /// The source is a scene-linear, matching extent 1× RGBA16F mip.
    /// On error, abandon this frame's Graph to return its slot.
    pub fn add_passes(
        &self,
        graph: &mut Graph,
        source: ImageView,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        self.append_validated(graph, source, Self::validate_exposure(exposure)?)
    }

    pub(crate) fn validate_exposure(exposure: f32) -> Result<Exposure, GraphError> {
        Exposure::new(exposure, TONE_MAP)
    }

    pub(crate) fn append_validated(
        &self,
        graph: &mut Graph,
        source: ImageView,
        exposure: Exposure,
    ) -> Result<OutputImages, GraphError> {
        self.append_to(graph, source, exposure, None)
    }

    pub(crate) fn append_to(
        &self,
        graph: &mut Graph,
        source: ImageView,
        exposure: Exposure,
        target: Option<ImageHandle>,
    ) -> Result<OutputImages, GraphError> {
        let display_linear = graph.image(DISPLAY_LINEAR, self.display_linear)?;
        tone_map(graph, TONE_MAP, source, display_linear, exposure)?;
        let sdr = match target {
            Some(image) => image,
            None => graph.image(SDR_COLOR, self.sdr)?,
        };
        let linear_view = graph.image_view(display_linear, 0, 0)?;
        let sdr = write_sdr(graph, WRITE_SDR, linear_view, sdr, self.output)?;
        Ok(OutputImages {
            display_linear,
            sdr,
        })
    }
}
