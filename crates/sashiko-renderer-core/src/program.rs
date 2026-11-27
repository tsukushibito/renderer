//! Managed shader interfaces and trusted Program descriptors.
//!
//! Program descriptors define bindings and the actual shader access footprint.
//! [`crate::graph::Graph::dispatch`] and [`crate::graph::Graph::raster_bindings`]
//! own the logical operands for execution. Caller-defined shader assets enter
//! through an explicit unsafe descriptor boundary.

use crate::ImageFormat;
use std::sync::Arc;

mod binary;
mod raster;
mod sampler;
pub use sampler::{AddressMode, FilterMode, MipFilter, SamplerDescriptor, SamplerError};

#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) use raster::MESH as MESH_RASTER_DESCRIPTOR;
pub use raster::{
    BlendState, CompareOp, D3D12InlineConstantBinding, D3D12RasterShaders, DepthState,
    MAX_RASTER_ATTRIBUTES, MAX_RASTER_BIND_GROUPS, MAX_RASTER_BINDINGS,
    MAX_RASTER_COLOR_ATTACHMENTS, MAX_RASTER_PROGRAMS, ManagedRasterProgram, RasterParameterLayout,
    RasterPipelineRequest, RasterProgramDescriptor, RasterShaderStages, SampledImageSamplerPair,
    VertexAttribute, VertexFormat, fullscreen_triangle_shaders,
};

/// Backend-specific binaries for one immutable Program contract.
///
/// Supply the formats available in the application build. Each runtime uses
/// its own format and reports an unsupported Program during preparation if
/// that format is unavailable.
#[derive(Clone, Copy)]
pub struct ShaderBinaries<V, D = V> {
    vulkan: Option<V>,
    d3d12: Option<D>,
    #[cfg(feature = "webgpu")]
    wgsl: Option<V>,
    #[cfg(feature = "webgpu")]
    nonfiltering: &'static [(u32, u32)],
}

impl<V, D> ShaderBinaries<V, D> {
    pub const fn vulkan(spirv: V) -> Self {
        Self {
            vulkan: Some(spirv),
            d3d12: None,
            #[cfg(feature = "webgpu")]
            wgsl: None,
            #[cfg(feature = "webgpu")]
            nonfiltering: &[],
        }
    }

    pub const fn d3d12(dxil: D) -> Self {
        Self {
            vulkan: None,
            d3d12: Some(dxil),
            #[cfg(feature = "webgpu")]
            wgsl: None,
            #[cfg(feature = "webgpu")]
            nonfiltering: &[],
        }
    }

    pub const fn both(spirv: V, dxil: D) -> Self {
        Self {
            vulkan: Some(spirv),
            d3d12: Some(dxil),
            #[cfg(feature = "webgpu")]
            wgsl: None,
            #[cfg(feature = "webgpu")]
            nonfiltering: &[],
        }
    }

    /// Policy and ABI metadata for CPU-only Graph construction.
    pub const fn metadata_only() -> Self {
        Self {
            vulkan: None,
            d3d12: None,
            #[cfg(feature = "webgpu")]
            wgsl: None,
            #[cfg(feature = "webgpu")]
            nonfiltering: &[],
        }
    }

    /// WebGPU source and the sampler coordinates that require nearest-only bindings
    /// (including samplers paired with depth views). The bytes contain UTF-8 WGSL.
    #[cfg(feature = "webgpu")]
    pub const fn webgpu(wgsl: V, nonfiltering: &'static [(u32, u32)]) -> Self {
        Self {
            vulkan: None,
            d3d12: None,
            wgsl: Some(wgsl),
            nonfiltering,
        }
    }

    #[cfg(feature = "webgpu")]
    #[doc(hidden)]
    pub const fn with_webgpu(mut self, wgsl: V, nonfiltering: &'static [(u32, u32)]) -> Self
    where
        V: Copy,
        D: Copy,
    {
        self.wgsl = Some(wgsl);
        self.nonfiltering = nonfiltering;
        self
    }

    /// Samplers paired with unfilterable float/depth views in this WGSL asset.
    #[cfg(feature = "webgpu")]
    pub const fn webgpu_nonfiltering_samplers(&self) -> &'static [(u32, u32)] {
        self.nonfiltering
    }

    fn map<U, E>(
        self,
        mut stage: impl FnMut(V) -> U,
        d3d12: impl FnOnce(D) -> E,
    ) -> ShaderBinaries<U, E> {
        ShaderBinaries {
            vulkan: self.vulkan.map(&mut stage),
            d3d12: self.d3d12.map(d3d12),
            #[cfg(feature = "webgpu")]
            wgsl: self.wgsl.map(stage),
            #[cfg(feature = "webgpu")]
            nonfiltering: self.nonfiltering,
        }
    }

    fn is_empty(&self) -> bool {
        #[cfg(feature = "webgpu")]
        if self.wgsl.is_some() {
            return false;
        }
        self.vulkan.is_none() && self.d3d12.is_none()
    }

