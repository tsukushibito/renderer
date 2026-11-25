//! Embed the shader formats selected for this application build.

macro_rules! packaged_shaders {
    (raster: ($vertex:expr, $fragment:expr), inline_constants: $inline:expr, nonfiltering: $slots:expr $(,)?) => {
        sashiko_renderer_core::program::with_wgsl!(
            sashiko_renderer_core::program::embedded_shader_binaries!(
                sashiko_renderer_core::program::RasterShaderStages {
                    vertex: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $vertex, ".spv")),
                    fragment: Some(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $fragment, ".spv"))),
                },
                sashiko_renderer_core::program::D3D12RasterShaders {
                    stages: sashiko_renderer_core::program::RasterShaderStages {
                        vertex: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/dxil/", $vertex, ".dxil")),
                        fragment: Some(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/dxil/", $fragment, ".dxil"))),
                    },
                    inline_constants: $inline,
                }
            ),
            sashiko_renderer_core::program::RasterShaderStages {
                vertex: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $vertex, ".wgsl")),
                fragment: Some(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $fragment, ".wgsl"))),
            },
            $slots
        )
    };
    (module: $module:expr, folder: $folder:literal, nonfiltering: $slots:expr $(,)?) => {
        sashiko_renderer_core::program::with_wgsl!(
            sashiko_renderer_core::program::embedded_shader_binaries!(
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $folder, $module, ".spv")),
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $folder, "dxil/", $module, ".dxil"))
            ),
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/", $folder, $module, ".wgsl")),
            $slots
        )
    };
    (module: $module:expr $(,)?) => {
        packaged_shaders!(module: $module, folder: "", nonfiltering: &[])
    };
}
pub(crate) use packaged_shaders;
