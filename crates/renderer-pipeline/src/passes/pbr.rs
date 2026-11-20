//! Fixed shader programs for Forward PBR and directional shadow maps.

use crate::shader_binaries::packaged_shaders;

// Only the reviewed immutable asset declarations may use renderer's trusted
// shader import. All bindings and graph construction use safe APIs.
#[allow(unsafe_code)]
mod pbr_programs;

#[allow(unsafe_code)]
mod transform_programs;

pub(crate) use pbr_programs::forward_plus;
pub use pbr_programs::{
    CAMERA_BYTES, INSTANCE_INLINE_BYTES, MATERIAL_BYTES, VERTEX_BYTES, blend_program, deferred,
    instancing_program, opaque_program, shadow_program,
};
pub(crate) use transform_programs::{TransformTechnique, transformed_programs};
pub use transform_programs::{transformed_opaque_program, transformed_shadow_program};
