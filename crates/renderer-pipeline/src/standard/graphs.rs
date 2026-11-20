//! Optional drawing recipes assembled from public standard passes.

use std::borrow::Borrow;

use crate::preparation::checked_mul;
use crate::standard::passes::{self, OpaqueDepth, SdrOutput};
use crate::{GraphRequirements, Preparation};
use renderer::graph::{
    Attachment, Graph, GraphError, ImageDesc, ImageHandle, ImageView, LoadOp, RasterDraw,
    SampleCount, StoreOp,
};
use renderer::program::ManagedRasterProgram;

const FORWARD_COLOR: &str = "forward color";
const FORWARD_DEPTH: &str = "forward depth";
const DISPLAY_LINEAR: &str = "display linear";
const SDR_COLOR: &str = "sdr";
const TONE_MAP: &str = "tone_map";
const WRITE_SDR: &str = "write_sdr";

/// Per-construction target and clear choices. The recipe creates a single
/// subresource RGBA8 or RGBA16F color target and, when requested, matching D32
/// depth. Use [`PreparedForward`] to include its requirements in a working set.
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

/// An unlit Forward recipe with fixed targets and a prepared draw limit.
pub struct PreparedForward {
    name: Box<str>,
    config: ForwardConfig,
    program: ManagedRasterProgram,
    max_draws: usize,
}

impl PreparedForward {
    /// Register the images, Program variant and declarations for this recipe.
    /// Reserve the caller's vertex and index inputs separately.
    pub fn prepare(
        preparation: &mut Preparation,
        name: &str,
        config: ForwardConfig,
        max_draws: usize,
    ) -> Result<Self, GraphError> {
        let program = forward_program(name, config)?;
        let depth = forward_depth(config);
        let attachments = 1 + usize::from(depth.is_some());
        let images = [config.color, depth.unwrap_or(config.color)];
        let accesses = checked_mul(max_draws, 2)?
            .checked_add(attachments)
            .ok_or_else(|| GraphError::invalid_input("forward access capacity overflow"))?;
        let name_bytes = name
            .len()
            .checked_add(FORWARD_COLOR.len() + depth.map_or(0, |_| FORWARD_DEPTH.len()))
            .ok_or_else(|| GraphError::invalid_input("forward name capacity overflow"))?;
        preparation.add_recipe(
            GraphRequirements {
                resources: attachments,
                passes: 1,
                accesses,
                draws: max_draws,
                raster_constant_words: checked_mul(
                    max_draws,
                    program.parameter_size() as usize / size_of::<u32>(),
                )?,
                attachments,
                name_bytes,
                ..GraphRequirements::default()
            },
            &images[..attachments],
            &[program.clone().into()],
        )?;
        Ok(Self {
            name: name.into(),
            config,
            program,
            max_draws,
        })
    }

    /// Add the prepared recipe to an existing Graph. Empty draws still clear
    /// its targets. On error, abandon this frame's Graph to return its slot.
    pub fn add_passes(
        &self,
        graph: &mut Graph,
        draws: &[RasterDraw],
    ) -> Result<SceneColor, GraphError> {
        if draws.len() > self.max_draws {
            return Err(
                GraphError::invalid_input("draws exceed the prepared Forward limit")
                    .with_pass_name(&self.name),
            );
        }
        append_forward(graph, &self.name, draws, self.config, &self.program)
    }
}

/// Add standard opaque drawing and return its Graph-local color and depth.
/// The name identifies the Raster/clear pass. Pass the returned images to
/// additional Graph operations or select them for export or readback.
/// Failed construction may leave earlier declarations in the Graph.
pub fn forward<I, D>(
    graph: &mut Graph,
    name: &str,
    draws: I,
    config: ForwardConfig,
) -> Result<SceneColor, GraphError>
where
    I: IntoIterator<Item = D>,
    D: Borrow<RasterDraw>,
{
    let program = forward_program(name, config)?;
    append_forward(graph, name, draws, config, &program)
}

fn forward_program(name: &str, config: ForwardConfig) -> Result<ManagedRasterProgram, GraphError> {
    let invalid = || {
        GraphError::custom(format!(
            "standard forward '{name}': color must be a single-sample, single-subresource RGBA8 or RGBA16F image"
        ))
    };
    if config.color.mip_levels != 1
        || config.color.array_layers != 1
        || config.color.sample_count != SampleCount::X1
    {
        return Err(invalid());
    }
    passes::opaque_program(config.color.format, OpaqueDepth::Write).ok_or_else(invalid)
}

fn forward_depth(config: ForwardConfig) -> Option<ImageDesc> {
    config
        .depth
        .then(|| ImageDesc::depth32(config.color.width, config.color.height))
}

fn append_forward<I, D>(
    graph: &mut Graph,
    name: &str,
    draws: I,
    config: ForwardConfig,
    program: &ManagedRasterProgram,
) -> Result<SceneColor, GraphError>
where
    I: IntoIterator<Item = D>,
    D: Borrow<RasterDraw>,
{
    let color = graph.image(FORWARD_COLOR, config.color)?;
    let depth = forward_depth(config)
        .map(|desc| graph.image(FORWARD_DEPTH, desc))
        .transpose()?;
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
    passes::draw_opaque(
        graph,
        name,
        draws,
        color_attachment,
        depth_attachment,
        program,
    )?;
    Ok(SceneColor { color, depth })
}

/// Output images belonging to the Graph that produced them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputImages {
    pub display_linear: ImageHandle,
    pub sdr: ImageHandle,
}

/// Tone mapping and SDR output with fixed extent, encoding and FXAA choice.
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
        let tone_map = passes::tone_map_neutral_pipeline();
        let write_sdr = output.pipeline();
        preparation.add_recipe(
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
            &[display_linear, sdr],
            &[tone_map, write_sdr],
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
        let display_linear = graph.image(DISPLAY_LINEAR, self.display_linear)?;
        passes::tone_map_neutral(graph, TONE_MAP, source, display_linear, exposure)?;
        let sdr = graph.image(SDR_COLOR, self.sdr)?;
        let linear_view = graph.image_view(display_linear, 0, 0)?;
        passes::write_sdr(graph, WRITE_SDR, linear_view, sdr, self.output)?;
        Ok(OutputImages {
            display_linear,
            sdr,
        })
    }
}