    fn as_ref(&self) -> ShaderBinaries<&V, &D> {
        ShaderBinaries {
            vulkan: self.vulkan.as_ref(),
            d3d12: self.d3d12.as_ref(),
            #[cfg(feature = "webgpu")]
            wgsl: self.wgsl.as_ref(),
            #[cfg(feature = "webgpu")]
            nonfiltering: self.nonfiltering,
        }
    }
}

/// Rejection at the runtime shader import boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderImportError {
    MissingBinaries,
    InvalidDescriptor(&'static str),
    InvalidSpirv(ShaderStages),
    InvalidDxil(ShaderStages),
    #[cfg(feature = "webgpu")]
    InvalidWgsl(ShaderStages),
}

impl std::fmt::Display for ShaderImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBinaries => f.write_str("runtime Program has no shader binaries"),
            Self::InvalidDescriptor(reason) => write!(f, "invalid Program descriptor: {reason}"),
            #[cfg(feature = "webgpu")]
            Self::InvalidWgsl(stage) => write!(f, "invalid {stage:?} WGSL source"),
            Self::InvalidSpirv(stage) => write!(f, "invalid {stage:?} SPIR-V module"),
            Self::InvalidDxil(stage) => write!(f, "invalid {stage:?} DXIL container"),
        }
    }
}

impl std::error::Error for ShaderImportError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramId {
    Embedded(usize),
    Loaded(u64),
}

enum ProgramAsset<S: 'static, O> {
    Embedded(&'static S),
    Loaded(Arc<LoadedProgram<O>>),
}

struct LoadedProgram<O> {
    id: u64,
    descriptor: O,
}

impl<S, O> Clone for ProgramAsset<S, O> {
    fn clone(&self) -> Self {
        match self {
            Self::Embedded(descriptor) => Self::Embedded(descriptor),
            Self::Loaded(program) => Self::Loaded(Arc::clone(program)),
        }
    }
}

impl<S, O> ProgramAsset<S, O> {
    fn loaded(descriptor: O) -> Self {
        Self::Loaded(Arc::new(LoadedProgram {
            id: crate::graph::next_id(),
            descriptor,
        }))
    }

    fn identity(&self) -> ProgramId {
        match self {
            Self::Embedded(descriptor) => {
                ProgramId::Embedded(std::ptr::from_ref(*descriptor) as usize)
            }
            Self::Loaded(program) => ProgramId::Loaded(program.id),
        }
    }
}

fn validate_binding_shape(
    bindings: &[BindingLayout],
    footprints: &[u64],
    stages: ShaderStages,
) -> Result<(), ShaderImportError> {
    if bindings.len() != footprints.len() {
        return Err(ShaderImportError::InvalidDescriptor("binding footprints"));
    }
    for (index, (slot, &bytes)) in bindings.iter().zip(footprints).enumerate() {
        let buffer = matches!(
            slot.kind,
            BindingType::Uniform
                | BindingType::StorageRead
                | BindingType::StorageWrite
                | BindingType::StorageReadWrite
        );
        if slot.stages.is_empty()
            || !stages.contains(slot.stages)
            || bindings[..index]
                .iter()
                .any(|other| (other.group, other.binding) == (slot.group, slot.binding))
            || if buffer {
                bytes == 0 || bytes > slot.min_size
            } else {
                bytes != 0 || slot.min_size != 0
            }
        {
            return Err(ShaderImportError::InvalidDescriptor("binding layout"));
        }
    }
    Ok(())
}

#[cfg(all(feature = "vulkan", feature = "d3d12", windows))]
#[doc(hidden)]
#[macro_export]
macro_rules! __renderer_embedded_shader_binaries {
    ($spirv:expr, $dxil:expr $(,)?) => {
        $crate::program::ShaderBinaries::both($spirv, $dxil)
    };
}
#[cfg(all(feature = "vulkan", not(all(feature = "d3d12", windows))))]
#[doc(hidden)]
#[macro_export]
macro_rules! __renderer_embedded_shader_binaries {
    ($spirv:expr, $dxil:expr $(,)?) => {
        $crate::program::ShaderBinaries::vulkan($spirv)
    };
}
#[cfg(all(not(feature = "vulkan"), feature = "d3d12", windows))]
#[doc(hidden)]
#[macro_export]
macro_rules! __renderer_embedded_shader_binaries {
    ($spirv:expr, $dxil:expr $(,)?) => {
        $crate::program::ShaderBinaries::d3d12($dxil)
    };
}
#[cfg(not(any(feature = "vulkan", all(feature = "d3d12", windows))))]
#[doc(hidden)]
#[macro_export]
macro_rules! __renderer_embedded_shader_binaries {
    ($spirv:expr, $dxil:expr $(,)?) => {
        $crate::program::ShaderBinaries::metadata_only()
    };
}

