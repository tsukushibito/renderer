//! Immutable graphics shader contracts retained by owned Raster commands.

use super::{BindingLayout, BindingType, ShaderStages};
use crate::ImageFormat;

/// Maximum distinct assets in one prepared Raster working set.
pub const MAX_RASTER_PROGRAMS: usize = 64;
/// Baseline descriptor-set/register-space limit shared by both backends.
pub const MAX_RASTER_BIND_GROUPS: usize = 4;
pub const MAX_RASTER_ATTRIBUTES: usize = 16;
pub const MAX_RASTER_BINDINGS: usize = 18;
pub const MAX_RASTER_COLOR_ATTACHMENTS: usize = 8;

/// Component layout for one attribute in the single per-vertex stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VertexFormat {
    Float32x2,
    Float32x3,
    Float32x4,
}
impl VertexFormat {
    pub const fn size(self) -> u32 {
        match self {
            Self::Float32x2 => 8,
            Self::Float32x3 => 12,
            Self::Float32x4 => 16,
        }
    }
}

/// `location` is both the SPIR-V location and DXIL `TEXCOORD` semantic index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VertexAttribute {
    pub location: u32,
    pub offset: u32,
    pub format: VertexFormat,
}

/// Inline words begin at byte offset zero in each selected shader stage.
/// D3D12 maps this block to the specified CBV register and register space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterParameterLayout {
    pub size: u32,
    pub stages: ShaderStages,
    pub dxil_register: u32,
    pub dxil_space: u32,
}

/// Fixed blending shared by Vulkan and D3D12, with all color channels writable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendState {
    Replace,
    /// Straight alpha: rgb = src.rgb * src.a + dst.rgb * (1 - src.a).
    /// Alpha = src.a + dst.a * (1 - src.a).
    Alpha,
    /// Premultiplied alpha: rgba = src.rgba + dst.rgba * (1 - src.a).
    PremultipliedAlpha,
    /// Add source and destination RGBA.
    Additive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareOp {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

/// The shader's depth contract. Optional testing is enabled exactly when an
/// attachment is supplied; Disabled accepts an attachment but does not test/write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthState {
    Disabled,
    Optional {
        compare: CompareOp,
        write: bool,
    },
    Required {
        compare: CompareOp,
        write: bool,
        require_load: bool,
    },
}
impl DepthState {
    pub const fn compare(self) -> Option<CompareOp> {
        match self {
            Self::Disabled => None,
            Self::Optional { compare, .. } | Self::Required { compare, .. } => Some(compare),
        }
    }
    pub const fn write_enabled(self) -> bool {
        match self {
            Self::Disabled => false,
            Self::Optional { write, .. } | Self::Required { write, .. } => write,
        }
    }
}

/// A possible image/sampler combination used by a trusted graphics shader.
/// `Load`/`texelFetch` accesses do not use a sampler and need no pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampledImageSamplerPair {
    pub image_group: u32,
    pub image_binding: u32,
    pub sampler_group: u32,
    pub sampler_binding: u32,
}

/// Immutable graphics assets and their complete ABI. Slices are static so
/// frame plans retain the contract without allocating or borrowing frame input.
pub struct RasterProgramDescriptor {
    pub entry: &'static str,
    /// Zero selects a generated fullscreen triangle and requires no attributes.
    pub vertex_stride: u32,
    pub vertex_attributes: &'static [VertexAttribute],
    pub parameters: RasterParameterLayout,
    pub color_outputs: &'static [ImageFormat],
    pub bindings: &'static [BindingLayout],
    /// Buffer-prefix footprints in binding order; zero for images/samplers.
    pub binding_bytes: &'static [u64],
    /// Complete possible sampling combinations in both shader stages.
    pub sampling_pairs: &'static [SampledImageSamplerPair],
    pub blend: BlendState,
    pub depth: DepthState,
    pub vertex_spirv: &'static [u8],
    pub fragment_spirv: &'static [u8],
    pub vertex_dxil: &'static [u8],
    pub fragment_dxil: &'static [u8],
}

