//! Embed the shader formats selected for this application build.

macro_rules! packaged_shaders {
    (
        vulkan: ($vertex_spirv:expr, $fragment_spirv:expr $(,)?),
        d3d12: ($vertex_dxil:expr, $fragment_dxil:expr $(,)?),
        inline_constants: $inline_constants:expr $(,)?
    ) => {
        packaged_shaders!(
            sashiko_renderer_core::program::RasterShaderStages {
                vertex: $vertex_spirv,
                fragment: $fragment_spirv,
            },
            sashiko_renderer_core::program::D3D12RasterShaders {
                stages: sashiko_renderer_core::program::RasterShaderStages {
                    vertex: $vertex_dxil,
                    fragment: $fragment_dxil,
                },
                inline_constants: $inline_constants,
            },
        )
    };
    ($spirv:expr, $dxil:expr $(,)?) => {
        sashiko_renderer_core::program::embedded_shader_binaries!($spirv, $dxil)
    };
}

pub(crate) use packaged_shaders;