/// Embed SPIR-V and/or DXIL according to the host's enabled Renderer backends.
/// Only expressions for enabled formats are expanded, including in downstream
/// crates that declare no backend features of their own.
#[doc(inline)]
pub use crate::__renderer_embedded_shader_binaries as embedded_shader_binaries;

/// Include browser shaders only in builds selecting WebGPU, even when used
/// through another crate that has no feature of its own.
#[cfg(feature = "webgpu")]
#[macro_export]
#[doc(hidden)]
macro_rules! __renderer_with_wgsl {
    ($native:expr, $wgsl:expr, $nonfiltering:expr) => {
        $crate::program::ShaderBinaries::with_webgpu($native, $wgsl, $nonfiltering)
    };
}
#[cfg(not(feature = "webgpu"))]
#[macro_export]
#[doc(hidden)]
macro_rules! __renderer_with_wgsl {
    ($native:expr, $wgsl:expr, $nonfiltering:expr) => {
        $native
    };
}
#[doc(inline)]
pub use crate::__renderer_with_wgsl as with_wgsl;

/// Binding in group zero reserved for inline raster parameters on WebGPU.
#[cfg(feature = "webgpu")]
pub const WEBGPU_INLINE_BINDING: u32 = 255;

macro_rules! embedded_raster_shaders {
    (
        $vertex_spirv:literal, $fragment_spirv:expr,
        $vertex_wgsl:literal, $fragment_wgsl:expr,
        $vertex_dxil:literal, $fragment_dxil:expr,
        $inline_constants:expr $(,)?
    ) => {
        $crate::program::with_wgsl!(
            $crate::program::embedded_shader_binaries!(
                $crate::program::RasterShaderStages {
                    vertex: include_bytes!($vertex_spirv),
                    fragment: $fragment_spirv,
                },
                $crate::program::D3D12RasterShaders {
                    stages: $crate::program::RasterShaderStages {
                        vertex: include_bytes!($vertex_dxil),
                        fragment: $fragment_dxil,
                    },
                    inline_constants: $inline_constants,
                },
            ),
            $crate::program::RasterShaderStages {
                vertex: include_bytes!($vertex_wgsl),
                fragment: $fragment_wgsl,
            },
            &[]
        )
    };
}
pub(crate) use embedded_raster_shaders;

/// Access derived from the trusted shader. Write access alone is not a promise
/// that every byte of a descriptor range will be initialized by a dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingType {
    Uniform,
    StorageRead,
    StorageWrite,
    /// Compute may read and modify any subset of this already initialized prefix.
    StorageReadWrite,
    SampledImage,
    /// Read-only typed storage image. The selected View must have this format.
    StorageImageRead(ImageFormat),
    /// Write-only typed storage image. An accepted dispatch initializes the
    /// complete selected View under the trusted Program's execution contract.
    StorageImageWrite(ImageFormat),
    Sampler,
}

/// Shader execution scopes derived from a reviewed Program, independent of
/// native pipeline flags. Vertex/index fetch and attachments have separate uses.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct ShaderStages(u8);

impl ShaderStages {
    pub const EMPTY: Self = Self(0);
    pub const VERTEX: Self = Self(1);
    pub const FRAGMENT: Self = Self(2);
    pub const COMPUTE: Self = Self(4);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}
impl std::fmt::Debug for ShaderStages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return f.write_str("EMPTY");
        }
        let mut separator = "";
        for (stage, name) in [
            (Self::VERTEX, "VERTEX"),
            (Self::FRAGMENT, "FRAGMENT"),
            (Self::COMPUTE, "COMPUTE"),
        ] {
            if self.contains(stage) {
                write!(f, "{separator}{name}")?;
                separator = "|";
            }
        }
        Ok(())
    }
}

/// One reflected slot in an immutable Program interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingLayout {
    pub group: u32,
    pub binding: u32,
    pub kind: BindingType,
    pub stages: ShaderStages,
    /// Required buffer bytes; zero for image and sampler slots.
    pub min_size: u64,
}

/// A logical byte range, excluding native allocation padding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BufferRange {
    pub(crate) offset: u64,
    pub(crate) size: u64,
}

/// A reviewed shader asset. Ordinary builds use checked-in artifacts and need
/// no compiler. Caller-defined assets use [`Self::from_trusted_descriptor`] for
/// embedded shaders or [`Self::from_trusted_owned_descriptor`] for loaded shaders.
#[derive(Clone)]
pub struct ManagedProgram {
    manifest: ProgramAsset<ComputeProgramDescriptor<'static>, OwnedComputeProgram>,
}

/// Immutable compute asset inputs. The constructor's safety contract establishes
/// the relationship between these metadata and each supplied shader binary.
#[derive(Clone, Copy)]
pub struct ComputeProgramDescriptor<'a, B = &'a [u8]> {
    pub entry: &'a str,
    pub workgroup_size: [u32; 3],
    pub bindings: &'a [BindingLayout],
    pub dispatch: DispatchContract<'a>,
    pub shaders: ShaderBinaries<B>,
}