/// An immutable trusted vertex/fragment shader pair, independent of a native
/// pipeline. Device limits and format capabilities are established at preparation.
#[derive(Clone, Copy)]
pub struct ManagedRasterProgram {
    manifest: &'static RasterProgramDescriptor,
}

/// A raster pipeline variant to prepare before frame processing.
#[derive(Clone, Copy)]
pub struct RasterPipelineRequest {
    pub program: ManagedRasterProgram,
    pub sample_count: crate::graph::SampleCount,
}

impl From<ManagedRasterProgram> for RasterPipelineRequest {
    fn from(program: ManagedRasterProgram) -> Self {
        Self {
            program,
            sample_count: crate::graph::SampleCount::X1,
        }
    }
}

const MESH_ATTRIBUTES: &[VertexAttribute] = &[
    VertexAttribute {
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x3,
    },
    VertexAttribute {
        location: 1,
        offset: 12,
        format: VertexFormat::Float32x4,
    },
];
static MESH: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 28,
    vertex_attributes: MESH_ATTRIBUTES,
    parameters: RasterParameterLayout {
        size: 80,
        stages: ShaderStages::VERTEX,
        dxil_register: 0,
        dxil_space: 0,
    },
    color_outputs: &[ImageFormat::Rgba8Unorm],
    bindings: &[],
    binding_bytes: &[],
    sampling_pairs: &[],
    blend: BlendState::Replace,
    depth: DepthState::Optional {
        compare: CompareOp::Less,
        write: true,
    },
    vertex_spirv: include_bytes!("../../shaders/mesh.vert.spv"),
    fragment_spirv: include_bytes!("../../shaders/mesh.frag.spv"),
    vertex_dxil: include_bytes!("../../shaders/dxil/mesh.vert.dxil"),
    fragment_dxil: include_bytes!("../../shaders/dxil/mesh.frag.dxil"),
};
static TEXTURED_MESH: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        dxil_register: 1,
        ..MESH.parameters
    },
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            min_size: 16,
            stages: ShaderStages::VERTEX,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 1,
            kind: BindingType::Sampler,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
    ],
    binding_bytes: &[16, 0, 0],
    sampling_pairs: &[SampledImageSamplerPair {
        image_group: 1,
        image_binding: 0,
        sampler_group: 1,
        sampler_binding: 1,
    }],
    vertex_spirv: include_bytes!("../../shaders/textured.vert.spv"),
    fragment_spirv: include_bytes!("../../shaders/textured.frag.spv"),
    vertex_dxil: include_bytes!("../../shaders/dxil/textured.vert.dxil"),
    fragment_dxil: include_bytes!("../../shaders/dxil/textured.frag.dxil"),
    ..MESH
};
static MESH_HDR: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[ImageFormat::Rgba16Float],
    ..MESH
};
static TEXTURED_MESH_HDR: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[ImageFormat::Rgba16Float],
    ..TEXTURED_MESH
};
static DEPTH_ONLY: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[],
    depth: DepthState::Required {
        compare: CompareOp::Less,
        write: true,
        require_load: false,
    },
    fragment_spirv: &[],
    fragment_dxil: &[],
    ..MESH
};
static MESH_PREPASSED: RasterProgramDescriptor = RasterProgramDescriptor {
    depth: DepthState::Required {
        compare: CompareOp::LessEqual,
        write: false,
        require_load: true,
    },
    ..MESH
};
static MESH_HDR_PREPASSED: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[ImageFormat::Rgba16Float],
    ..MESH_PREPASSED
};

