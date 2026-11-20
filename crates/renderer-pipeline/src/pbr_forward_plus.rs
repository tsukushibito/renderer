//! Forward PBR with per-tile masks for world-space point lights.

use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{BufferDesc, BufferHandle, Graph, GraphError, ImageDesc},
    program::RasterPipelineRequest,
};

use crate::{
    passes::{
        output::OutputImages,
        pbr::{forward_plus::tiled_raster_bindings, shadow_program},
    },
    pbr::{
        Bounds, Environment, Frame, FrameImages, PreparedView, SceneImages, SceneLimits, Sun,
        ViewInput,
        raster::add_scene_passes,
        scene::{DrawMode, PreparedScene, Programs, RasterScene},
    },
};

pub use crate::passes::pbr::forward_plus::{
    LIGHT_BUFFER_BYTES, MAX_LIGHTS, MAX_TILES, PointLight, TILE_SIZE, active_tile_count,
    append_culling, compute_program, light_bytes, raster_bindings, raster_program, tile_mask_desc,
};
pub use crate::pbr::Config;

const LIGHTS: BufferDesc = BufferDesc {
    size: LIGHT_BUFFER_BYTES,
};

/// Prepared Forward+ programs, frame capacities and PBR scratch.
pub struct Prepared {
    common: PreparedScene,
    mask_desc: ImageDesc,
    fallback_programs: Programs,
}

/// Prepare a 1× or 4× Forward+ recipe and its transient tile masks.
/// Frames with no point lights use the prepared ordinary Forward programs.
pub fn prepare(config: Config, limits: SceneLimits) -> Result<Prepared, GraphError> {
    let [width, height] = config.extent;
    let mask_desc = tile_mask_desc(width, height)?;
    let programs = Programs {
        opaque: raster_program(true, false),
        blend: raster_program(true, true),
        shadow: shadow_program(),
    };
    let mut common = PreparedScene::new(config, limits, programs, true, DrawMode::Indirect)?;
    let preparation = common.preparation_mut();
    let count = preparation.frames_in_flight().get();
    let fallback_programs = Programs::default();
    let fallback = [&fallback_programs.opaque, &fallback_programs.blend].map(|program| {
        RasterPipelineRequest {
            program: program.clone(),
            sample_count: config.samples,
        }
    });
    preparation.add_requirements(
        GraphRequirements {
            resources: 2,
            passes: 2,
            compute_dispatches: 1,
            accesses: 4,
            bindings: 3,
            name_bytes: "local point lights".len()
                + "local point light update".len()
                + "tile light masks".len()
                + "Forward+ tile light masks".len(),
            upload_bytes: LIGHT_BUFFER_BYTES as usize,
            ..GraphRequirements::default()
        },
        WorkingSet {
            resources: &[
                ResourceClass::Buffer {
                    desc: LIGHTS,
                    count,
                },
                ResourceClass::Image {
                    desc: mask_desc,
                    count,
                },
            ],
            raster: &fallback,
            compute: &[compute_program()],
        },
    )?;
    Ok(Prepared {
        common,
        mask_desc,
        fallback_programs,
    })
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

    /// Add culling, shadows and scene draws. The Graph owns the tile masks
    /// through their last consumer and no tile startup submission is needed.
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        lights: &[PointLight],
    ) -> Result<SceneImages, GraphError> {
        let config = self.common.config();
        let append_scene = |graph: &mut Graph, scene: RasterScene<'_>| {
            add_scene_passes(
                graph,
                config.extent,
                config.clear_color,
                config.samples,
                scene.has_transparency,
                |pass| scene.draw_opaque(pass),
                |pass| scene.draw_transparent(pass),
            )
        };
        if lights.is_empty() {
            return self.common.add_scene_using_programs(
                graph,
                frame,
                Some(&self.fallback_programs),
                |_, _| Ok([]),
                append_scene,
            );
        }
        let [width, height] = config.extent;
        let bytes = light_bytes(width, height, lights)?;
        let mask_desc = self.mask_desc;
        self.common.add_scene_using(
            graph,
            frame,
            |graph, camera| {
                let lights = upload_lights(graph, &bytes)?;
                let mask = graph.image("tile light masks", mask_desc)?;
                let mask = graph.image_view(mask, 0, 0)?;
                append_culling(graph, camera, lights, mask)?;
                Ok(tiled_raster_bindings(lights, mask))
            },
            append_scene,
        )
    }

    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: renderer::graph::ImageHandle,
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
        lights: &[PointLight],
    ) -> Result<FrameImages, GraphError> {
        let (output, exposure) = self.common.checked_output(frame.exposure)?;
        let scene = self.add_scene(graph, frame, lights)?;
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

fn upload_lights(
    graph: &mut Graph,
    bytes: &[u8; LIGHT_BUFFER_BYTES as usize],
) -> Result<BufferHandle, GraphError> {
    let lights = graph.buffer("local point lights", LIGHTS)?;
    graph.upload_buffer("local point light update", lights, 0, bytes)?;
    Ok(lights)
}