struct OwnedComputeProgram {
    workgroup_size: [u32; 3],
    bindings: Box<[BindingLayout]>,
    workgroups: OwnedDispatchWorkgroups,
    bytes: Box<[u64]>,
    #[cfg_attr(
        not(any(
            feature = "vulkan",
            feature = "webgpu",
            all(feature = "d3d12", windows)
        )),
        expect(
            dead_code,
            reason = "the CPU-only build retains but never prepares shader bytes"
        )
    )]
    shaders: ShaderBinaries<Box<[u8]>>,
}

// Only the buffer-grid dispatch carries variable-length metadata.
enum OwnedDispatchWorkgroups {
    Fixed([u32; 3]),
    Image2d {
        group: u32,
        binding: u32,
    },
    BufferGrid2d {
        group: u32,
        binding: u32,
        read_bindings: Box<[(u32, u32)]>,
        max_workgroups: [u32; 2],
    },
}

impl OwnedDispatchWorkgroups {
    fn import(
        domain: DispatchWorkgroups<'_>,
        bindings: &[BindingLayout],
        local_size: [u32; 3],
    ) -> Result<Self, ShaderImportError> {
        let kind = |group, binding| {
            bindings
                .iter()
                .find(|slot| (slot.group, slot.binding) == (group, binding))
                .map(|slot| slot.kind)
        };
        let error = ShaderImportError::InvalidDescriptor("compute dispatch domain");
        match domain {
            DispatchWorkgroups::Fixed(counts) if !counts.contains(&0) => Ok(Self::Fixed(counts)),
            DispatchWorkgroups::Image2d { group, binding }
                if local_size[2] == 1
                    && matches!(
                        kind(group, binding),
                        Some(BindingType::StorageImageWrite(_))
                    ) =>
            {
                Ok(Self::Image2d { group, binding })
            }
            DispatchWorkgroups::BufferGrid2d {
                group,
                binding,
                read_bindings,
                max_workgroups,
            } if !max_workgroups.contains(&0)
                && kind(group, binding) == Some(BindingType::StorageWrite) =>
            {
                for (index, &(read_group, read_binding)) in read_bindings.iter().enumerate() {
                    if kind(read_group, read_binding) != Some(BindingType::StorageRead)
                        || read_bindings[..index].contains(&(read_group, read_binding))
                    {
                        return Err(error);
                    }
                }
                Ok(Self::BufferGrid2d {
                    group,
                    binding,
                    read_bindings: read_bindings.into(),
                    max_workgroups,
                })
            }
            _ => Err(error),
        }
    }

    fn as_view(&self) -> DispatchWorkgroups<'_> {
        match *self {
            Self::Fixed(counts) => DispatchWorkgroups::Fixed(counts),
            Self::Image2d { group, binding } => DispatchWorkgroups::Image2d { group, binding },
            Self::BufferGrid2d {
                group,
                binding,
                ref read_bindings,
                max_workgroups,
            } => DispatchWorkgroups::BufferGrid2d {
                group,
                binding,
                read_bindings,
                max_workgroups,
            },
        }
    }
}

/// Reviewed execution footprint, separate from the ABI's minimum descriptor
/// size. Entries follow manifest binding order. A buffer entry is its byte
/// prefix except for the grid-sized slots of [`DispatchWorkgroups::BufferGrid2d`],
/// where it is the number of bytes accessed per workgroup. Image entries are zero and
/// cover the complete selected View. Samplers have no content access.
/// Changing shader behavior requires reviewing this contract too.
#[derive(Clone, Copy)]
pub struct DispatchContract<'a> {
    pub workgroups: DispatchWorkgroups<'a>,
    pub bytes: &'a [u64],
}

/// Reviewed dispatch domain, including how a variable dispatch establishes its
/// output footprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchWorkgroups<'a> {
    Fixed([u32; 3]),
    /// Ceil-divide this write-only storage image View's extent by the Program's
    /// local size. Every storage image binding must have that same extent.
    Image2d {
        group: u32,
        binding: u32,
    },
    /// Dispatch `[x, y, 1]`, with each axis between one and its inclusive
    /// maximum. The named storage buffer slot writes a tightly packed grid with
    /// one entry per workgroup, beginning at the binding offset. The
    /// corresponding [`DispatchContract::bytes`] entry gives the entry stride.
    BufferGrid2d {
        group: u32,
        binding: u32,
        /// Distinct read-only storage slots whose packed footprint also scales
        /// with the workgroup count, starting at each binding offset.
        read_bindings: &'a [(u32, u32)],
        max_workgroups: [u32; 2],
    },
}

impl std::fmt::Debug for ManagedProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedProgram")
            .field("entry", &self.entry_point())
            .field("workgroup_size", &self.workgroup_size())
            .field("bindings", &self.bindings())
            .finish()
    }
}