impl std::fmt::Debug for ManagedRasterProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedRasterProgram")
            .field("vertex_stride", &self.vertex_stride())
            .field("parameters", &self.parameter_layout())
            .field("color_outputs", &self.color_outputs())
            .field("bindings", &self.bindings())
            .field("blend", &self.blend())
            .field("depth", &self.depth())
            .finish()
    }
}
impl ManagedRasterProgram {
    /// Import a statically owned graphics asset without Renderer effect variants.
    ///
    /// # Safety
    /// Both shader pairs must be valid Vulkan 1.3/SPIR-V 1.3 and D3D12 SM6.0
    /// vertex/fragment programs for `entry = "main"`, using only enabled backend
    /// features. Their stage interfaces, descriptor slots, vertex attributes,
    /// inline parameters and outputs must exactly match this complete descriptor.
    /// For indexed draws there is one per-vertex stream with a nonzero,
    /// four-byte-aligned stride and at most 16 unique locations below 16,
    /// with four-byte-aligned offsets and formats wholly within that stride.
    /// DXIL uses TEXCOORD semantic indices equal to attribute locations.
    /// Indices are u32 triangle lists with base vertex and first instance zero.
    /// Draws may select an index range and request any nonzero instance count.
    ///
    /// A zero stride and empty attributes select vertex generation via
    /// [`crate::graph::RasterPass::draw_fullscreen`]. The vertex shader consumes
    /// no vertex attributes, and must support vertex IDs 0 through 2 and
    /// instance ID 0.
    ///
    /// No undeclared vertex input, resources, push constants or side effects are
    /// permitted. Color outputs require a fragment shader. Declare their
    /// formats in contiguous shader output location order from zero, up to
    /// [`MAX_RASTER_COLOR_ATTACHMENTS`]. Each format is RGBA8_UNORM or RGBA16_FLOAT.
    /// Blend state applies uniformly to every color target. With no color outputs
    /// a fragment shader is optional and may discard fragments, but has no outputs.
    /// An absent stage has empty binaries for both backends and no stage inputs,
    /// bindings or inline parameters declared for it.
    /// Fragment depth, stencil, sample-mask writes, sample shading, storage writes,
    /// atomics, input attachments and attachment feedback are unsupported.
    ///
    /// Parameters have a four-byte multiple size at most 128 bytes, start at
    /// offset zero, and are used only in the nonempty declared Vertex/Fragment
    /// stages (zero size requires EMPTY stages). DXIL represents this block as
    /// the specified CBV register/space, with no overlapping Uniform binding.
    /// Inline members must not be dynamically indexed: D3D12 root constants
    /// permit only static/literal indexing. Use Uniform/StorageRead for arrays
    /// that require dynamic indexing.
    /// There are at most 18 bindings, with group indices below 4, unique
    /// group/binding coordinates and nonempty graphics
    /// stages only. Supported kinds are Uniform, StorageRead, SampledImage and
    /// Sampler; every slot has exactly one descriptor (no descriptor arrays).
    /// Read-only buffers are raw/structured root-descriptor compatible
    /// in DXIL. Groups equal register spaces and bindings equal register numbers.
    /// `binding_bytes` has one entry per binding, a nonzero buffer prefix no
    /// larger than min_size, and zero for image/sampler slots (min_size zero).
    /// Images are non-array, non-MS float 2D resources. Sampling may read any
    /// mip of the selected View; its complete range is declared as a read. No
    /// optional narrow arithmetic types or unsupported capabilities are required.
    /// `sampling_pairs` lists every possible image/sampler combination in both
    /// stages. Each referenced slot exists with the corresponding SampledImage
    /// or Sampler kind. No combination may be omitted, including dynamically
    /// selected combinations. Load/texelFetch-only accesses need no pair.
    ///
    /// For every accepted draw, including every permitted instance index, all
    /// permitted inline words and resource contents,
    /// shaders may access only the declared buffer prefixes and selected image
    /// Views. They must not perform out-of-bounds accesses, races or unsupported
    /// operations. No inter-draw shader synchronization or storage side effects
    /// may be required. Shader arithmetic must safely handle all accepted inputs;
    /// reflection/hash validation does not establish these behavioral guarantees.
    /// Replacing source, compiler options, binaries or metadata requires reviewing
    /// the guarantees again and importing a new immutable descriptor.
    pub const unsafe fn from_trusted_descriptor(
        descriptor: &'static RasterProgramDescriptor,
    ) -> Self {
        Self {
            manifest: descriptor,
        }
    }

