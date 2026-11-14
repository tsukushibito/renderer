//! Managed shader interfaces, independent of a native backend or Graph.
//!
//! The first managed asset reads one `std140` uniform `uvec4`. Its storage
//! output is reserved for private GPU acceptance tests; general storage
//! bindings and Graph execution are subsequent work. Arbitrary source/binary
//! import is deliberately unavailable: reflection alone does not establish
//! shader memory safety, declared accesses, or synchronization.
//!
//! ```no_run
//! use renderer::program::{ManagedProgram, UniformBindingDesc};
//!
//! #[cfg(feature = "vulkan")]
//! fn create(runtime: &renderer::VulkanRuntime)
//!     -> Result<renderer::VulkanUniformBinding, renderer::RuntimeError>
//! {
//!     let program = runtime.create_program(&ManagedProgram::uniform_readback())?;
//!     let buffer = runtime.create_uniform_buffer(&[0; 16])?;
//!     runtime.create_uniform_binding(&program, &buffer, UniformBindingDesc {
//!         group: 0, binding: 0, offset: 0, size: 16,
//!     })
//! }
//!
//! #[cfg(all(feature = "d3d12", windows))]
//! fn create_d3d12(runtime: &renderer::D3D12Runtime)
//!     -> Result<renderer::D3D12UniformBinding, renderer::D3D12RuntimeError>
//! {
//!     let program = runtime.create_program(&ManagedProgram::uniform_readback())?;
//!     let buffer = runtime.create_uniform_buffer(&[0; 16])?;
//!     runtime.create_uniform_binding(&program, &buffer, UniformBindingDesc {
//!         group: 0, binding: 0, offset: 0, size: 16,
//!     })
//! }
//! ```

/// A uniform slot reflected from an immutable, reviewed shader asset.
/// This describes an interface; native bindings retain their actual Program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UniformLayout {
    pub group: u32,
    pub binding: u32,
    pub min_size: u64,
}

/// A byte range and slot to bind, checked once against the Program and Device.
/// The range is logical shader data, independently of native allocation padding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UniformBindingDesc {
    pub group: u32,
    pub binding: u32,
    pub offset: u64,
    pub size: u64,
}

/// A trusted, immutable shader asset with a generated and reviewed interface.
/// Normal Rust builds use checked-in artifacts and need no shader compiler.
/// Creating a native Program from this value never mutates an existing Program.
#[derive(Clone, Copy, Debug)]
pub struct ManagedProgram {
    manifest: &'static Manifest,
}

#[derive(Debug)]
struct Manifest {
    entry: &'static str,
    workgroup_size: [u32; 3],
    uniform: UniformLayout,
}

static UNIFORM_READBACK: Manifest = Manifest {
    entry: "main",
    workgroup_size: [1, 1, 1],
    uniform: UniformLayout {
        group: 0,
        binding: 0,
        min_size: 16,
    },
};

impl ManagedProgram {
    /// Reviewed acceptance asset: copy one uniform `uvec4` into a private
    /// storage output at group 1, binding 0. It is not a general dispatch API.
    /// Its single invocation reads/writes exactly 16 bytes without races.
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

    pub const fn uniform_layout(&self) -> UniformLayout {
        self.manifest.uniform
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
    LayoutMismatch,
    InvalidAlignment,
    InvalidRange,
    OutOfBounds,
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LayoutMismatch => "uniform slot does not match the managed Program layout",
            Self::InvalidAlignment => "uniform offset does not meet Device alignment",
            Self::InvalidRange => "uniform range is outside the shader or Device limits",
            Self::OutOfBounds => "uniform range exceeds the immutable buffer",
        })
    }
}

/// Native creation calls supply real, cached Device limits. On success the
/// private immutable binding retains this range and both native owners, so
/// recording does not recheck its contents. No mapping or mutation escapes.
#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn validate_uniform_binding(
    layout: UniformLayout,
    buffer_size: u64,
    desc: UniformBindingDesc,
    alignment: u64,
    max_range: u64,
) -> Result<(), ProgramError> {
    if desc.group != layout.group || desc.binding != layout.binding {
        return Err(ProgramError::LayoutMismatch);
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

// Both native acceptance fixtures consume these exact asymmetric bytes. Keep
// test payloads out of ordinary builds, including development diagnostics.
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

    fn desc() -> UniformBindingDesc {
        UniformBindingDesc {
            group: 0,
            binding: 0,
            offset: 256,
            size: 16,
        }
    }

    #[test]
    fn reflected_uniform_contract_checks_logical_ranges_independently_of_padding() {
        let program = ManagedProgram::uniform_readback();
        assert_eq!(program.entry_point(), "main");
        assert_eq!(program.workgroup_size(), [1, 1, 1]);
        assert_eq!(program.uniform_layout().min_size, 16);
        let check = |size, descriptor| {
            validate_uniform_binding(program.uniform_layout(), size, descriptor, 256, 65_536)
        };
        assert_eq!(check(272, desc()), Ok(()));
        assert_eq!(check(271, desc()), Err(ProgramError::OutOfBounds));
        assert_eq!(
            check(512, UniformBindingDesc { group: 1, ..desc() }),
            Err(ProgramError::LayoutMismatch)
        );
        assert_eq!(
            check(
                512,
                UniformBindingDesc {
                    binding: 1,
                    ..desc()
                }
            ),
            Err(ProgramError::LayoutMismatch)
        );
        assert_eq!(
            check(
                512,
                UniformBindingDesc {
                    offset: 1,
                    ..desc()
                }
            ),
            Err(ProgramError::InvalidAlignment)
        );
        for size in [0, 15, 65_537] {
            assert_eq!(
                check(u64::MAX, UniformBindingDesc { size, ..desc() }),
                Err(ProgramError::InvalidRange)
            );
        }
    }

    #[test]
    fn overflowing_uniform_range_never_wraps_into_the_buffer() {
        let layout = ManagedProgram::uniform_readback().uniform_layout();
        assert_eq!(
            validate_uniform_binding(
                layout,
                u64::MAX,
                UniformBindingDesc {
                    offset: u64::MAX - 15,
                    ..desc()
                },
                1,
                65_536,
            ),
            Err(ProgramError::OutOfBounds)
        );
    }
}
