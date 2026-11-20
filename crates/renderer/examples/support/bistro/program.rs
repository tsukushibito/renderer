//! Bistro-specific Program variants and the shared Forward PBR ABI.

pub use pbr_programs::{deferred, forward_plus};
#[path = "../../../../renderer-pipeline/shaders/pbr_programs.rs"]
mod pbr_programs;

#[path = "program/visibility.rs"]
pub mod visibility;

use crate::shader_binaries::packaged_shaders;

use renderer::program::{
    ManagedRasterProgram, RasterParameterLayout, RasterProgramDescriptor, ShaderStages,
};

use pbr_programs::OPAQUE;
#[cfg(all(windows, feature = "d3d12"))]
use pbr_programs::PBR_FRAGMENT_DXIL;
#[cfg(feature = "vulkan")]
use pbr_programs::PBR_FRAGMENT_SPIRV;
pub use pbr_programs::{
    CAMERA_BYTES, MATERIAL_BYTES, VERTEX_BYTES, blend_program, opaque_program, shadow_program,
};

pub(super) const INSTANCE_INLINE_BYTES: u32 = 68;

static INSTANCING: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: INSTANCE_INLINE_BYTES,
        stages: ShaderStages::VERTEX,
    },
    shaders: packaged_shaders!(
        vulkan: (
            include_bytes!("../../../../renderer-pipeline/shaders/pbr_instancing.vert.spv"),
            Some(PBR_FRAGMENT_SPIRV),
        ),
        d3d12: (
            include_bytes!("../../../../renderer-pipeline/shaders/dxil/pbr_instancing.vert.dxil"),
            Some(PBR_FRAGMENT_DXIL),
        ),
        inline_constants: Some(renderer::program::D3D12InlineConstantBinding { register: 7, space: 0 }),
    ),
    ..OPAQUE
};
pub const fn instancing_program() -> ManagedRasterProgram {
    // SAFETY: This variant preserves the vertex inputs, fragment interface and
    // bindings of OPAQUE. It only reads the declared 68 inline bytes, then
    // uses a bounded instance index in arithmetic to transform the positions.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&INSTANCING) }
}
