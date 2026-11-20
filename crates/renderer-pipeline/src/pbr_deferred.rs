//! Single-sample Deferred PBR with a typed G-buffer and Forward transparency.

pub mod passes;

use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{
        Attachment, BufferDesc, Graph, GraphError, ImageDesc, ImageHandle, LoadOp,
        RasterAttachments, SampleCount, StoreOp,
    },
};

use crate::{
    passes::{
        output::OutputImages,
        pbr::{
            CAMERA_BYTES, blend_program,
            deferred::{DEFERRED_RECONSTRUCTION_BYTES, deferred_lighting_program, gbuffer_program},
            shadow_program,
        },
    },
    pbr::{Bounds, Environment, Frame, PreparedView, SceneLimits, Sun, ViewInput},
    pbr_forward::{self, Programs, prepare_recipe},
};

pub use crate::pbr_forward::Config;
pub use passes::GBuffer;

/// The HDR scene and retained, Graph-local material planes.
#[derive(Clone, Copy, Debug)]
pub struct SceneImages {
    pub scene: crate::pbr::SceneImages,
    pub gbuffer: GBuffer,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameImages {
    pub scene: crate::pbr::FrameImages,
    pub gbuffer: GBuffer,
}

/// Prepared Deferred programs, frame capacities and PBR scratch.
pub struct Prepared {
    common: pbr_forward::Prepared,
    config: Config,
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
    let mut common = prepare_recipe(config, limits, programs, false)?;
    let preparation = common.preparation_mut();
    let [width, height] = config.extent;
    let frames = preparation.frames_in_flight().get();
    let planes = passes::PLANES.len();
    let plane_count = frames
        .checked_mul(planes)
        .ok_or_else(|| GraphError::invalid_input("Deferred G-buffer reservation overflow"))?;
    preparation.add_requirements(
        GraphRequirements {
            resources: planes + 1,
            passes: 2,
            accesses: 20,
            draws: 1,
            bindings: 16,
            attachments: planes + 1,
            name_bytes: passes::PLANES.iter().map(|name| name.len()).sum::<usize>()
                + "G-buffer opaque and mask".len()
                + "deferred inverse view projection".len()
                + "deferred reconstruction update".len()
                + "deferred PBR lighting".len(),
            upload_bytes: DEFERRED_RECONSTRUCTION_BYTES,
            ..GraphRequirements::default()
        },
        WorkingSet {
            resources: &[
                ResourceClass::Image {
                    desc: ImageDesc::rgba16_float(width, height),
                    count: plane_count,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc {
                        size: DEFERRED_RECONSTRUCTION_BYTES as u64,
                    },
                    count: frames,
                },
            ],
            raster: &[deferred_lighting_program().into()],
            ..WorkingSet::default()
        },
    )?;
    Ok(Prepared { common, config })
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
        let config = self.config;
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
        Ok(SceneImages {
            scene,
            gbuffer: gbuffer.expect("the scene callback records its G-buffer"),
        })
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
        let SceneImages { scene, gbuffer } = self.add_scene(graph, frame)?;
        let output =
            output.append_validated(graph, graph.image_view(scene.hdr, 0, 0)?, exposure)?;
        Ok(FrameImages {
            scene: crate::pbr::FrameImages {
                hdr: scene.hdr,
                depth: scene.depth,
                sdr: output.sdr,
                shadows: scene.shadows,
                camera: scene.camera,
            },
            gbuffer,
        })
    }
}
