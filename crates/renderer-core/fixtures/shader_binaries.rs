// Include only the shader formats compiled into the consuming executable.
macro_rules! fixture_shaders {
    ($spirv:expr, $dxil:expr $(,)?) => {{
        #[cfg(all(feature = "vulkan", windows, feature = "d3d12"))]
        {
            renderer_core::program::ShaderBinaries::both($spirv, $dxil)
        }
        #[cfg(all(feature = "vulkan", not(all(windows, feature = "d3d12"))))]
        {
            renderer_core::program::ShaderBinaries::vulkan($spirv)
        }
        #[cfg(all(not(feature = "vulkan"), windows, feature = "d3d12"))]
        {
            renderer_core::program::ShaderBinaries::d3d12($dxil)
        }
        #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
        {
            renderer_core::program::ShaderBinaries::metadata_only()
        }
    }};
}
