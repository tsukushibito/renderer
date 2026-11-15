//! Reviewed graphics shader contracts retained by owned Raster commands.

use super::ImageFormat;

/// An immutable, reviewed vertex/fragment shader pair, independent of a native
/// pipeline. This CPU contract does not establish Device limits or PSO creation.
/// Arbitrary shader source, binaries and self-declared manifests are not accepted.
#[derive(Clone, Copy)]
pub struct ManagedRasterProgram {
    manifest: &'static Manifest,
}

struct Manifest {
    vertex_stride: u32,
    parameter_size: u32,
    color_outputs: &'static [ImageFormat],
    #[cfg(feature = "vulkan")]
    vertex_spirv: &'static [u8],
    #[cfg(feature = "vulkan")]
    fragment_spirv: &'static [u8],
}

static MESH: Manifest = Manifest {
    vertex_stride: 28,
    parameter_size: 80,
    color_outputs: &[ImageFormat::Rgba8Unorm],
    #[cfg(feature = "vulkan")]
    vertex_spirv: include_bytes!("../../shaders/mesh.vert.spv"),
    #[cfg(feature = "vulkan")]
    fragment_spirv: include_bytes!("../../shaders/mesh.frag.spv"),
};

impl std::fmt::Debug for ManagedRasterProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedRasterProgram")
            .field("vertex_stride", &self.vertex_stride())
            .field("parameter_size", &self.parameter_size())
            .field("color_outputs", &self.color_outputs())
            .finish()
    }
}

impl ManagedRasterProgram {
    /// The existing mesh shaders: packed `xyz` and linear `rgba` f32 vertex
    /// attributes; one vertex-stage inline column-major matrix and tint block;
    /// one RGBA8 color output. There are no logical resource descriptor bindings
    /// or storage side effects. The generated DXIL represents the same inline
    /// parameters as its `b0` constant block; it is not a new Graph buffer binding.
    ///
    /// The current command contract uses u32 triangle indices from first index
    /// zero, base vertex zero and one instance. It admits one color output with
    /// optional depth in CPU plans; native support remains backend-specific.
    pub const fn mesh() -> Self {
        Self { manifest: &MESH }
    }

    pub const fn vertex_stride(&self) -> u32 {
        self.manifest.vertex_stride
    }

    /// Size in bytes of the vertex-stage inline matrix and tint parameters.
    pub const fn parameter_size(&self) -> u32 {
        self.manifest.parameter_size
    }

    pub const fn color_outputs(&self) -> &'static [ImageFormat] {
        self.manifest.color_outputs
    }

    pub(crate) fn is_mesh(&self) -> bool {
        std::ptr::eq(self.manifest, &MESH)
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn vertex_spirv(&self) -> &'static [u8] {
        self.manifest.vertex_spirv
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn fragment_spirv(&self) -> &'static [u8] {
        self.manifest.fragment_spirv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature<'a>(reflection: &'a str, heading: &str) -> &'a str {
        reflection
            .split_once(heading)
            .unwrap()
            .1
            .split_once(";\n;\n;")
            .unwrap()
            .0
    }

    #[test]
    fn mesh_contract_matches_existing_shader_sources_and_compiler_reflection() {
        let program = ManagedRasterProgram::mesh();
        assert!(program.is_mesh());
        let contracts: serde_json::Value =
            serde_json::from_str(include_str!("../../shaders/dxil-contracts.json")).unwrap();
        let mesh = &contracts["shaders"]["mesh.vert"];
        assert_eq!(
            mesh["vertex_stride"].as_u64(),
            Some(u64::from(program.vertex_stride()))
        );
        assert_eq!(
            mesh["constant_bytes"].as_u64(),
            Some(u64::from(program.parameter_size()))
        );

        // Tie the Rust metadata to actual generated signatures and offsets,
        // rather than treating the documented contract as independent proof.
        let vertex =
            include_str!("../../shaders/dxil/mesh.vert.reflection.txt").replace("\r\n", "\n");
        let fragment =
            include_str!("../../shaders/dxil/mesh.frag.reflection.txt").replace("\r\n", "\n");
        let input = signature(&vertex, "; Input signature:");
        let attributes: Vec<_> = input
            .lines()
            .filter_map(|line| {
                let columns: Vec<_> = line.trim_start_matches(';').split_whitespace().collect();
                (columns.first() == Some(&"TEXCOORD")).then_some(columns)
            })
            .collect();
        assert_eq!(attributes.len(), 2);
        assert_eq!(&attributes[0][..3], ["TEXCOORD", "0", "xyz"]);
        assert_eq!(&attributes[1][..3], ["TEXCOORD", "1", "xyzw"]);
        assert_eq!(attributes[0][5], "float");
        assert_eq!(attributes[1][5], "float");
        assert_eq!(program.vertex_stride(), (3 + 4) * 4);
        assert!(
            vertex
                .lines()
                .any(|line| line.contains("row_major float4x4") && line.contains("Offset:    0"))
        );
        assert!(
            vertex
                .lines()
                .any(|line| line.contains("float4 _19_m1") && line.contains("Offset:   64"))
        );
        let size_line = vertex
            .lines()
            .find(|line| line.contains("} _17_19;") && line.contains("Size:"))
            .unwrap();
        let size = size_line
            .split_once("Size:")
            .unwrap()
            .1
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!(program.parameter_size(), size);

        let output = signature(&fragment, "; Output signature:");
        assert_eq!(
            output
                .lines()
                .filter(|line| line.contains("SV_Target"))
                .count(),
            program.color_outputs().len()
        );
        assert!(output.lines().any(|line| {
            line.trim_start_matches(';')
                .split_whitespace()
                .collect::<Vec<_>>()
                .starts_with(&["SV_Target", "0", "xyzw", "0", "TARGET", "float"])
        }));
        assert_eq!(program.color_outputs(), &[ImageFormat::Rgba8Unorm]);
        assert!(
            signature(&fragment, "; Resource Bindings:")
                .lines()
                .all(|line| !line.contains("cbuffer")
                    && !line.contains("texture")
                    && !line.contains("UAV"))
        );

        let source = include_str!("../../shaders/mesh.vert");
        assert!(source.contains("layout(location = 0) in vec3 position;"));
        assert!(source.contains("layout(location = 1) in vec4 color;"));
        assert!(source.contains("layout(push_constant) uniform DrawParameters"));
        assert!(source.contains("mat4 clip_from_model;"));
        assert!(source.contains("vec4 tint;"));
        let fragment_source = include_str!("../../shaders/mesh.frag");
        assert!(fragment_source.contains("layout(location = 0) out vec4 output_color;"));
    }

    #[cfg(feature = "vulkan")]
    #[test]
    fn mesh_native_assets_are_the_reviewed_vertex_and_fragment_pair() {
        let program = ManagedRasterProgram::mesh();
        assert_eq!(
            program.vertex_spirv(),
            include_bytes!("../../shaders/mesh.vert.spv")
        );
        assert_eq!(
            program.fragment_spirv(),
            include_bytes!("../../shaders/mesh.frag.spv")
        );
    }
}
