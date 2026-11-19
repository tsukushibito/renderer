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
    ($spirv:expr, $dxil:expr $(,)?) => {{
        #[cfg(all(feature = "vulkan", windows, feature = "d3d12"))]
        {
            renderer::program::ShaderBinaries::both($spirv, $dxil)
        }
        #[cfg(all(feature = "vulkan", not(all(windows, feature = "d3d12"))))]
        {
            renderer::program::ShaderBinaries::vulkan($spirv)
        }
        #[cfg(all(not(feature = "vulkan"), windows, feature = "d3d12"))]
        {
            renderer::program::ShaderBinaries::d3d12($dxil)
        }
        #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
        {
            renderer::program::ShaderBinaries::metadata_only()
        }
    }};
}

pub(super) use packaged_shaders;
