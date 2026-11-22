//! Single-sample Deferred PBR with a typed G-buffer and Forward transparency.

pub mod passes;

use renderer_core::{
    capacity::Preparation,
    graph::{
        Attachment, Graph, GraphError, ImageDesc, ImageHandle, LoadOp, RasterAttachments,
        SampleCount, StoreOp,
    },
};

use crate::{
    passes::{
        output::OutputImages,
        pbr::{
            CAMERA_BYTES, TransformTechnique, blend_program,
            deferred::{DEFERRED_RECONSTRUCTION_BYTES, deferred_lighting_program, gbuffer_program},
            shadow_program,
        },
    },
    pbr::{
        Bounds, Environment, FrameImages, PbrDrawInput, PreparedView, SceneImages, SceneLimits,
        Sun, ViewInput,
        scene::{DrawMode, PreparedScene, Programs},
    },
};

pub use crate::pbr::Config;
pub use passes::GBuffer;

/// Prepared Deferred programs, frame capacities and PBR scratch.
pub struct Prepared {
    pub(crate) common: PreparedScene,
}

/// Prepare a 1× Deferred scene with a three-plane RGBA16F G-buffer and D32
/// depth. Opaque and alpha-mask draws contribute material data. Transparent
/// draws retain caller order in a later Forward pass.
pub fn prepare(config: Config, limits: SceneLimits) -> Result<Prepared, GraphError> {
    if config.samples != SampleCount::X1 {
        return Err(GraphError::invalid_input(
            "Deferred PBR requires single-sample targets",
        ));
    }
    let programs = Programs {
        opaque: gbuffer_program(),
        blend: blend_program(),
        shadow: shadow_program(),
    };
    let mut common = PreparedScene::new(
        config,
        limits,
        programs,
        false,
        DrawMode::Indirect,
        Some(TransformTechnique::Deferred),
    )?;
    passes::reserve_passes(
        common.preparation_mut(),
        config.extent,
        true,
        limits.max_point_lights > 0,
    )?;
    Ok(Prepared { common })
}

impl Prepared {
    pub fn preparation(&self) -> &Preparation {
        self.common.preparation()
    }

    pub fn prepare_view(
        &self,
        view: ViewInput,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        self.common.prepare_view(view, sun, bounds, environment)
    }

    /// Declare the shadows, G-buffer, full-screen lighting and transparent
    /// scene. The depth in the G-buffer is shared with the Forward pass.
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: PbrDrawInput<'_>,
    ) -> Result<SceneImages, GraphError> {
        self.add_scene_with_gbuffer(graph, frame, |_, _| Ok(()))
    }

    /// Append a G-buffer consumer after lighting and before Forward composition.
    /// A caller that needs geometry depth later can copy it in this callback.
    pub fn add_scene_with_gbuffer(
        &mut self,
        graph: &mut Graph,
        frame: PbrDrawInput<'_>,
        consume: impl FnOnce(&mut Graph, GBuffer) -> Result<(), GraphError>,
    ) -> Result<SceneImages, GraphError> {
        let config = self.common.config();
        let [width, height] = config.extent;
        let view_projection = *frame.view.matrix();
        let mut gbuffer = None;
        let clear = frame.view.clear_override.unwrap_or(config.clear_color);
        let scene = self.common.add_scene_using(graph, frame, |graph, scene| {
            let hdr = graph.image("hdr", ImageDesc::rgba16_float(width, height))?;
            let depth = graph.image("depth", ImageDesc::depth32(width, height))?;
            let hdr_view = graph.image_view(hdr, 0, 0)?;
            let depth_view = graph.image_view(depth, 0, 0)?;
            let views = passes::append(
                graph,
                passes::Inputs {
                    hdr: hdr_view,
                    depth: depth_view,
                    clear,
                    camera: scene.camera,
                    view_projection,
                    lighting: scene.lighting,
                    point_lights: scene.point_lights,
                },
                |pass, _| scene.draw_pbr_opaque(pass),
            )?;
            consume(graph, views)?;
            if scene.has_transparency || scene.has_forward_opaque() {
                graph.raster(
                    if scene.has_forward_opaque() {
                        "scene_forward"
                    } else {
                        "scene_transparent"
                    },
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[Attachment {
                            view: hdr_view,
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        }],
                        depth: Some(Attachment {
                            view: depth_view,
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        }),
                    },
                    |pass| {
                        scene.draw_forward_opaque(pass)?;
                        scene.draw_transparent(pass)
                    },
                )?;
            }
            gbuffer = (!scene.has_forward_opaque()).then_some(views);
            Ok((hdr, Some(depth)))
        })?;
        Ok(SceneImages { gbuffer, ..scene })
    }

    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        self.common.add_output(graph, hdr, exposure)
    }

    /// Append the scene and SDR output. Missing output or invalid exposure is
    /// rejected before declaring any work. Discard the Graph on later errors.
    pub fn add_scene_and_sdr_output(
        &mut self,
        graph: &mut Graph,
        frame: PbrDrawInput<'_>,
        exposure: f32,
    ) -> Result<FrameImages, GraphError> {
        let (output, exposure) = self.common.checked_output(exposure)?;
        let scene = self.add_scene(graph, frame)?;
        let output =
            output.append_validated(graph, graph.image_view(scene.hdr, 0, 0)?, exposure)?;
        Ok(scene.with_sdr(output.sdr))
    }
}
