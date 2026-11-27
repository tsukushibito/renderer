//! Immutable graphics shader contracts retained by owned Raster commands.

use super::{
    BindingLayout, BindingType, ProgramAsset, ProgramId, ShaderBinaries, ShaderImportError,
    ShaderStages, binary, embedded_raster_shaders, validate_binding_shape,
};
use crate::ImageFormat;

mod fullscreen;
pub use fullscreen::fullscreen_triangle_shaders;

/// Maximum distinct assets in one prepared Raster working set.
pub const MAX_RASTER_PROGRAMS: usize = 64;
/// Baseline descriptor-set/register-space limit shared by both backends.
pub const MAX_RASTER_BIND_GROUPS: usize = 4;
pub const MAX_RASTER_ATTRIBUTES: usize = 16;
pub const MAX_RASTER_BINDINGS: usize = 26;
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
    /// Vertex buffer binding (0 or 1).
    pub stream: u32,
    pub location: u32,
    pub offset: u32,
    pub format: VertexFormat,
}

/// Inline words begin at byte offset zero in each selected shader stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterParameterLayout {
    pub size: u32,
    pub stages: ShaderStages,
}

/// The shader stages for one raster backend. A depth-only program may omit
/// its fragment stage or supply one for alpha discard.
#[derive(Clone, Copy)]
pub struct RasterShaderStages<B = &'static [u8]> {
    pub vertex: B,
    pub fragment: Option<B>,
}

impl<B> RasterShaderStages<B> {
    fn map<T>(self, mut stage: impl FnMut(B) -> T) -> RasterShaderStages<T> {
        RasterShaderStages {
            vertex: stage(self.vertex),
            fragment: self.fragment.map(stage),
        }
    }
}

impl RasterShaderStages<Box<[u8]>> {
    fn as_view(&self) -> RasterShaderStages<&[u8]> {
        RasterShaderStages {
            vertex: &self.vertex,
            fragment: self.fragment.as_deref(),
        }
    }
}

/// Shader stages and the root-constant binding for a D3D12 raster program.
#[derive(Clone, Copy)]
pub struct D3D12RasterShaders<B = &'static [u8]> {
    pub stages: RasterShaderStages<B>,
    pub inline_constants: Option<D3D12InlineConstantBinding>,
}

/// CBV register and register space used for D3D12 root constants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3D12InlineConstantBinding {
    pub register: u32,
    pub space: u32,
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

/// Graphics shader assets and their complete ABI.
#[derive(Clone, Copy)]
pub struct RasterProgramDescriptor<'a, B = &'a [u8]> {
    pub entry: &'a str,
    /// Zero selects a generated fullscreen triangle and requires no attributes.
    pub vertex_stride: u32,
    /// Stride of optional independent per-vertex input at binding 1. Zero disables it.
    pub secondary_vertex_stride: u32,
    pub vertex_attributes: &'a [VertexAttribute],
    pub parameters: RasterParameterLayout,
    pub color_outputs: &'a [ImageFormat],
    pub bindings: &'a [BindingLayout],
    /// Buffer-prefix footprints in binding order; zero for images/samplers.
    pub binding_bytes: &'a [u64],
    /// Complete possible sampling combinations in both shader stages.
    pub sampling_pairs: &'a [SampledImageSamplerPair],
    pub blend: BlendState,
    pub depth: DepthState,
    pub shaders: ShaderBinaries<RasterShaderStages<B>, D3D12RasterShaders<B>>,
}

