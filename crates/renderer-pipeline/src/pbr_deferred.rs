//! Single-sample Deferred PBR with a typed G-buffer and Forward transparency.

pub mod passes;

use renderer::{
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
        Bounds, Environment, Frame, FrameImages, PreparedView, SceneImages, SceneLimits, Sun,
        ViewInput,
        scene::{DrawMode, PreparedScene, Programs},
    },
};

pub use crate::pbr::Config;
pub use passes::GBuffer;

/// Prepared Deferred programs, frame capacities and PBR scratch.
pub struct Prepared {
    common: PreparedScene,
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
    passes::reserve_passes(common.preparation_mut(), config.extent, true)?;
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
        frame: Frame<'_>,
    ) -> Result<SceneImages, GraphError> {
        let config = self.common.config();
        let [width, height] = config.extent;
        let view_projection = *frame.view.matrix();
        let mut gbuffer = None;
        let scene = self.common.add_scene_using(
            graph,
            frame,
            |_, _| Ok([]),
            |graph, scene| {
                let hdr = graph.image("hdr", ImageDesc::rgba16_float(width, height))?;
                let depth = graph.image("depth", ImageDesc::depth32(width, height))?;
                let hdr_view = graph.image_view(hdr, 0, 0)?;
                let depth_view = graph.image_view(depth, 0, 0)?;
                let views = passes::append(
                    graph,
                    passes::Inputs {
                        hdr: hdr_view,
                        depth: depth_view,
                        clear: config.clear_color,
                        camera: scene.camera,
                        view_projection,
                        lighting: scene.lighting,
                    },
                    |pass, _| scene.draw_opaque(pass),
                )?;
                if scene.has_transparency {
                    graph.raster(
                        "scene_transparent",
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
                        |pass| scene.draw_transparent(pass),
                    )?;
                }
                gbuffer = Some(views);
                Ok((hdr, Some(depth)))
            },
        )?;
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
            gbuffer: scene.gbuffer,
        })
    }
}
