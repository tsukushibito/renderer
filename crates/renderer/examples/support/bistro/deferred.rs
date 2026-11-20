//! Single-sample PBR passes shared with the library implementation.

use super::program::{
    CAMERA_BYTES,
    deferred::{DEFERRED_RECONSTRUCTION_BYTES, deferred_lighting_program, gbuffer_program},
};

#[path = "../../../../renderer-pipeline/src/pbr_deferred/passes.rs"]
mod passes;

pub(super) use passes::{Inputs, PLANES, append};