impl<B> RasterProgramDescriptor<'_, B> {
    fn validate_layout(&self) -> Result<(), ShaderImportError> {
        let graphics = ShaderStages::VERTEX.union(ShaderStages::FRAGMENT);
        if !self.secondary_vertex_stride.is_multiple_of(4)
            || (self.secondary_vertex_stride != 0)
                != self.vertex_attributes.iter().any(|a| a.stream == 1)
            || !self.vertex_stride.is_multiple_of(4)
            || self.vertex_attributes.len() > MAX_RASTER_ATTRIBUTES
            || (self.vertex_stride == 0 && !self.vertex_attributes.is_empty())
        {
            return Err(ShaderImportError::InvalidDescriptor("raster vertex layout"));
        }
        for (index, attribute) in self.vertex_attributes.iter().enumerate() {
            if attribute.stream > 1
                || attribute.location >= MAX_RASTER_ATTRIBUTES as u32
                || !attribute.offset.is_multiple_of(4)
                || attribute
                    .offset
                    .checked_add(attribute.format.size())
                    .is_none_or(|end| {
                        end > if attribute.stream == 0 {
                            self.vertex_stride
                        } else {
                            self.secondary_vertex_stride
                        }
                    })
                || self.vertex_attributes[..index]
                    .iter()
                    .any(|other| other.location == attribute.location)
            {
                return Err(ShaderImportError::InvalidDescriptor(
                    "raster vertex attribute",
                ));
            }
        }
        if !self.parameters.size.is_multiple_of(4)
            || self.parameters.size > 128
            || self.parameters.stages.is_empty() != (self.parameters.size == 0)
            || !graphics.contains(self.parameters.stages)
        {
            return Err(ShaderImportError::InvalidDescriptor(
                "raster inline parameters",
            ));
        }
        if self.color_outputs.len() > MAX_RASTER_COLOR_ATTACHMENTS
            || self
                .color_outputs
                .iter()
                .any(|format| !matches!(format, ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float))
        {
            return Err(ShaderImportError::InvalidDescriptor("raster color outputs"));
        }
        if self.bindings.len() > MAX_RASTER_BINDINGS
            || self.bindings.iter().any(|slot| {
                slot.group >= MAX_RASTER_BIND_GROUPS as u32
                    || !matches!(
                        slot.kind,
                        BindingType::Uniform
                            | BindingType::StorageRead
                            | BindingType::SampledImage
                            | BindingType::SampledDepth
                            | BindingType::Sampler
                            | BindingType::ComparisonSampler
                    )
            })
        {
            return Err(ShaderImportError::InvalidDescriptor("raster bindings"));
        }
        let kind_at = |group, binding| {
            self.bindings
                .iter()
                .find(|slot| (slot.group, slot.binding) == (group, binding))
                .map(|slot| slot.kind)
        };
        if self.sampling_pairs.iter().any(|pair| {
            !matches!(
                (
                    kind_at(pair.image_group, pair.image_binding),
                    kind_at(pair.sampler_group, pair.sampler_binding)
                ),
                (
                    Some(BindingType::SampledImage | BindingType::SampledDepth),
                    Some(BindingType::Sampler)
                ) | (
                    Some(BindingType::SampledDepth),
                    Some(BindingType::ComparisonSampler)
                )
            )
        }) {
            return Err(ShaderImportError::InvalidDescriptor(
                "raster sampling pairs",
            ));
        }
        Ok(())
    }
}

fn validate_stages(
    shaders: &RasterShaderStages<Box<[u8]>>,
    requires_fragment: bool,
    valid: impl Fn(&[u8], binary::Stage) -> bool,
    error: impl Fn(ShaderStages) -> ShaderImportError,
) -> Result<(), ShaderImportError> {
    if !valid(&shaders.vertex, binary::Stage::Vertex) {
        return Err(error(ShaderStages::VERTEX));
    }
    if let Some(fragment) = &shaders.fragment {
        if !valid(fragment, binary::Stage::Fragment) {
            return Err(error(ShaderStages::FRAGMENT));
        }
    } else if requires_fragment {
        return Err(ShaderImportError::InvalidDescriptor(
            "raster fragment stage is required",
        ));
    }
    Ok(())
}

/// An immutable trusted vertex/fragment shader pair, independent of a native
/// pipeline. Device limits and format capabilities are established at preparation.
#[derive(Clone)]
pub struct ManagedRasterProgram {
    manifest: ProgramAsset<RasterProgramDescriptor<'static>, OwnedRasterProgram>,
}

type RasterBinaries<B> = ShaderBinaries<RasterShaderStages<B>, D3D12RasterShaders<B>>;

