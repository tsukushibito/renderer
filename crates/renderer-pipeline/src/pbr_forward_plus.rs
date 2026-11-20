//! Forward PBR with per-tile lists for world-space point lights.

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::runtime::{ExecutionError, FrameSubmission, Runtime};
use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{
        BufferDesc, BufferHandle, Graph, GraphCapacity, GraphError, ImportBuffer, ResourceKey,
    },
};

use crate::{
    passes::{
        output::OutputImages,
        pbr::{forward_plus::tiled_raster_bindings, shadow_program},
    },
    pbr::{
        Bounds, Camera, Environment, Frame, FrameImages, PreparedView, SceneImages, SceneLimits,
        Sun,
    },
    pbr_forward::{self, Programs, add_scene_passes, prepare_recipe},
};

pub use crate::passes::pbr::forward_plus::{
    LIGHT_BUFFER_BYTES, MAX_LIGHTS, MAX_TILES, PointLight, TILE_BUFFER_BYTES, TILE_SIZE,
    active_tile_count, append_culling, compute_program, light_bytes, raster_bindings,
    raster_program,
};
pub use crate::pbr_forward::Config;

const LIGHTS: BufferDesc = BufferDesc {
    size: LIGHT_BUFFER_BYTES,
};
const TILES: BufferDesc = BufferDesc {
    size: TILE_BUFFER_BYTES,
};
const INITIAL_EXTENT: u32 = 4096;

/// Graph slot capacity for one tile initialization and an imported camera.
/// Add the requirements for any other declarations in the same slot.
pub const TILE_INITIALIZATION_CAPACITY: GraphCapacity = GraphCapacity {
    resources: 3,
    passes: 2,
    accesses: 4,
    draws: 0,
    raster_constant_words: Some(0),
    bindings: 3,
    attachments: 0,
    dependencies: 0,
    initial_ranges: 1,
    name_bytes: 192,
    upload_bytes: LIGHT_BUFFER_BYTES as usize,
    edges: 8,
    segments: 8,
    readers: 8,
};

/// The export declaration for one fully initialized tile buffer.
#[must_use = "accept the submitted initialization to recover its tile buffer"]
pub struct TileInit {
    buffer: BufferHandle,
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    accepted: bool,
}

impl TileInit {
    pub fn buffer(&self) -> BufferHandle {
        self.buffer
    }

    /// Recover the registration from the matching accepted submission.
    /// A mismatched receipt leaves initialization available for a retry.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept(&mut self, submission: &FrameSubmission) -> Result<Tiles, TileInitError> {
        if self.accepted {
            return Err(TileInitError::AlreadyAccepted);
        }
        let buffer = submission
            .export_buffer(self.buffer)
            .map_err(TileInitError::Execution)?;
        self.accepted = true;
        Ok(Tiles { key: buffer.key() })
    }
}

#[derive(Debug)]
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub enum TileInitError {
    AlreadyAccepted,
    Execution(ExecutionError),
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl std::fmt::Display for TileInitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyAccepted => {
                formatter.write_str("Forward+ tile initialization was already accepted")
            }
            Self::Execution(error) => error.fmt(formatter),
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl std::error::Error for TileInitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyAccepted => None,
            Self::Execution(error) => Some(error),
        }
    }
}

/// A persistent tile list with all entries initialized in a startup Graph.
/// The Runtime owns in-flight retention after the registration is deleted.
#[must_use = "delete the tile registration when the view is no longer needed"]
#[derive(Debug)]
pub struct Tiles {
    key: ResourceKey,
}

impl Tiles {
    pub fn key(&self) -> ResourceKey {
        self.key
    }

    /// Acquire the latest accepted contents, including pending GPU work.
    /// Refresh before building each frame that writes the list.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn snapshot(&self, runtime: &Runtime) -> Result<ImportBuffer, ExecutionError> {
        runtime.refresh_buffer(self.key)
    }

    /// Release the registration. Existing submissions retain the buffer.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn delete(&self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        runtime.delete(self.key)
    }
}

