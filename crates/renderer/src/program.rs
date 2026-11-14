//! Managed shader interfaces, independent of a native backend or Graph.
//!
//! Programs describe their complete buffer binding interface. Uniform bytes
//! are copied once and stay immutable; storage is created without initialized
//! contents and may be written by the GPU. Immutable bindings retain the actual
//! Program and resource versions, not a promise of immutable storage contents.
//! General dispatch/Graph integration and arbitrary shader import are subsequent
//! work. Reflection alone cannot establish shader memory safety or synchronization.
//!
//! ```no_run
//! use renderer::program::{BufferBinding, BufferBindingDesc, BufferDescriptor, ManagedProgram};
//!
//! #[cfg(feature = "vulkan")]
//! fn create(runtime: &renderer::VulkanRuntime)
//!     -> Result<renderer::VulkanBindings, renderer::RuntimeError>
//! {
//!     let program = runtime.create_program(&ManagedProgram::uniform_readback())?;
//!     let uniform = runtime.create_managed_buffer(BufferDescriptor::Uniform { bytes: &[0; 16] })?;
//!     let storage = runtime.create_managed_buffer(BufferDescriptor::Storage { size: 272 })?;
//!     runtime.create_bindings(&program, &[
//!         BufferBinding { buffer: &uniform,
//!             desc: BufferBindingDesc { group: 0, binding: 0, offset: 0, size: 16 } },
//!         BufferBinding { buffer: &storage,
//!             desc: BufferBindingDesc { group: 1, binding: 0, offset: 256, size: 16 } },
//!     ])
//! }
//!
//! #[cfg(all(feature = "d3d12", windows))]
//! fn create_d3d12(runtime: &renderer::D3D12Runtime)
//!     -> Result<renderer::D3D12Bindings, renderer::D3D12RuntimeError>
//! {
//!     let program = runtime.create_program(&ManagedProgram::uniform_readback())?;
//!     let uniform = runtime.create_managed_buffer(BufferDescriptor::Uniform { bytes: &[0; 16] })?;
//!     let storage = runtime.create_managed_buffer(BufferDescriptor::Storage { size: 272 })?;
//!     runtime.create_bindings(&program, &[
//!         BufferBinding { buffer: &uniform,
//!             desc: BufferBindingDesc { group: 0, binding: 0, offset: 0, size: 16 } },
//!         BufferBinding { buffer: &storage,
//!             desc: BufferBindingDesc { group: 1, binding: 0, offset: 256, size: 16 } },
//!     ])
//! }
//! ```

/// A resource's immutable native usage. No CPU mapping or mutation is exposed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferUsage {
    Uniform,
    Storage,
}

/// Create a new resource version. Storage creation does not initialize contents.
#[derive(Clone, Copy, Debug)]
pub enum BufferDescriptor<'a> {
    Uniform { bytes: &'a [u8] },
    Storage { size: u64 },
}

impl BufferDescriptor<'_> {
    pub const fn usage(&self) -> BufferUsage {
        match self {
            Self::Uniform { .. } => BufferUsage::Uniform,
            Self::Storage { .. } => BufferUsage::Storage,
        }
    }

    pub const fn size(&self) -> u64 {
        match self {
            Self::Uniform { bytes } => bytes.len() as u64,
            Self::Storage { size } => *size,
        }
    }
}

/// Access derived from the trusted shader. Write access alone is not a promise
/// that every byte of a descriptor range will be initialized by a dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferBindingType {
    Uniform,
    StorageWrite,
}

impl BufferBindingType {
    pub const fn usage(self) -> BufferUsage {
        match self {
            Self::Uniform => BufferUsage::Uniform,
            Self::StorageWrite => BufferUsage::Storage,
        }
    }
}

/// One reflected slot in an immutable Program interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferBindingLayout {
    pub group: u32,
    pub binding: u32,
    pub kind: BufferBindingType,
    pub min_size: u64,
}

/// A logical byte range, excluding native allocation padding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferBindingDesc {
    pub group: u32,
    pub binding: u32,
    pub offset: u64,
    pub size: u64,
}

/// A creation input borrowed only until `create_bindings` returns. The returned
/// immutable binding collection retains the actual Program and buffer owners.
pub struct BufferBinding<'a, B> {
    pub buffer: &'a B,
    pub desc: BufferBindingDesc,
}

/// A reviewed shader asset. Ordinary builds use checked-in artifacts and need
/// no compiler. Arbitrary binary/source and self-declared manifests are not
/// accepted through a safe constructor.
#[derive(Clone, Copy, Debug)]
pub struct ManagedProgram {
    manifest: &'static Manifest,
}

