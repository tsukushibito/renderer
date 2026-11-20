//! Fixed shader programs for Forward PBR and directional shadow maps.

use crate::shader_binaries::packaged_shaders;

// Only the reviewed immutable asset declarations may use renderer's trusted
// shader import. All bindings and graph construction use safe APIs.
#[allow(unsafe_code)]
#[path = "../../shaders/pbr_programs.rs"]
mod pbr_programs;

pub use pbr_programs::{
    CAMERA_BYTES, MATERIAL_BYTES, VERTEX_BYTES, blend_program, opaque_program, shadow_program,
};
pub(crate) use pbr_programs::{deferred, forward_plus};