/// Declare a one-time initialization of the full tile range.
///
/// `camera` supplies an initialized 64-byte view-projection prefix. The matrix
/// may be zero at startup because the initialization dispatch has no lights.
/// Submit the Graph, then recover the registration with `TileInit::accept`.
pub fn append_tile_initialization(
    graph: &mut Graph,
    camera: BufferHandle,
) -> Result<TileInit, GraphError> {
    let lights = upload_lights(graph, &light_bytes(INITIAL_EXTENT, INITIAL_EXTENT, &[])?)?;
    let buffer = graph.buffer("persistent tile light lists", TILES)?;
    append_culling(
        graph,
        INITIAL_EXTENT,
        INITIAL_EXTENT,
        camera,
        lights,
        buffer,
    )?;
    graph.export_buffer(buffer)?;
    Ok(TileInit {
        buffer,
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        accepted: false,
    })
}

/// Prepared Forward+ programs, frame capacities and PBR scratch.
pub struct Prepared {
    common: pbr_forward::Prepared,
    config: Config,
}

/// Prepare a 1× or 4× Forward+ recipe and reserve one persistent tile list.
/// Retained registrations and startup Graph scheduling belong to the caller.
/// Use at least [`TILE_BUFFER_BYTES`] for D3D12's maximum buffer size.
pub fn prepare(config: Config, limits: SceneLimits) -> Result<Prepared, GraphError> {
    let [width, height] = config.extent;
    active_tile_count(width, height)?;
    let programs = Programs {
        opaque: raster_program(true, false),
        blend: raster_program(true, true),
        shadow: shadow_program(),
    };
    let mut common = prepare_recipe(config, limits, programs, true)?;
    let preparation = common.preparation_mut();
    preparation.add_requirements(
        GraphRequirements {
            resources: 2,
            passes: 2,
            compute_dispatches: 1,
            accesses: 4,
            bindings: 3,
            name_bytes: "local point lights".len()
                + "local point light update".len()
                + "persistent tile light lists".len()
                + "Forward+ tile light lists".len(),
            upload_bytes: LIGHT_BUFFER_BYTES as usize,
            ..GraphRequirements::default()
        },
        WorkingSet {
            resources: &[
                ResourceClass::Buffer {
                    desc: LIGHTS,
                    count: preparation.frames_in_flight().get(),
                },
                ResourceClass::Buffer {
                    desc: TILES,
                    count: 1,
                },
            ],
            compute: &[compute_program()],
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
        camera: Camera,
        sun: Sun,
        bounds: Bounds,
        environment: Environment,
    ) -> Result<PreparedView, GraphError> {
        self.common.prepare_view(camera, sun, bounds, environment)
    }

    /// Add culling, shadows and scene draws. Use the latest scheduled tile
    /// contents from `Tiles::snapshot`. The culling pass overwrites the active
    /// prefix and the Runtime publishes the updated contents when it submits.
    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        lights: &[PointLight],
        tiles: &ImportBuffer,
    ) -> Result<SceneImages, GraphError> {
        if tiles.desc() != TILES {
            return Err(GraphError::invalid_input(
                "Forward+ tile buffer has the wrong size",
            ));
        }
        let config = self.config;
        let [width, height] = config.extent;
        let bytes = light_bytes(width, height, lights)?;
        self.common.add_scene_using(
            graph,
            frame,
            |graph, camera| {
                let lights = upload_lights(graph, &bytes)?;
                let tiles = graph.import_buffer("persistent tile light lists", tiles)?;
                append_culling(graph, width, height, camera, lights, tiles)?;
                Ok(tiled_raster_bindings(lights, tiles))
            },
            |graph, scene| {
                add_scene_passes(
                    graph,
                    config.extent,
                    config.clear_color,
                    config.samples,
                    scene.has_transparency,
                    |pass| scene.draw_opaque(pass),
                    |pass| scene.draw_transparent(pass),
                )
            },
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

    pub fn add_passes(
        &mut self,
        graph: &mut Graph,
        frame: Frame<'_>,
        lights: &[PointLight],
        tiles: &ImportBuffer,
    ) -> Result<FrameImages, GraphError> {
        let exposure = frame.exposure;
        let scene = self.add_scene(graph, frame, lights, tiles)?;
        let output = self.add_output(graph, scene.hdr, exposure)?;
        Ok(FrameImages {
            hdr: scene.hdr,
            depth: scene.depth,
            sdr: output.sdr,
            shadows: scene.shadows,
            camera: scene.camera,
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