static UNIFORM_READBACK: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [1, 1, 1],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Fixed([1, 1, 1]),
        bytes: &[16, 16],
    },
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            stages: ShaderStages::COMPUTE,
            min_size: 16,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            stages: ShaderStages::COMPUTE,
            min_size: 16,
        },
    ],
    shaders: with_wgsl!(
        embedded_shader_binaries!(
            include_bytes!("../shaders/uniform.comp.spv"),
            include_bytes!("../shaders/dxil/uniform.comp.dxil"),
        ),
        include_bytes!("../shaders/uniform.comp.wgsl"),
        &[]
    ),
};

static STORAGE_READBACK: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [1, 1, 1],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Fixed([1, 1, 1]),
        bytes: &[16, 16],
    },
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::StorageRead,
            stages: ShaderStages::COMPUTE,
            min_size: 16,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            stages: ShaderStages::COMPUTE,
            min_size: 16,
        },
    ],
    shaders: with_wgsl!(
        embedded_shader_binaries!(
            include_bytes!("../shaders/storage_read.comp.spv"),
            include_bytes!("../shaders/dxil/storage_read.comp.dxil"),
        ),
        include_bytes!("../shaders/storage_read.comp.wgsl"),
        &[]
    ),
};

pub(crate) const INDIRECT_GROUP_SIZE: u32 = 64;
pub(crate) const MAX_INDIRECT_GROUPS: u32 = 65_535;
pub(crate) const INDIRECT_TEMPLATE_BYTES: u64 = 8;
pub(crate) const INDIRECT_MASK_BYTES: u64 = 4;
pub(crate) const INDIRECT_COMMAND_BYTES: u64 = 20;

static INDEXED_INDIRECT_ENCODER: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [INDIRECT_GROUP_SIZE, 1, 1],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::BufferGrid2d {
            group: 0,
            binding: 2,
            read_bindings: &[(0, 0), (0, 1)],
            max_workgroups: [MAX_INDIRECT_GROUPS, 1],
        },
        bytes: &[
            INDIRECT_TEMPLATE_BYTES * INDIRECT_GROUP_SIZE as u64,
            INDIRECT_MASK_BYTES * INDIRECT_GROUP_SIZE as u64,
            INDIRECT_COMMAND_BYTES * INDIRECT_GROUP_SIZE as u64,
        ],
    },
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::StorageRead,
            stages: ShaderStages::COMPUTE,
            min_size: INDIRECT_TEMPLATE_BYTES * INDIRECT_GROUP_SIZE as u64,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::StorageRead,
            stages: ShaderStages::COMPUTE,
            min_size: INDIRECT_MASK_BYTES * INDIRECT_GROUP_SIZE as u64,
        },
        BindingLayout {
            group: 0,
            binding: 2,
            kind: BindingType::StorageWrite,
            stages: ShaderStages::COMPUTE,
            min_size: INDIRECT_COMMAND_BYTES * INDIRECT_GROUP_SIZE as u64,
        },
    ],
    shaders: with_wgsl!(
        embedded_shader_binaries!(
            include_bytes!("../shaders/indexed_indirect.comp.spv"),
            include_bytes!("../shaders/dxil/indexed_indirect.comp.dxil"),
        ),
        include_bytes!("../shaders/indexed_indirect.comp.wgsl"),
        &[]
    ),
};

static SAMPLED_READBACK: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [1, 1, 1],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Fixed([1, 1, 1]),
        bytes: &[0, 0, 16],
    },
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            stages: ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            stages: ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            stages: ShaderStages::COMPUTE,
            min_size: 16,
        },
    ],
    shaders: with_wgsl!(
        embedded_shader_binaries!(
            include_bytes!("../shaders/sampled.comp.spv"),
            include_bytes!("../shaders/dxil/sampled.comp.dxil"),
        ),
        include_bytes!("../shaders/sampled.comp.wgsl"),
        &[]
    ),
};

static SAMPLED_FLOAT_READBACK: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [1, 1, 1],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Fixed([1, 1, 1]),
        bytes: &[0, 0, 64],
    },
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            stages: ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            stages: ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            stages: ShaderStages::COMPUTE,
            min_size: 64,
        },
    ],
    shaders: with_wgsl!(
        embedded_shader_binaries!(
            include_bytes!("../shaders/sampled_float.comp.spv"),
            include_bytes!("../shaders/dxil/sampled_float.comp.dxil"),
        ),
        include_bytes!("../shaders/sampled_float.comp.wgsl"),
        &[]
    ),
};