struct OwnedRasterProgram {
    vertex_stride: u32,
    secondary_vertex_stride: u32,
    vertex_attributes: Box<[VertexAttribute]>,
    parameters: RasterParameterLayout,
    color_outputs: Box<[ImageFormat]>,
    bindings: Box<[BindingLayout]>,
    binding_bytes: Box<[u64]>,
    sampling_pairs: Box<[SampledImageSamplerPair]>,
    blend: BlendState,
    depth: DepthState,
    shaders: RasterBinaries<Box<[u8]>>,
}

/// A raster pipeline variant to prepare before frame processing.
#[derive(Clone)]
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
        stream: 0,
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x3,
    },
    VertexAttribute {
        stream: 0,
        location: 1,
        offset: 12,
        format: VertexFormat::Float32x4,
    },
];
pub(crate) static MESH: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 28,
    secondary_vertex_stride: 0,
    vertex_attributes: MESH_ATTRIBUTES,
    parameters: RasterParameterLayout {
        size: 80,
        stages: ShaderStages::VERTEX,
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
    shaders: embedded_raster_shaders!(
        "../../shaders/mesh.vert.spv",
        Some(include_bytes!("../../shaders/mesh.frag.spv")),
        "../../shaders/mesh.vert.wgsl",
        Some(include_bytes!("../../shaders/mesh.frag.wgsl")),
        "../../shaders/dxil/mesh.vert.dxil",
        Some(include_bytes!("../../shaders/dxil/mesh.frag.dxil")),
        Some(D3D12InlineConstantBinding {
            register: 0,
            space: 0
        }),
    ),
};
static TEXTURED_MESH: RasterProgramDescriptor = RasterProgramDescriptor {
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
    shaders: embedded_raster_shaders!(
        "../../shaders/textured.vert.spv",
        Some(include_bytes!("../../shaders/textured.frag.spv")),
        "../../shaders/textured.vert.wgsl",
        Some(include_bytes!("../../shaders/textured.frag.wgsl")),
        "../../shaders/dxil/textured.vert.dxil",
        Some(include_bytes!("../../shaders/dxil/textured.frag.dxil")),
        Some(D3D12InlineConstantBinding {
            register: 1,
            space: 0
        }),
    ),
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
    shaders: embedded_raster_shaders!(
        "../../shaders/mesh.vert.spv",
        None,
        "../../shaders/mesh.vert.wgsl",
        None,
        "../../shaders/dxil/mesh.vert.dxil",
        None,
        Some(D3D12InlineConstantBinding {
            register: 0,
            space: 0
        }),
    ),
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
    /// Each supplied backend must have valid Vulkan 1.3/SPIR-V 1.3, D3D12 SM6.0 or WGSL
    /// vertex and optional fragment shaders for `entry = "main"`, as selected by
    /// its slot, using only enabled backend features. Stage interfaces, descriptor slots, vertex attributes,
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
    /// An omitted fragment stage is `None` in the stage collection of each
    /// supplied format, with no inputs, bindings or inline parameters for it.
    /// Fragment depth, stencil, sample-mask writes, sample shading, storage writes,
    /// atomics, input attachments and attachment feedback are unsupported.
    ///
    /// Parameters have a four-byte multiple size at most 128 bytes, start at
    /// offset zero, and are used only in the nonempty declared Vertex/Fragment
    /// stages (zero size requires EMPTY stages). When supplying D3D12 shaders,
    /// specify `inline_constants` if the size is nonzero and leave it `None`
    /// otherwise. DXIL represents this block as that CBV register and space,
    /// with no overlapping Uniform binding.
    /// WGSL uses a uniform at group 0, binding 255 for the inline block, with
    /// the same byte offsets and no overlapping binding. Nearest-only samplers
    /// paired with unfilterable float/depth views must be declared in its metadata.
    /// Inline members must not be dynamically indexed: D3D12 root constants
    /// permit only static/literal indexing. Use Uniform/StorageRead for arrays
    /// that require dynamic indexing.
    /// There are at most [`MAX_RASTER_BINDINGS`] bindings, with group indices below 4, unique
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
        descriptor: &'static RasterProgramDescriptor<'static>,
    ) -> Self {
        Self {
            manifest: ProgramAsset::Embedded(descriptor),
        }
    }

    /// Import raster shader bytes loaded by the application. Moves the binary
    /// storage into the Program and copies the ABI slices at this boundary.
    /// Cloning a Program shares the same immutable version.
    ///
    /// # Safety
    ///
    /// The complete shader, ABI and execution requirements of
    /// [`Self::from_trusted_descriptor`] apply to the loaded bytes. Import checks
    /// the descriptor shape and selected binary headers. The caller must
    /// establish the bytecode validity, its exact match to the declared ABI and
    /// pipeline state, and all documented shader behavior before import.
    pub unsafe fn from_trusted_owned_descriptor<B: Into<Box<[u8]>>>(
        descriptor: RasterProgramDescriptor<'_, B>,
    ) -> Result<Self, ShaderImportError> {
        if descriptor.entry != "main" {
            return Err(ShaderImportError::InvalidDescriptor("raster entry point"));
        }
        descriptor.validate_layout()?;
        validate_binding_shape(
            descriptor.bindings,
            descriptor.binding_bytes,
            ShaderStages::VERTEX.union(ShaderStages::FRAGMENT),
        )?;
        let shaders = descriptor.shaders.map(
            |spirv| spirv.map(Into::into),
            |dxil| D3D12RasterShaders {
                stages: dxil.stages.map(Into::into),
                inline_constants: dxil.inline_constants,
            },
        );
        if shaders.is_empty() {
            return Err(ShaderImportError::MissingBinaries);
        }
        let requires_fragment = !descriptor.color_outputs.is_empty()
            || descriptor
                .parameters
                .stages
                .contains(ShaderStages::FRAGMENT)
            || descriptor
                .bindings
                .iter()
                .any(|slot| slot.stages.contains(ShaderStages::FRAGMENT));
        #[cfg(feature = "webgpu")]
        if let Some(stages) = &shaders.wgsl {
            validate_stages(
                stages,
                requires_fragment,
                binary::valid_wgsl,
                ShaderImportError::InvalidWgsl,
            )?;
            if shaders.nonfiltering.iter().any(|&(g, b)| {
                !descriptor.bindings.iter().any(|slot| {
                    slot.group == g && slot.binding == b && slot.kind == BindingType::Sampler
                })
            }) || (descriptor.parameters.size != 0
                && descriptor
                    .bindings
                    .iter()
                    .any(|slot| slot.group == 0 && slot.binding == super::WEBGPU_INLINE_BINDING))
            {
                return Err(ShaderImportError::InvalidDescriptor(
                    "WebGPU binding layout",
                ));
            }
        }
        if let Some(spirv) = &shaders.vulkan {
            validate_stages(
                spirv,
                requires_fragment,
                binary::valid_spirv,
                ShaderImportError::InvalidSpirv,
            )?;
        }
        if let Some(dxil) = &shaders.d3d12 {
            if dxil.inline_constants.is_some() != (descriptor.parameters.size != 0)
                || dxil.inline_constants.is_some_and(|constants| {
                    descriptor.bindings.iter().any(|slot| {
                        slot.kind == BindingType::Uniform
                            && (slot.group, slot.binding) == (constants.space, constants.register)
                    })
                })
            {
                return Err(ShaderImportError::InvalidDescriptor("D3D12 root constants"));
            }
            validate_stages(
                &dxil.stages,
                requires_fragment,
                binary::valid_dxil,
                ShaderImportError::InvalidDxil,
            )?;
        }
        Ok(Self {
            manifest: ProgramAsset::loaded(OwnedRasterProgram {
                vertex_stride: descriptor.vertex_stride,
                secondary_vertex_stride: descriptor.secondary_vertex_stride,
                vertex_attributes: descriptor.vertex_attributes.into(),
                parameters: descriptor.parameters,
                color_outputs: descriptor.color_outputs.into(),
                bindings: descriptor.bindings.into(),
                binding_bytes: descriptor.binding_bytes.into(),
                sampling_pairs: descriptor.sampling_pairs.into(),
                blend: descriptor.blend,
                depth: descriptor.depth,
                shaders,
            }),
        })
    }

    pub const fn mesh() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&MESH),
        }
    }
    pub const fn mesh_hdr() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&MESH_HDR),
        }
    }
    /// Read-only LESS_EQUAL depth after the same geometry's depth prepass.
    pub const fn mesh_prepassed() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&MESH_PREPASSED),
        }
    }
    pub const fn mesh_hdr_prepassed() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&MESH_HDR_PREPASSED),
        }
    }
    pub const fn depth_only() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&DEPTH_ONLY),
        }
    }
    /// Mesh with a vertex Uniform offset and fragment image/sampler bindings.
    pub const fn textured_mesh() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&TEXTURED_MESH),
        }
    }
    pub const fn textured_mesh_hdr() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&TEXTURED_MESH_HDR),
        }
    }
    /// Borrow the complete immutable ABI and any supplied backend binaries.
    pub fn descriptor(&self) -> RasterProgramDescriptor<'_> {
        match &self.manifest {
            ProgramAsset::Embedded(descriptor) => **descriptor,
            ProgramAsset::Loaded(program) => {
                let owned = &program.descriptor;
                RasterProgramDescriptor {
                    entry: "main",
                    vertex_stride: owned.vertex_stride,
                    secondary_vertex_stride: owned.secondary_vertex_stride,
                    vertex_attributes: &owned.vertex_attributes,
                    parameters: owned.parameters,
                    color_outputs: &owned.color_outputs,
                    bindings: &owned.bindings,
                    binding_bytes: &owned.binding_bytes,
                    sampling_pairs: &owned.sampling_pairs,
                    blend: owned.blend,
                    depth: owned.depth,
                    shaders: owned
                        .shaders
                        .as_ref()
                        .map(RasterShaderStages::as_view, |dxil| D3D12RasterShaders {
                            stages: dxil.stages.as_view(),
                            inline_constants: dxil.inline_constants,
                        }),
                }
            }
        }
    }
    /// Identity of this immutable asset, including all fixed pipeline states.
    pub(crate) fn identity(&self) -> ProgramId {
        self.manifest.identity()
    }
    pub fn same(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
    pub fn vertex_stride(&self) -> u32 {
        self.descriptor().vertex_stride
    }
    pub(crate) fn is_fullscreen(&self) -> bool {
        self.vertex_stride() == 0
    }
    pub fn vertex_attributes(&self) -> &[VertexAttribute] {
        self.descriptor().vertex_attributes
    }
    pub fn parameter_size(&self) -> u32 {
        self.descriptor().parameters.size
    }
    pub fn parameter_layout(&self) -> RasterParameterLayout {
        self.descriptor().parameters
    }
    pub fn color_outputs(&self) -> &[ImageFormat] {
        self.descriptor().color_outputs
    }
    pub fn bindings(&self) -> &[BindingLayout] {
        self.descriptor().bindings
    }
    pub fn binding_bytes(&self) -> &[u64] {
        self.descriptor().binding_bytes
    }
    pub fn sampling_pairs(&self) -> &[SampledImageSamplerPair] {
        self.descriptor().sampling_pairs
    }
    pub fn blend(&self) -> BlendState {
        self.descriptor().blend
    }
    pub fn depth(&self) -> DepthState {
        self.descriptor().depth
    }
    pub fn requires_depth(&self) -> bool {
        matches!(self.depth(), DepthState::Required { .. })
    }
    pub fn requires_depth_load(&self) -> bool {
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
            .any(|slot| slot.kind.descriptor_class() == BindingType::SampledImage)
    }
    #[cfg(test)]
    fn is_prepassed(&self) -> bool {
        self.requires_depth_load()
    }
    /// WGSL modules for this immutable raster program.
    #[cfg(feature = "webgpu")]
    pub fn wgsl(&self) -> Option<RasterShaderStages<&str>> {
        let stages = self.descriptor().shaders.wgsl?;
        Some(RasterShaderStages {
            vertex: std::str::from_utf8(stages.vertex).ok()?,
            fragment: stages.fragment.map(std::str::from_utf8).transpose().ok()?,
        })
    }

    /// Nearest-only WebGPU sampler bindings, including manual depth comparisons.
    #[cfg(feature = "webgpu")]
    pub fn webgpu_nonfiltering_samplers(&self) -> &[(u32, u32)] {
        self.descriptor().shaders.nonfiltering
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn spirv(&self) -> Option<RasterShaderStages<&[u8]>> {
        self.descriptor().shaders.vulkan
    }
    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn dxil(&self) -> Option<D3D12RasterShaders<&[u8]>> {
        self.descriptor().shaders.d3d12
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "webgpu")]
    #[test]
    fn owned_wgsl_preserves_stages_and_nearest_sampler_layout() {
        let stages = TEXTURED_MESH.shaders.wgsl.unwrap();
        let descriptor = RasterProgramDescriptor {
            shaders: ShaderBinaries::webgpu(stages, &[(1, 1)]),
            ..TEXTURED_MESH
        };
        // SAFETY: The shipped textured-mesh ABI is unchanged, and the sampler
        // restriction is compatible with its ordinary float texture sampling.
        let loaded =
            unsafe { ManagedRasterProgram::from_trusted_owned_descriptor(descriptor) }.unwrap();
        assert_eq!(loaded.wgsl().unwrap().vertex.as_bytes(), stages.vertex);
        assert_eq!(
            loaded.wgsl().unwrap().fragment.map(str::as_bytes),
            stages.fragment
        );
        assert_eq!(loaded.webgpu_nonfiltering_samplers(), &[(1, 1)]);
        assert!(
            ManagedRasterProgram::depth_only()
                .wgsl()
                .unwrap()
                .fragment
                .is_none()
        );
    }

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
        assert!(
            all.iter().enumerate().all(|(index, program)| {
                all[..index].iter().all(|earlier| !earlier.same(program))
            }),
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
                assert_eq!(
                    program.spirv().unwrap().vertex,
                    original.spirv().unwrap().vertex
                );
                assert_eq!(
                    program.spirv().unwrap().fragment,
                    original.spirv().unwrap().fragment
                );
            }
            #[cfg(all(feature = "d3d12", windows))]
            {
                assert_eq!(
                    program.dxil().unwrap().stages.vertex,
                    original.dxil().unwrap().stages.vertex
                );
                assert_eq!(
                    program.dxil().unwrap().stages.fragment,
                    original.dxil().unwrap().stages.fragment
                );
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
                assert_eq!(
                    program.spirv().unwrap().vertex,
                    original.spirv().unwrap().vertex
                );
                assert_eq!(
                    program.spirv().unwrap().fragment,
                    original.spirv().unwrap().fragment
                );
            }
            #[cfg(all(feature = "d3d12", windows))]
            {
                assert_eq!(
                    program.dxil().unwrap().stages.vertex,
                    original.dxil().unwrap().stages.vertex
                );
                assert_eq!(
                    program.dxil().unwrap().stages.fragment,
                    original.dxil().unwrap().stages.fragment
                );
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
            assert_eq!(
                program.spirv().unwrap().vertex,
                mesh.spirv().unwrap().vertex
            );
            assert!(program.spirv().unwrap().fragment.is_none());
        }
        #[cfg(all(feature = "d3d12", windows))]
        {
            assert_eq!(
                program.dxil().unwrap().stages.vertex,
                mesh.dxil().unwrap().stages.vertex
            );
            assert!(program.dxil().unwrap().stages.fragment.is_none());
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
            program.spirv().unwrap().vertex,
            include_bytes!("../../shaders/mesh.vert.spv")
        );
        assert_eq!(
            program.spirv().unwrap().fragment,
            Some(include_bytes!("../../shaders/mesh.frag.spv").as_slice())
        );
    }

    #[cfg(feature = "vulkan")]
    #[test]
    fn textured_native_assets_are_distinct_from_the_mesh_pipeline() {
        let program = ManagedRasterProgram::textured_mesh();
        assert_eq!(
            program.spirv().unwrap().vertex,
            include_bytes!("../../shaders/textured.vert.spv")
        );
        assert_eq!(
            program.spirv().unwrap().fragment,
            Some(include_bytes!("../../shaders/textured.frag.spv").as_slice())
        );
        assert_ne!(
            program.spirv().unwrap().vertex,
            ManagedRasterProgram::mesh().spirv().unwrap().vertex
        );
        assert_ne!(
            program.spirv().unwrap().fragment,
            ManagedRasterProgram::mesh().spirv().unwrap().fragment
        );
    }
}
