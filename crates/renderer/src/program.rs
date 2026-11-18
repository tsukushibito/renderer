//! Managed shader interfaces and trusted Program descriptors.
//!
//! Program descriptors define bindings and the actual shader access footprint.
//! [`crate::graph::Graph::dispatch`] and [`crate::graph::Graph::raster_bindings`]
//! own the logical operands for execution. Caller-defined shader assets enter
//! through an explicit unsafe descriptor boundary.

use crate::ImageFormat;

mod raster;

pub use raster::{
    BlendState, CompareOp, DepthState, MAX_RASTER_ATTRIBUTES, MAX_RASTER_BIND_GROUPS,
    MAX_RASTER_BINDINGS, MAX_RASTER_COLOR_ATTACHMENTS, MAX_RASTER_PROGRAMS, ManagedRasterProgram,
    RasterParameterLayout, RasterPipelineRequest, RasterProgramDescriptor, SampledImageSamplerPair,
    VertexAttribute, VertexFormat,
};

/// Access derived from the trusted shader. Write access alone is not a promise
/// that every byte of a descriptor range will be initialized by a dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingType {
    Uniform,
    StorageRead,
    StorageWrite,
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

/// Reviewed normalized-coordinate sampler configurations, without comparison
/// or anisotropy. Native image/format capabilities are checked before use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplerDescriptor {
    /// Nearest minification/magnification/mip selection, clamp-to-edge on all
    /// axes, and LOD zero.
    NearestClamp,
    /// Linear minification/magnification and mip interpolation, repeat on all
    /// axes, and the complete selected View's LOD range.
    LinearRepeat,
    /// Linear minification/magnification/mip interpolation, clamp-to-edge on
    /// every axis, and the complete selected View's LOD range.
    LinearClamp,
    /// Linear minification/magnification/mip interpolation, repeat on U and
    /// clamp-to-edge on V/W, and the complete selected View's LOD range.
    LinearRepeatUClampV,
}
#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
impl SamplerDescriptor {
    pub(crate) const fn is_linear(self) -> bool {
        !matches!(self, Self::NearestClamp)
    }
}

/// A reviewed shader asset. Ordinary builds use checked-in artifacts and need
/// no compiler. Caller-defined assets use [`Self::from_trusted_descriptor`].
#[derive(Clone, Copy)]
pub struct ManagedProgram {
    manifest: &'static ComputeProgramDescriptor,
}

/// Immutable compute asset inputs. The constructor's safety contract establishes
/// the relationship between these metadata and both native shader binaries.
/// The slices must be static so frame plans retain them without allocating.
pub struct ComputeProgramDescriptor {
    pub entry: &'static str,
    pub workgroup_size: [u32; 3],
    pub bindings: &'static [BindingLayout],
    pub dispatch: DispatchContract,
    pub spirv: &'static [u8],
    pub dxil: &'static [u8],
}

/// Reviewed execution footprint, separate from the ABI's minimum descriptor
/// size. Entries follow manifest binding order. A buffer entry is its byte
/// prefix except for the grid-sized slots of [`DispatchWorkgroups::BufferGrid2d`],
/// where it is the number of bytes accessed per workgroup. Image entries are zero and
/// cover the complete selected View. Samplers have no content access.
/// Changing shader behavior requires reviewing this contract too.
pub struct DispatchContract {
    pub workgroups: DispatchWorkgroups,
    pub bytes: &'static [u64],
}

/// Reviewed dispatch domain, including how a variable dispatch establishes its
/// output footprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchWorkgroups {
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
        read_bindings: &'static [(u32, u32)],
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
    spirv: include_bytes!("../shaders/uniform.comp.spv"),
    dxil: include_bytes!("../shaders/dxil/uniform.comp.dxil"),
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
    spirv: include_bytes!("../shaders/storage_read.comp.spv"),
    dxil: include_bytes!("../shaders/dxil/storage_read.comp.dxil"),
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
    spirv: include_bytes!("../shaders/indexed_indirect.comp.spv"),
    dxil: include_bytes!("../shaders/dxil/indexed_indirect.comp.dxil"),
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
    spirv: include_bytes!("../shaders/sampled.comp.spv"),
    dxil: include_bytes!("../shaders/dxil/sampled.comp.dxil"),
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
    spirv: include_bytes!("../shaders/sampled_float.comp.spv"),
    dxil: include_bytes!("../shaders/dxil/sampled_float.comp.dxil"),
};

impl ManagedProgram {
    /// Import a statically owned, trusted compute asset without adding a
    /// Renderer operation or effect variant.
    ///
    /// # Safety
    ///
    /// Both binaries must be valid Vulkan 1.3/SPIR-V 1.3 and D3D12 SM6.0 compute
    /// shaders, respectively, for `entry = "main"` and exactly the declared nonzero local
    /// size. Their complete descriptor interfaces must match the unique,
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
    /// synchronization. Storage writes must initialize every declared byte or
    /// image texel, and no other contents. Aliased read/write bindings are
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
        descriptor: &'static ComputeProgramDescriptor,
    ) -> Self {
        Self {
            manifest: descriptor,
        }
    }

    /// Portable compute encoder used by [`crate::graph::Graph::encode_indexed_indirect`].
    /// Include it in the prepared Compute program set before submitting those graphs.
    pub const fn indexed_indirect_encoder() -> Self {
        Self {
            manifest: &INDEXED_INDIRECT_ENCODER,
        }
    }

    /// Reviewed compute asset: one invocation copies exactly 16 bytes from the
    /// uniform slot into the storage slot. Extra descriptor bytes are untouched.
    pub const fn uniform_readback() -> Self {
        Self {
            manifest: &UNIFORM_READBACK,
        }
    }

    /// Reviewed compute asset: one invocation copies exactly 16 bytes from a
    /// read-only storage slot into a write-only storage slot. Extra descriptor
    /// bytes are untouched. The input must be initialized before execution.
    pub const fn storage_readback() -> Self {
        Self {
            manifest: &STORAGE_READBACK,
        }
    }

    /// Reviewed compute asset: four nearest/clamp samples, packed as RGBA8
    /// into exactly 16 storage bytes. Dispatch executes one invocation.
    pub const fn sampled_readback() -> Self {
        Self {
            manifest: &SAMPLED_READBACK,
        }
    }

    /// Reviewed compute asset: four normalized-coordinate samples at View LOD
    /// zero preserve all RGBA FP32 component bits in exactly 64 storage bytes.
    /// The selected immutable sampler supplies filtering and addressing; the
    /// image format supplies sRGB decoding. RGBA16F needs no shader FP16 feature.
    /// Dispatch executes one invocation.
    pub const fn sampled_float_readback() -> Self {
        Self {
            manifest: &SAMPLED_FLOAT_READBACK,
        }
    }

    pub const fn entry_point(&self) -> &'static str {
        self.manifest.entry
    }

    pub const fn workgroup_size(&self) -> [u32; 3] {
        self.manifest.workgroup_size
    }

    pub const fn bindings(&self) -> &'static [BindingLayout] {
        self.manifest.bindings
    }

    pub(crate) const fn dispatch_contract(&self) -> &'static DispatchContract {
        &self.manifest.dispatch
    }

    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    pub(crate) fn same_asset(&self, other: &Self) -> bool {
        std::ptr::eq(self.manifest, other.manifest)
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn spirv(&self) -> &'static [u8] {
        self.manifest.spirv
    }

    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn dxil(&self) -> &'static [u8] {
        self.manifest.dxil
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    if kind.buffer_usage().is_some() {
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
