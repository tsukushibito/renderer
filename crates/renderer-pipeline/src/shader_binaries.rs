//! Embed the shader formats selected for this application build.

macro_rules! packaged_shaders {
    (
        vulkan: ($vertex_spirv:expr, $fragment_spirv:expr $(,)?),
        d3d12: ($vertex_dxil:expr, $fragment_dxil:expr $(,)?),
        inline_constants: $inline_constants:expr $(,)?
    ) => {
        packaged_shaders!(
            renderer::program::RasterShaderStages {
                vertex: $vertex_spirv,
                fragment: $fragment_spirv,
            },
            renderer::program::D3D12RasterShaders {
                stages: renderer::program::RasterShaderStages {
                    vertex: $vertex_dxil,
                    fragment: $fragment_dxil,
                },
                inline_constants: $inline_constants,
            },
        )
    };
    ($spirv:expr, $dxil:expr $(,)?) => {
        renderer::program::embedded_shader_binaries!($spirv, $dxil)
    };
}

pub(crate) use packaged_shaders;