impl ManagedProgram {
    /// Import a statically owned, trusted compute asset without adding a
    /// Renderer operation or effect variant.
    ///
    /// # Safety
    ///
    /// Each supplied module must be valid Vulkan 1.3/SPIR-V 1.3, D3D12 SM6.0 or WGSL
    /// compute shader, as selected by its slot, for `entry = "main"` and exactly
    /// the declared nonzero local size. Its complete descriptor interface must match the unique,
    /// manifest-ordered `bindings`; descriptor arrays, push constants and
    /// undeclared resources or side effects are not permitted. Buffer minimum
    /// sizes and `dispatch.bytes` must have one entry per binding, with a nonzero
    /// byte prefix no larger than the minimum size for buffers and zero for
    /// image/sampler bindings. The selected `BufferGrid2d` slots use nonzero
    /// per-workgroup strides instead of fixed prefixes. All slots must have only
    /// `ShaderStages::COMPUTE`.
    /// Non-buffer slots must have minimum size zero. Images must be non-array,
    /// non-multisampled 2D float-component resources; shader arithmetic uses
    /// ordinary FP32 values, not shader Float16 or optional narrow types. Storage
    /// image format qualifiers must equal the slot's exact typed format.
    /// D3D12 register spaces equal groups and register numbers equal bindings:
    /// Uniform uses CBV, StorageRead/Write use root-descriptor-compatible raw or
    /// structured SRV/UAV buffers (not typed `Buffer<T>`), sampled images use
    /// SRVs, storage images use UAVs even when readonly, and samplers use sampler
    /// descriptors. Only the features enabled by the backend may be required.
    /// Workgroup/shared storage is not supported by this descriptor contract;
    /// the shader must not declare or use it.
    ///
    /// For every accepted dispatch and every permitted resource content, the
    /// shader may access only the declared buffer prefixes and selected image
    /// Views. It must not perform out-of-bounds accesses, races, unsupported
    /// feature-dependent operations, or omit required shader-internal
    /// synchronization. Write-only slots must initialize every declared byte or
    /// image texel, and no other contents. Read/write buffer slots may update a
    /// subset of their declared prefix, including via race-free 32-bit atomics.
    /// Aliased read/write bindings are
    /// rejected by Graph, but invocation-level race freedom remains a caller
    /// obligation. Only the typed formats supported by the backend are allowed.
    ///
    /// `Image2d` must name a write-only storage image slot, use local Z = 1, and
    /// safely guard rounded-up invocations. The shader must work for every
    /// nonzero accepted image extent, with all storage images sharing that
    /// extent. Sampled images may have different extents. `BufferGrid2d` must
    /// name a write-only storage buffer slot and use nonzero axis maxima. Its
    /// optional read slots must be distinct read-only storage buffer slots and
    /// may only read the prefix computed from the same workgroup count. Each
    /// workgroup coordinates writes so that its entire entry is initialized
    /// without racing another workgroup. The shader must write exactly
    /// `x * y * stride` bytes in the packed grid for every accepted dispatch
    /// and resource content. In particular, buffer addressing cannot depend on
    /// caller-provided dimensions unless it remains correct for every value
    /// those inputs may contain. The shader must safely handle every selected
    /// image extent with the buffer domain. `Fixed` counts must be nonzero
    /// and fixed-domain shaders must safely handle every selected image extent.
    /// Changing source, compiler options, binaries or metadata
    /// requires re-establishing these guarantees; reflection/hash validation
    /// alone does not establish them.
    pub const unsafe fn from_trusted_descriptor(
        descriptor: &'static ComputeProgramDescriptor<'static>,
    ) -> Self {
        Self {
            manifest: ProgramAsset::Embedded(descriptor),
        }
    }

    /// Import compute shader bytes loaded by the application. Moves the binary
    /// storage into the Program and copies the ABI slices at this boundary.
    /// Cloning a Program shares the same immutable version.
    ///
    /// # Safety
    ///
    /// The complete shader, ABI and execution requirements of
    /// [`Self::from_trusted_descriptor`] apply to the loaded bytes. Import checks
    /// the descriptor shape and selected binary headers. The caller must
    /// establish the bytecode validity, its exact match to the declared ABI and
    /// dispatch footprint, and all documented shader behavior before import.
    pub unsafe fn from_trusted_owned_descriptor<B: Into<Box<[u8]>>>(
        descriptor: ComputeProgramDescriptor<'_, B>,
    ) -> Result<Self, ShaderImportError> {
        if descriptor.entry != "main" || descriptor.workgroup_size.contains(&0) {
            return Err(ShaderImportError::InvalidDescriptor(
                "compute entry or local size",
            ));
        }
        validate_binding_shape(
            descriptor.bindings,
            descriptor.dispatch.bytes,
            ShaderStages::COMPUTE,
        )?;
        let workgroups = OwnedDispatchWorkgroups::import(
            descriptor.dispatch.workgroups,
            descriptor.bindings,
            descriptor.workgroup_size,
        )?;
        let shaders = descriptor.shaders.map(Into::into, Into::into);
        if shaders.is_empty() {
            return Err(ShaderImportError::MissingBinaries);
        }
        #[cfg(feature = "webgpu")]
        if shaders
            .wgsl
            .as_deref()
            .is_some_and(|bytes| !binary::valid_wgsl(bytes, binary::Stage::Compute))
        {
            return Err(ShaderImportError::InvalidWgsl(ShaderStages::COMPUTE));
        }
        if shaders
            .vulkan
            .as_deref()
            .is_some_and(|bytes| !binary::valid_spirv(bytes, binary::Stage::Compute))
        {
            return Err(ShaderImportError::InvalidSpirv(ShaderStages::COMPUTE));
        }
        if shaders
            .d3d12
            .as_deref()
            .is_some_and(|bytes| !binary::valid_dxil(bytes, binary::Stage::Compute))
        {
            return Err(ShaderImportError::InvalidDxil(ShaderStages::COMPUTE));
        }
        Ok(Self {
            manifest: ProgramAsset::loaded(OwnedComputeProgram {
                workgroup_size: descriptor.workgroup_size,
                bindings: descriptor.bindings.into(),
                workgroups,
                bytes: descriptor.dispatch.bytes.into(),
                shaders,
            }),
        })
    }

