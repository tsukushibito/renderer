//! Forward PBR with conservative per-tile, variable-length local-light masks.
pub use crate::passes::pbr::forward_plus::{
    LocalLight, LocalLightLayout, MAX_TILES, TILE_SIZE, active_tile_count, append_culling,
    compute_program, raster_bindings, raster_program,
};
use crate::{
    passes::pbr::TransformTechnique,
    pbr::{
        SceneLimits,
        scene::{DrawMode, PreparedScene, Programs},
    },
};
pub use crate::{pbr::Config, pbr_forward::Prepared};
use sashiko_renderer_core::graph::GraphError;

/// Prepare the standard forward targets, plus culling when local lights are enabled.
pub fn prepare(config: Config, limits: SceneLimits) -> Result<Prepared, GraphError> {
    Ok(Prepared {
        common: PreparedScene::new(
            config,
            limits,
            Programs::default(),
            true,
            DrawMode::Indirect,
            Some(TransformTechnique::ForwardPlus),
        )?,
    })
}
