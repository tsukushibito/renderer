//! Standard PBR Forward with directional shadows and HDR output.
//!
//! Prepare the recipe before creating the Renderer. To insert an HDR pass,
//! call the scene and output stages separately.

use renderer::{
    capacity::Preparation,
    graph::{Graph, GraphError, ImageHandle, SampleCount},
    program::ManagedRasterProgram,
};

use crate::{
    passes::{
        output::OutputImages,
        pbr::{TransformTechnique, blend_program, opaque_program, shadow_program},
    },
    pbr::{
        Bounds, Environment, Frame, FrameImages, PreparedView, SceneImages, SceneLimits, Sun,
        ViewInput,
        scene::{DrawMode, PreparedScene},
    },
};

pub use crate::pbr::{
    Config,
    raster::{add_scene_passes, add_shadow_pass},
    scene::Programs,
};

impl Default for Programs {
    fn default() -> Self {
        Self {
            opaque: opaque_program(),
            blend: blend_program(),
            shadow: shadow_program(),
        }
    }
}

/// Prepared Forward programs, frame capacity and PBR scratch.
pub struct Prepared {
    common: PreparedScene,
}

pub fn prepare(config: Config, limits: SceneLimits) -> Result<Prepared, GraphError> {
    prepare_variant(config, limits, Programs::default(), DrawMode::Indirect)
}

/// Prepare Forward 1× with custom Programs for ordinary indexed draws.
pub fn prepare_with_programs(
    config: Config,
    limits: SceneLimits,
    programs: Programs,
) -> Result<Prepared, GraphError> {
    if config.samples != SampleCount::X1 {
        return Err(GraphError::invalid_input(
            "custom PBR Programs require Forward 1×",
        ));
    }
    let expected = Programs::default();
    for (actual, expected) in [
        (&programs.opaque, &expected.opaque),
        (&programs.blend, &expected.blend),
        (&programs.shadow, &expected.shadow),
    ] {
        validate_program(actual, expected)?;
    }
    prepare_variant(config, limits, programs, DrawMode::Direct)
}

fn prepare_variant(
    config: Config,
    limits: SceneLimits,
    programs: Programs,
    mode: DrawMode,
) -> Result<Prepared, GraphError> {
    Ok(Prepared {
        common: PreparedScene::new(
            config,
            limits,
            programs,
            true,
            mode,
            (mode == DrawMode::Indirect).then_some(TransformTechnique::Forward),
        )?,
    })
}

fn validate_program(
    actual: &ManagedRasterProgram,
    expected: &ManagedRasterProgram,
) -> Result<(), GraphError> {
    let actual = actual.descriptor();
    let expected = expected.descriptor();
    if actual.vertex_stride != expected.vertex_stride
        || actual.vertex_attributes != expected.vertex_attributes
        || actual.parameters != expected.parameters
        || actual.bindings != expected.bindings
        || actual.binding_bytes != expected.binding_bytes
        || actual.sampling_pairs != expected.sampling_pairs
        || actual.color_outputs != expected.color_outputs
        || actual.blend != expected.blend
        || actual.depth != expected.depth
    {
        return Err(GraphError::invalid_input(
            "Program ABI is incompatible with its PBR Forward role",
        ));
    }
    Ok(())
}

impl Prepared {
    pub fn preparation(&self) -> &Preparation {
        self.common.preparation()
    }

    /// Calculate view and cascade projections before selecting draws.
    pub fn prepare_view(
        &self,
        view: ViewInput,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        self.common.prepare_view(view, sun, bounds, environment)
    }

    /// Build shadows, scene and SDR output. Missing output or invalid exposure
    /// is rejected before declaring any work. For later declaration failures,
    /// discard this frame's Graph and start the next attempt with a reset Graph.
    pub fn add_passes(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
    ) -> Result<FrameImages, GraphError> {
        let (output, exposure) = self.common.checked_output(frame.exposure)?;
        let scene = self.add_scene(graph, frame)?;
        let output =
            output.append_validated(graph, graph.image_view(scene.hdr, 0, 0)?, exposure)?;
        Ok(FrameImages {
            hdr: scene.hdr,
            depth: scene.depth,
            sdr: output.sdr,
            shadows: scene.shadows,
            camera: scene.camera,
        })
    }

    /// Build the scene and return resolved HDR for further processing.
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        let Config {
            extent,
            clear_color,
            samples,
            ..
        } = self.common.config();
        self.common.add_scene_using(
            graph,
            frame,
            |_, _| Ok([]),
            |graph, scene| {
                add_scene_passes(
                    graph,
                    extent,
                    clear_color,
                    samples,
                    scene.has_transparency,
                    |pass| scene.draw_opaque(pass),
                    |pass| scene.draw_transparent(pass),
                )
            },
        )
    }

    /// Tone map and encode the scene's HDR result or a compatible custom HDR
    /// result with this recipe's prepared output settings.
    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        self.common.add_output(graph, hdr, exposure)
    }
}