    /// Portable compute encoder used by [`crate::graph::Graph::encode_indexed_indirect`].
    /// Include it in the prepared Compute program set before submitting those graphs.
    pub const fn indexed_indirect_encoder() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&INDEXED_INDIRECT_ENCODER),
        }
    }

    /// Reviewed compute asset: one invocation copies exactly 16 bytes from the
    /// uniform slot into the storage slot. Extra descriptor bytes are untouched.
    pub const fn uniform_readback() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&UNIFORM_READBACK),
        }
    }

    /// Reviewed compute asset: one invocation copies exactly 16 bytes from a
    /// read-only storage slot into a write-only storage slot. Extra descriptor
    /// bytes are untouched. The input must be initialized before execution.
    pub const fn storage_readback() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&STORAGE_READBACK),
        }
    }

    /// Reviewed compute asset: four nearest/clamp samples, packed as RGBA8
    /// into exactly 16 storage bytes. Dispatch executes one invocation.
    pub const fn sampled_readback() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&SAMPLED_READBACK),
        }
    }

    /// Reviewed compute asset: four normalized-coordinate samples at View LOD
    /// zero preserve all RGBA FP32 component bits in exactly 64 storage bytes.
    /// The selected immutable sampler supplies filtering and addressing; the
    /// image format supplies sRGB decoding. RGBA16F needs no shader FP16 feature.
    /// Dispatch executes one invocation.
    pub const fn sampled_float_readback() -> Self {
        Self {
            manifest: ProgramAsset::Embedded(&SAMPLED_FLOAT_READBACK),
        }
    }

    pub const fn entry_point(&self) -> &'static str {
        match self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.entry,
            ProgramAsset::Loaded(_) => "main",
        }
    }

    pub fn workgroup_size(&self) -> [u32; 3] {
        match &self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.workgroup_size,
            ProgramAsset::Loaded(program) => program.descriptor.workgroup_size,
        }
    }

    pub fn bindings(&self) -> &[BindingLayout] {
        match &self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.bindings,
            ProgramAsset::Loaded(program) => &program.descriptor.bindings,
        }
    }

    /// The immutable, reviewed dispatch domain and resource footprints for this Program.
    pub fn dispatch_contract(&self) -> DispatchContract<'_> {
        match &self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.dispatch,
            ProgramAsset::Loaded(program) => DispatchContract {
                workgroups: program.descriptor.workgroups.as_view(),
                bytes: &program.descriptor.bytes,
            },
        }
    }

    pub(crate) fn identity(&self) -> ProgramId {
        self.manifest.identity()
    }

    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn same_asset(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }

    /// WGSL from this immutable program, available when packaged for WebGPU.
    #[cfg(feature = "webgpu")]
    pub fn wgsl(&self) -> Option<&str> {
        let bytes = match &self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.shaders.wgsl?,
            ProgramAsset::Loaded(program) => program.descriptor.shaders.wgsl.as_deref()?,
        };
        std::str::from_utf8(bytes).ok()
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn spirv(&self) -> Option<&[u8]> {
        match &self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.shaders.vulkan,
            ProgramAsset::Loaded(program) => program.descriptor.shaders.vulkan.as_deref(),
        }
    }

    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn dxil(&self) -> Option<&[u8]> {
        match &self.manifest {
            ProgramAsset::Embedded(descriptor) => descriptor.shaders.d3d12,
            ProgramAsset::Loaded(program) => program.descriptor.shaders.d3d12.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_program_keeps_only_enabled_backend_formats() {
        const BINARIES: ShaderBinaries<&[u8]> = with_wgsl!(
            embedded_shader_binaries!(b"spirv".as_slice(), b"dxil".as_slice()),
            b"wgsl".as_slice(),
            &[]
        );
        #[cfg(feature = "webgpu")]
        assert_eq!(BINARIES.wgsl, Some(b"wgsl".as_slice()));
        assert_eq!(
            BINARIES.vulkan,
            cfg!(feature = "vulkan").then_some(b"spirv".as_slice())
        );
        assert_eq!(
            BINARIES.d3d12,
            cfg!(all(windows, feature = "d3d12")).then_some(b"dxil".as_slice())
        );
    }

    #[test]
    fn indexed_indirect_encoder_matches_unsized_shader_buffer_layouts() {
        let reflection: serde_json::Value = serde_json::from_str(include_str!(
            "../shaders/dxil/indexed_indirect.comp.spirv-reflection.json"
        ))
        .unwrap();
        let program = ManagedProgram::indexed_indirect_encoder();
        assert_eq!(
            reflection["entryPoints"][0]["workgroup_size"],
            serde_json::json!(program.workgroup_size())
        );
        let buffers = reflection["ssbos"].as_array().unwrap();
        let contract = program.dispatch_contract();
        assert_eq!(buffers.len(), program.bindings().len());
        for ((buffer, slot), &footprint) in
            buffers.iter().zip(program.bindings()).zip(contract.bytes)
        {
            assert_eq!(buffer["set"], slot.group);
            assert_eq!(buffer["binding"], slot.binding);
            assert_eq!(buffer["block_size"], 0);
            match slot.kind {
                BindingType::StorageRead => assert_eq!(buffer["readonly"], true),
                BindingType::StorageWrite => assert_eq!(buffer["writeonly"], true),
                _ => panic!("indexed indirect uses only storage buffers"),
            }
            let member = &reflection["types"][buffer["type"].as_str().unwrap()]["members"][0];
            assert_eq!(member["offset"], 0);
            assert_eq!(member["array"], serde_json::json!([0]));
            let stride = member["array_stride"].as_u64().unwrap();
            assert_eq!(footprint, stride * u64::from(INDIRECT_GROUP_SIZE));
            assert_eq!(slot.min_size, footprint);
        }
    }

    #[test]
    fn managed_layout_matches_generated_shader_reflection() {
        for (program, json) in [
            (
                ManagedProgram::uniform_readback(),
                include_str!("../shaders/dxil/uniform.comp.spirv-reflection.json"),
            ),
            (
                ManagedProgram::sampled_readback(),
                include_str!("../shaders/dxil/sampled.comp.spirv-reflection.json"),
            ),
            (
                ManagedProgram::storage_readback(),
                include_str!("../shaders/dxil/storage_read.comp.spirv-reflection.json"),
            ),
            (
                ManagedProgram::sampled_float_readback(),
                include_str!("../shaders/dxil/sampled_float.comp.spirv-reflection.json"),
            ),
        ] {
            let reflected: serde_json::Value = serde_json::from_str(json).unwrap();
            let entry = &reflected["entryPoints"][0];
            assert_eq!(entry["name"], program.entry_point());
            assert_eq!(entry["mode"], "comp");
            assert_eq!(
                entry["workgroup_size"],
                serde_json::json!(program.workgroup_size())
            );
            let mut seen = Vec::new();
            for (category, kind) in [
                ("ubos", BindingType::Uniform),
                ("ssbos", BindingType::StorageWrite),
                ("separate_images", BindingType::SampledImage),
                ("separate_samplers", BindingType::Sampler),
            ] {
                for resource in reflected[category].as_array().into_iter().flatten() {
                    let kind = if category == "ssbos" && resource["readonly"] == true {
                        BindingType::StorageRead
                    } else {
                        kind
                    };
                    let slot = program
                        .bindings()
                        .iter()
                        .find(|slot| {
                            Some(u64::from(slot.group)) == resource["set"].as_u64()
                                && Some(u64::from(slot.binding)) == resource["binding"].as_u64()
                        })
                        .expect("reflected resource must have a managed slot");
                    assert_eq!(slot.kind, kind);
                    assert!(!seen.contains(&(slot.group, slot.binding)));
                    seen.push((slot.group, slot.binding));
                    if matches!(
                        kind,
                        BindingType::Uniform
                            | BindingType::StorageRead
                            | BindingType::StorageWrite
                            | BindingType::StorageReadWrite
                    ) {
                        assert_eq!(Some(slot.min_size), resource["block_size"].as_u64());
                        let member =
                            &reflected["types"][resource["type"].as_str().unwrap()]["members"][0];
                        assert_eq!(member["type"], "uvec4");
                        assert_eq!(member["offset"], 0);
                        if slot.min_size == 64 {
                            assert_eq!(member["array"], serde_json::json!([4]));
                            assert_eq!(member["array_stride"], 16);
                        }
                        if kind == BindingType::StorageWrite {
                            assert_eq!(resource["writeonly"], true);
                        } else if kind == BindingType::StorageRead {
                            assert_eq!(resource["readonly"], true);
                        }
                    } else {
                        assert_eq!(slot.min_size, 0);
                        assert_eq!(
                            resource["type"],
                            if kind == BindingType::SampledImage {
                                "texture2D"
                            } else {
                                "sampler"
                            }
                        );
                    }
                }
            }
            assert_eq!(seen.len(), program.bindings().len());
        }
    }
}