#[derive(Debug)]
struct Manifest {
    entry: &'static str,
    workgroup_size: [u32; 3],
    bindings: &'static [BufferBindingLayout],
}

static UNIFORM_READBACK: Manifest = Manifest {
    entry: "main",
    workgroup_size: [1, 1, 1],
    bindings: &[
        BufferBindingLayout {
            group: 0,
            binding: 0,
            kind: BufferBindingType::Uniform,
            min_size: 16,
        },
        BufferBindingLayout {
            group: 1,
            binding: 0,
            kind: BufferBindingType::StorageWrite,
            min_size: 16,
        },
    ],
};

impl ManagedProgram {
    /// Reviewed compute asset: one invocation copies exactly 16 bytes from the
    /// uniform slot into the storage slot. Extra descriptor bytes are untouched.
    /// Dispatch is currently exercised only by private native acceptance tests.
    pub const fn uniform_readback() -> Self {
        Self {
            manifest: &UNIFORM_READBACK,
        }
    }

    pub const fn entry_point(&self) -> &'static str {
        self.manifest.entry
    }

    pub const fn workgroup_size(&self) -> [u32; 3] {
        self.manifest.workgroup_size
    }

    pub const fn bindings(&self) -> &'static [BufferBindingLayout] {
        self.manifest.bindings
    }

    #[cfg(feature = "vulkan")]
    pub(crate) fn spirv(&self) -> &'static [u8] {
        include_bytes!("../shaders/uniform.comp.spv")
    }

    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn dxil(&self) -> &'static [u8] {
        include_bytes!("../shaders/dxil/uniform.comp.dxil")
    }
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramError {
    BindingCount,
    UnknownSlot,
    DuplicateSlot,
    LayoutMismatch,
    UsageMismatch,
    InvalidAlignment,
    InvalidRange,
    OutOfBounds,
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BindingCount => "binding collection must cover the complete Program layout",
            Self::UnknownSlot => "binding collection contains an unknown Program slot",
            Self::DuplicateSlot => "binding collection contains a duplicate Program slot",
            Self::LayoutMismatch => "buffer slot does not match the managed Program layout",
            Self::UsageMismatch => "buffer usage does not match the managed Program slot",
            Self::InvalidAlignment => "buffer offset does not meet Device binding alignment",
            Self::InvalidRange => "buffer range is outside the shader or Device limits",
            Self::OutOfBounds => "binding range exceeds the logical buffer",
        })
    }
}

