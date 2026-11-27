//! The shared vertex stage for a generated fullscreen triangle.

use super::{D3D12InlineConstantBinding, D3D12RasterShaders, RasterShaderStages};
use crate::program::ShaderBinaries;

/// Pair Renderer's built-in fullscreen triangle vertex shader with fragment
/// binaries. The vertex shader uses vertex IDs 0–2, no inputs or bindings, and
/// entry point `main`. Only enabled backends with supplied fragments are kept.
///
/// The returned binaries still need a descriptor that matches their actual
/// behavior before they can be [imported as a Program](super::ManagedRasterProgram::from_trusted_descriptor).
pub const fn fullscreen_triangle_shaders(
    fragments: ShaderBinaries<&'static [u8]>,
    d3d12_inline_constants: Option<D3D12InlineConstantBinding>,
) -> ShaderBinaries<RasterShaderStages, D3D12RasterShaders> {
    ShaderBinaries {
        #[cfg(feature = "vulkan")]
        clip_distance_vertex: None,
        #[cfg(feature = "webgpu")]
        wgsl: match fragments.wgsl {
            Some(fragment) => Some(RasterShaderStages {
                vertex: include_bytes!("../../../shaders/composite.vert.wgsl"),
                fragment: Some(fragment),
            }),
            None => None,
        },
        #[cfg(feature = "webgpu")]
        nonfiltering: fragments.nonfiltering,
        vulkan: match fragments.vulkan {
            #[cfg(feature = "vulkan")]
            Some(fragment) => Some(RasterShaderStages {
                vertex: include_bytes!("../../../shaders/composite.vert.spv"),
                fragment: Some(fragment),
            }),
            _ => None,
        },
        d3d12: match (fragments.d3d12, d3d12_inline_constants) {
            #[cfg(all(feature = "d3d12", windows))]
            (Some(fragment), inline_constants) => Some(D3D12RasterShaders {
                stages: RasterShaderStages {
                    vertex: include_bytes!("../../../shaders/dxil/composite.vert.dxil"),
                    fragment: Some(fragment),
                },
                inline_constants,
            }),
            _ => None,
        },
    }
}