    pub const fn mesh() -> Self {
        Self { manifest: &MESH }
    }
    pub const fn mesh_hdr() -> Self {
        Self {
            manifest: &MESH_HDR,
        }
    }
    /// Read-only LESS_EQUAL depth after the same geometry's depth prepass.
    pub const fn mesh_prepassed() -> Self {
        Self {
            manifest: &MESH_PREPASSED,
        }
    }
    pub const fn mesh_hdr_prepassed() -> Self {
        Self {
            manifest: &MESH_HDR_PREPASSED,
        }
    }
    pub const fn depth_only() -> Self {
        Self {
            manifest: &DEPTH_ONLY,
        }
    }
    /// Mesh with a vertex Uniform offset and fragment image/sampler bindings.
    pub const fn textured_mesh() -> Self {
        Self {
            manifest: &TEXTURED_MESH,
        }
    }
    pub const fn textured_mesh_hdr() -> Self {
        Self {
            manifest: &TEXTURED_MESH_HDR,
        }
    }
    pub const fn descriptor(&self) -> &'static RasterProgramDescriptor {
        self.manifest
    }
    /// Identity of this immutable asset, including all fixed pipeline states.
    pub fn identity(&self) -> usize {
        std::ptr::from_ref(self.manifest) as usize
    }
    pub fn same(&self, other: &Self) -> bool {
        std::ptr::eq(self.manifest, other.manifest)
    }
    pub const fn vertex_stride(&self) -> u32 {
        self.manifest.vertex_stride
    }
    pub(crate) const fn is_fullscreen(&self) -> bool {
        self.vertex_stride() == 0
    }
    pub const fn vertex_attributes(&self) -> &'static [VertexAttribute] {
        self.manifest.vertex_attributes
    }
    pub const fn parameter_size(&self) -> u32 {
        self.manifest.parameters.size
    }
    pub const fn parameter_layout(&self) -> RasterParameterLayout {
        self.manifest.parameters
    }
    pub const fn color_outputs(&self) -> &'static [ImageFormat] {
        self.manifest.color_outputs
    }
    pub const fn bindings(&self) -> &'static [BindingLayout] {
        self.manifest.bindings
    }
    pub const fn binding_bytes(&self) -> &'static [u64] {
        self.manifest.binding_bytes
    }
    pub const fn sampling_pairs(&self) -> &'static [SampledImageSamplerPair] {
        self.manifest.sampling_pairs
    }
    pub const fn blend(&self) -> BlendState {
        self.manifest.blend
    }
    pub const fn depth(&self) -> DepthState {
        self.manifest.depth
    }
    pub const fn requires_depth(&self) -> bool {
        matches!(self.depth(), DepthState::Required { .. })
    }
    pub const fn requires_depth_load(&self) -> bool {
        matches!(
            self.depth(),
            DepthState::Required {
                require_load: true,
                ..
            }
        )
    }
    pub fn is_depth_only(&self) -> bool {
        self.color_outputs().is_empty()
    }
    #[cfg(test)]
    fn is_mesh(&self) -> bool {
        self.same(&Self::mesh())
    }
    #[cfg(test)]
    fn is_textured(&self) -> bool {
        self.bindings()
            .iter()
            .any(|slot| slot.kind == BindingType::SampledImage)
    }
    #[cfg(test)]
    fn is_prepassed(&self) -> bool {
        self.requires_depth_load()
    }
    #[cfg(feature = "vulkan")]
    pub(crate) fn vertex_spirv(&self) -> &'static [u8] {
        self.manifest.vertex_spirv
    }
    #[cfg(feature = "vulkan")]
    pub(crate) fn fragment_spirv(&self) -> &'static [u8] {
        self.manifest.fragment_spirv
    }
    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn vertex_dxil(&self) -> &'static [u8] {
        self.manifest.vertex_dxil
    }
    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn fragment_dxil(&self) -> &'static [u8] {
        self.manifest.fragment_dxil
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepassed_programs_keep_shader_abis_but_have_distinct_depth_contracts() {
        let all = [
            ManagedRasterProgram::mesh(),
            ManagedRasterProgram::textured_mesh(),
            ManagedRasterProgram::mesh_hdr(),
            ManagedRasterProgram::textured_mesh_hdr(),
            ManagedRasterProgram::depth_only(),
            ManagedRasterProgram::mesh_prepassed(),
            ManagedRasterProgram::mesh_hdr_prepassed(),
        ];
        let mut ids = all.map(|program| program.identity());
        ids.sort_unstable();
        assert!(
            ids.windows(2).all(|pair| pair[0] != pair[1]),
            "native pipeline identities must not alias"
        );
        for (program, original) in [
            (
                ManagedRasterProgram::mesh_prepassed(),
                ManagedRasterProgram::mesh(),
            ),
            (
                ManagedRasterProgram::mesh_hdr_prepassed(),
                ManagedRasterProgram::mesh_hdr(),
            ),
        ] {
            assert!(program.is_prepassed());
            assert!(program.requires_depth());
            assert!(!original.is_prepassed());
            assert!(!original.requires_depth());
            assert_eq!(program.color_outputs(), original.color_outputs());
            assert_eq!(program.vertex_stride(), original.vertex_stride());
            assert_eq!(program.parameter_size(), original.parameter_size());
            assert_eq!(program.bindings(), original.bindings());
            assert_eq!(program.binding_bytes(), original.binding_bytes());
            #[cfg(feature = "vulkan")]
            {
                assert_eq!(program.vertex_spirv(), original.vertex_spirv());
                assert_eq!(program.fragment_spirv(), original.fragment_spirv());
            }
            #[cfg(all(feature = "d3d12", windows))]
            {
                assert_eq!(program.vertex_dxil(), original.vertex_dxil());
                assert_eq!(program.fragment_dxil(), original.fragment_dxil());
            }
        }
    }

    #[test]
    fn hdr_and_depth_contracts_preserve_the_reviewed_shader_abis() {
        for (program, original) in [
            (
                ManagedRasterProgram::mesh_hdr(),
                ManagedRasterProgram::mesh(),
            ),
            (
                ManagedRasterProgram::textured_mesh_hdr(),
                ManagedRasterProgram::textured_mesh(),
            ),
        ] {
            assert_eq!(program.color_outputs(), &[ImageFormat::Rgba16Float]);
            assert_eq!(program.vertex_stride(), original.vertex_stride());
            assert_eq!(program.parameter_size(), original.parameter_size());
            assert_eq!(program.bindings(), original.bindings());
            assert_eq!(program.binding_bytes(), original.binding_bytes());
            assert_eq!(program.is_textured(), original.is_textured());
            assert!(!program.is_mesh());
            assert!(!program.is_depth_only());
            #[cfg(feature = "vulkan")]
            {
                assert_eq!(program.vertex_spirv(), original.vertex_spirv());
                assert_eq!(program.fragment_spirv(), original.fragment_spirv());
            }
            #[cfg(all(feature = "d3d12", windows))]
            {
                assert_eq!(program.vertex_dxil(), original.vertex_dxil());
                assert_eq!(program.fragment_dxil(), original.fragment_dxil());
            }
        }
        let program = ManagedRasterProgram::depth_only();
        let mesh = ManagedRasterProgram::mesh();
        assert!(program.color_outputs().is_empty());
        assert!(program.bindings().is_empty());
        assert!(program.binding_bytes().is_empty());
        assert!(program.is_depth_only());
        assert!(!program.is_textured());
        assert!(!program.is_mesh());
        assert_eq!(program.vertex_stride(), mesh.vertex_stride());
        assert_eq!(program.parameter_size(), mesh.parameter_size());
        #[cfg(feature = "vulkan")]
        {
            assert_eq!(program.vertex_spirv(), mesh.vertex_spirv());
            assert!(program.fragment_spirv().is_empty());
        }
        #[cfg(all(feature = "d3d12", windows))]
        {
            assert_eq!(program.vertex_dxil(), mesh.vertex_dxil());
            assert!(program.fragment_dxil().is_empty());
        }
    }

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
        assert!(program.bindings().is_empty());
        assert!(program.binding_bytes().is_empty());
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

    #[test]
    fn textured_contract_derives_each_slot_and_stage_from_the_real_modules() {
        let program = ManagedRasterProgram::textured_mesh();
        assert!(!program.is_mesh());
        assert_eq!(
            program.vertex_stride(),
            ManagedRasterProgram::mesh().vertex_stride()
        );
        assert_eq!(
            program.parameter_size(),
            ManagedRasterProgram::mesh().parameter_size()
        );
        assert_eq!(program.color_outputs(), &[ImageFormat::Rgba8Unorm]);
        let vertex: serde_json::Value = serde_json::from_str(include_str!(
            "../../shaders/dxil/textured.vert.spirv-reflection.json"
        ))
        .unwrap();
        let fragment: serde_json::Value = serde_json::from_str(include_str!(
            "../../shaders/dxil/textured.frag.spirv-reflection.json"
        ))
        .unwrap();
        let mut reflected = Vec::new();
        let mut bytes = Vec::new();
        for (module, mode, stages) in [
            (&vertex, "vert", ShaderStages::VERTEX),
            (&fragment, "frag", ShaderStages::FRAGMENT),
        ] {
            assert_eq!(module["entryPoints"][0]["name"], "main");
            assert_eq!(module["entryPoints"][0]["mode"], mode);
            for (category, kind) in [
                ("ubos", BindingType::Uniform),
                ("separate_images", BindingType::SampledImage),
                ("separate_samplers", BindingType::Sampler),
            ] {
                for resource in module[category].as_array().into_iter().flatten() {
                    let min_size = resource["block_size"].as_u64().unwrap_or(0);
                    reflected.push(BindingLayout {
                        group: resource["set"].as_u64().unwrap() as u32,
                        binding: resource["binding"].as_u64().unwrap() as u32,
                        kind,
                        min_size,
                        stages,
                    });
                    bytes.push(min_size);
                }
            }
            assert!(module["ssbos"].as_array().is_none_or(Vec::is_empty));
            assert!(module["images"].as_array().is_none_or(Vec::is_empty));
        }
        assert_eq!(program.bindings(), reflected);
        assert_eq!(program.binding_bytes(), bytes);
        assert_eq!(program.binding_bytes(), &[16, 0, 0]);
        let offset = &vertex["types"][vertex["ubos"][0]["type"].as_str().unwrap()]["members"];
        assert_eq!(offset.as_array().unwrap().len(), 1);
        assert_eq!(offset[0]["type"], "vec4");
        assert_eq!(offset[0]["offset"], 0);
        let push =
            &vertex["types"][vertex["push_constants"][0]["type"].as_str().unwrap()]["members"];
        assert_eq!(push[0]["type"], "mat4");
        assert_eq!(push[0]["offset"], 0);
        assert_eq!(push[0]["matrix_stride"], 16);
        assert_ne!(push[0]["row_major"].as_bool(), Some(true));
        assert_eq!(push[1]["type"], "vec4");
        assert_eq!(push[1]["offset"], 64);
        assert_eq!(fragment["separate_images"][0]["type"], "texture2D");
        assert_eq!(fragment["separate_samplers"][0]["type"], "sampler");
        assert_eq!(fragment["outputs"].as_array().unwrap().len(), 1);
        assert_eq!(fragment["outputs"][0]["type"], "vec4");
        assert_eq!(fragment["outputs"][0]["location"], 0);

        // Review the behavior separately from reflection: no storage writes,
        // no input-dependent indexing into the Uniform, and explicit mip zero.
        let source = include_str!("../../shaders/textured.vert");
        assert!(source.contains("+ vertex_offset.clip_offset;"));
        let source = include_str!("../../shaders/textured.frag");
        assert!(
            source.contains("textureLod(sampler2D(source_image, source_sampler), sample_uv, 0.0)")
        );
        let dxil = include_str!("../../shaders/dxil/textured.frag.hlsl");
        assert!(dxil.contains("SampleLevel("));
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

    #[cfg(feature = "vulkan")]
    #[test]
    fn textured_native_assets_are_distinct_from_the_mesh_pipeline() {
        let program = ManagedRasterProgram::textured_mesh();
        assert_eq!(
            program.vertex_spirv(),
            include_bytes!("../../shaders/textured.vert.spv")
        );
        assert_eq!(
            program.fragment_spirv(),
            include_bytes!("../../shaders/textured.frag.spv")
        );
        assert_ne!(
            program.vertex_spirv(),
            ManagedRasterProgram::mesh().vertex_spirv()
        );
        assert_ne!(
            program.fragment_spirv(),
            ManagedRasterProgram::mesh().fragment_spirv()
        );
    }
}