/// The trusted layout has unique slots. Equal count plus known, unique inputs
/// proves complete coverage, independently of caller order. Native owners store
/// the successful collection in manifest order; recording need not repeat this.
#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn validate_binding_slots(
    layout: &[BufferBindingLayout],
    descs: &[BufferBindingDesc],
) -> Result<(), ProgramError> {
    if layout.len() != descs.len() {
        return Err(ProgramError::BindingCount);
    }
    for (index, desc) in descs.iter().enumerate() {
        let same_slot = |group, binding| desc.group == group && desc.binding == binding;
        if !layout
            .iter()
            .any(|slot| same_slot(slot.group, slot.binding))
        {
            return Err(ProgramError::UnknownSlot);
        }
        if descs[..index]
            .iter()
            .any(|other| same_slot(other.group, other.binding))
        {
            return Err(ProgramError::DuplicateSlot);
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn validate_buffer_binding(
    layout: BufferBindingLayout,
    usage: BufferUsage,
    buffer_size: u64,
    desc: BufferBindingDesc,
    alignment: u64,
    max_range: u64,
) -> Result<(), ProgramError> {
    if desc.group != layout.group || desc.binding != layout.binding {
        return Err(ProgramError::LayoutMismatch);
    }
    if usage != layout.kind.usage() {
        return Err(ProgramError::UsageMismatch);
    }
    if alignment == 0 || !desc.offset.is_multiple_of(alignment) {
        return Err(ProgramError::InvalidAlignment);
    }
    if desc.size == 0 || desc.size < layout.min_size || desc.size > max_range {
        return Err(ProgramError::InvalidRange);
    }
    if desc
        .offset
        .checked_add(desc.size)
        .is_none_or(|end| end > buffer_size)
    {
        return Err(ProgramError::OutOfBounds);
    }
    Ok(())
}

// Native acceptance uses the same asymmetric bytes. Payloads are not retained
// by ordinary builds, including those with development diagnostics enabled.
#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) const UNIFORM_TEST_DATA: [[u8; 16]; 2] = [
    [
        0x67, 0x45, 0x23, 0x01, 0xef, 0xcd, 0xab, 0x89, 0x98, 0xba, 0xdc, 0xfe, 0x10, 0x32, 0x54,
        0x76,
    ],
    [
        0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0x7f, 0, 0xff, 0x80, 0x62, 0x19, 0xc3, 0xa5,
    ],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(group: u32) -> BufferBindingDesc {
        BufferBindingDesc {
            group,
            binding: 0,
            offset: 256,
            size: 16,
        }
    }

    #[test]
    fn managed_layout_matches_generated_shader_reflection() {
        let program = ManagedProgram::uniform_readback();
        let reflected: serde_json::Value = serde_json::from_str(include_str!(
            "../shaders/dxil/uniform.comp.spirv-reflection.json"
        ))
        .unwrap();
        let entry = &reflected["entryPoints"][0];
        assert_eq!(entry["name"], program.entry_point());
        assert_eq!(entry["mode"], "comp");
        assert_eq!(
            entry["workgroup_size"],
            serde_json::json!(program.workgroup_size())
        );
        let mut seen = Vec::new();
        for (category, kind) in [
            ("ubos", BufferBindingType::Uniform),
            ("ssbos", BufferBindingType::StorageWrite),
        ] {
            for resource in reflected[category].as_array().unwrap() {
                let slot = program
                    .bindings()
                    .iter()
                    .find(|slot| {
                        Some(u64::from(slot.group)) == resource["set"].as_u64()
                            && Some(u64::from(slot.binding)) == resource["binding"].as_u64()
                    })
                    .expect("reflected resource must have a managed slot");
                assert_eq!(slot.kind, kind);
                assert_eq!(Some(slot.min_size), resource["block_size"].as_u64());
                assert!(!seen.contains(&(slot.group, slot.binding)));
                seen.push((slot.group, slot.binding));
                let member = &reflected["types"][resource["type"].as_str().unwrap()]["members"][0];
                assert_eq!(member["type"], "uvec4");
                assert_eq!(member["offset"], 0);
                if kind == BufferBindingType::StorageWrite {
                    assert_eq!(resource["writeonly"], true);
                }
            }
        }
        assert_eq!(seen.len(), program.bindings().len());
    }

    #[test]
    fn complete_binding_collection_accepts_caller_order_and_rejects_malformed_slots() {
        let layout = ManagedProgram::uniform_readback().bindings();
        assert_eq!(validate_binding_slots(layout, &[desc(1), desc(0)]), Ok(()));
        for incomplete in [&[][..], &[desc(0)][..], &[desc(0), desc(1), desc(2)][..]] {
            assert_eq!(
                validate_binding_slots(layout, incomplete),
                Err(ProgramError::BindingCount)
            );
        }
        assert_eq!(
            validate_binding_slots(layout, &[desc(0), desc(0)]),
            Err(ProgramError::DuplicateSlot)
        );
        assert_eq!(
            validate_binding_slots(layout, &[desc(0), desc(2)]),
            Err(ProgramError::UnknownSlot)
        );
    }

    #[test]
    fn typed_buffer_ranges_use_logical_size_and_the_actual_device_limits() {
        for layout in ManagedProgram::uniform_readback().bindings() {
            let check = |usage, size, descriptor| {
                validate_buffer_binding(*layout, usage, size, descriptor, 256, 65_536)
            };
            let usage = layout.kind.usage();
            assert_eq!(check(usage, 272, desc(layout.group)), Ok(()));
            assert_eq!(
                check(usage, 271, desc(layout.group)),
                Err(ProgramError::OutOfBounds)
            );
            let wrong = match usage {
                BufferUsage::Uniform => BufferUsage::Storage,
                BufferUsage::Storage => BufferUsage::Uniform,
            };
            assert_eq!(
                check(wrong, 272, desc(layout.group)),
                Err(ProgramError::UsageMismatch)
            );
            assert_eq!(
                check(
                    usage,
                    512,
                    BufferBindingDesc {
                        group: 9,
                        ..desc(layout.group)
                    }
                ),
                Err(ProgramError::LayoutMismatch)
            );
            assert_eq!(
                check(
                    usage,
                    512,
                    BufferBindingDesc {
                        offset: 1,
                        ..desc(layout.group)
                    }
                ),
                Err(ProgramError::InvalidAlignment)
            );
            for size in [0, 15, 65_537] {
                assert_eq!(
                    check(
                        usage,
                        u64::MAX,
                        BufferBindingDesc {
                            size,
                            ..desc(layout.group)
                        }
                    ),
                    Err(ProgramError::InvalidRange)
                );
            }
            assert_eq!(
                validate_buffer_binding(
                    *layout,
                    usage,
                    u64::MAX,
                    BufferBindingDesc {
                        offset: u64::MAX - 15,
                        ..desc(layout.group)
                    },
                    1,
                    65_536
                ),
                Err(ProgramError::OutOfBounds)
            );
        }
    }
}
