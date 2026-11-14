//! Managed shader interfaces, independent of a native backend or Graph.
//!
//! Programs describe their complete buffer, image View and sampler interface. Uniform bytes
//! are copied once and stay immutable; storage is created without initialized
//! contents and may be written by the GPU. Immutable bindings retain the actual
//! Program and resource versions, not a promise of immutable storage contents.
//! General dispatch/Graph integration and arbitrary shader import are subsequent
//! work. Reflection alone cannot establish shader memory safety or synchronization.
//!
//! Creation retains uninitialized images; this example does not execute a dispatch.
//!
//! ```no_run
//! use renderer::program::{Binding, BindingResource, BufferDescriptor, ManagedProgram,
//!     ImageDescriptor, ImageFormat, ImageViewDescriptor, SamplerDescriptor};
//!
//! #[cfg(feature = "vulkan")]
//! fn create(runtime: &renderer::VulkanRuntime)
//!     -> Result<renderer::VulkanBindings, renderer::RuntimeError>
//! {
//!     let program = runtime.create_program(&ManagedProgram::sampled_readback())?;
//!     let image = runtime.create_managed_image(ImageDescriptor {
//!         width: 2, height: 2, format: ImageFormat::Rgba8Unorm })?;
//!     let view = runtime.create_image_view(&image, ImageViewDescriptor::default())?;
//!     let sampler = runtime.create_sampler(SamplerDescriptor::NearestClamp)?;
//!     let storage = runtime.create_managed_buffer(BufferDescriptor::Storage { size: 272 })?;
//!     runtime.create_bindings(&program, &[
//!         Binding { group: 0, binding: 0, resource: BindingResource::ImageView(&view) },
//!         Binding { group: 0, binding: 1, resource: BindingResource::Sampler(&sampler) },
//!         Binding { group: 1, binding: 0,
//!             resource: BindingResource::Buffer { buffer: &storage, offset: 256, size: 16 } },
//!     ])
//! }
//!
//! #[cfg(all(feature = "d3d12", windows))]
//! fn create_d3d12(runtime: &renderer::D3D12Runtime)
//!     -> Result<renderer::D3D12Bindings, renderer::D3D12RuntimeError>
//! {
//!     let program = runtime.create_program(&ManagedProgram::sampled_readback())?;
//!     let image = runtime.create_managed_image(ImageDescriptor {
//!         width: 2, height: 2, format: ImageFormat::Rgba8Unorm })?;
//!     let view = runtime.create_image_view(&image, ImageViewDescriptor::default())?;
//!     let sampler = runtime.create_sampler(SamplerDescriptor::NearestClamp)?;
//!     let storage = runtime.create_managed_buffer(BufferDescriptor::Storage { size: 272 })?;
//!     runtime.create_bindings(&program, &[
//!         Binding { group: 0, binding: 0, resource: BindingResource::ImageView(&view) },
//!         Binding { group: 0, binding: 1, resource: BindingResource::Sampler(&sampler) },
//!         Binding { group: 1, binding: 0,
//!             resource: BindingResource::Buffer { buffer: &storage, offset: 256, size: 16 } },
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
pub enum BindingType {
    Uniform,
    StorageWrite,
    SampledImage,
    Sampler,
}

impl BindingType {
    pub const fn buffer_usage(self) -> Option<BufferUsage> {
        match self {
            Self::Uniform => Some(BufferUsage::Uniform),
            Self::StorageWrite => Some(BufferUsage::Storage),
            Self::SampledImage | Self::Sampler => None,
        }
    }
}

/// One reflected slot in an immutable Program interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingLayout {
    pub group: u32,
    pub binding: u32,
    pub kind: BindingType,
    /// Required buffer bytes; zero for image and sampler slots.
    pub min_size: u64,
}

/// A logical byte range, excluding native allocation padding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferRange {
    pub offset: u64,
    pub size: u64,
}

/// A creation input borrowed only until `create_bindings` returns. The returned
/// immutable binding collection retains the actual Program and resource owners.
pub struct Binding<'a, B, V, S> {
    pub group: u32,
    pub binding: u32,
    pub resource: BindingResource<'a, B, V, S>,
}

/// A resource reference borrowed only during binding creation.
pub enum BindingResource<'a, B, V, S> {
    Buffer {
        buffer: &'a B,
        offset: u64,
        size: u64,
    },
    ImageView(&'a V),
    Sampler(&'a S),
}

/// Managed image formats. Sampling returns ordinary FP32 components;
/// RGBA16F image storage does not require shader FP16 arithmetic support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFormat {
    Rgba8Unorm,
    Rgba16Float,
    D32Float,
}

/// An uninitialized sampled 2D image. Mip, layer and sample counts are one.
/// Color images support sampling and transfer destination use; D32 images
/// support sampling and depth attachment use.
/// Creation does not upload pixels or submit GPU work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageDescriptor {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
}

/// The operation supported by an image View.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageViewUsage {
    Sampled,
    /// A depth attachment View, supported only by [`ImageFormat::D32Float`].
    DepthAttachment,
}

/// The usage and subresources interpreted by a 2D View. Only the complete
/// single mip/layer is supported; unsupported ranges fail at View creation.
/// The color or depth aspect is determined by the image format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageViewDescriptor {
    pub usage: ImageViewUsage,
    pub base_mip_level: u32,
    pub mip_level_count: u32,
    pub base_array_layer: u32,
    pub array_layer_count: u32,
}

impl Default for ImageViewDescriptor {
    fn default() -> Self {
        Self {
            usage: ImageViewUsage::Sampled,
            base_mip_level: 0,
            mip_level_count: 1,
            base_array_layer: 0,
            array_layer_count: 1,
        }
    }
}

/// Normalized coordinates, nearest min/mag/mip filtering, clamp-to-edge on all
/// axes, LOD zero, no comparison or anisotropy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplerDescriptor {
    NearestClamp,
}

/// A reviewed shader asset. Ordinary builds use checked-in artifacts and need
/// no compiler. Arbitrary binary/source and self-declared manifests are not
/// accepted through a safe constructor.
#[derive(Clone, Copy)]
pub struct ManagedProgram {
    manifest: &'static Manifest,
}

struct Manifest {
    entry: &'static str,
    workgroup_size: [u32; 3],
    bindings: &'static [BindingLayout],
    #[cfg(feature = "vulkan")]
    spirv: &'static [u8],
    #[cfg(all(feature = "d3d12", windows))]
    dxil: &'static [u8],
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

static UNIFORM_READBACK: Manifest = Manifest {
    entry: "main",
    workgroup_size: [1, 1, 1],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            min_size: 16,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            min_size: 16,
        },
    ],
    #[cfg(feature = "vulkan")]
    spirv: include_bytes!("../shaders/uniform.comp.spv"),
    #[cfg(all(feature = "d3d12", windows))]
    dxil: include_bytes!("../shaders/dxil/uniform.comp.dxil"),
};

static SAMPLED_READBACK: Manifest = Manifest {
    entry: "main",
    workgroup_size: [1, 1, 1],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            min_size: 0,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            min_size: 16,
        },
    ],
    #[cfg(feature = "vulkan")]
    spirv: include_bytes!("../shaders/sampled.comp.spv"),
    #[cfg(all(feature = "d3d12", windows))]
    dxil: include_bytes!("../shaders/dxil/sampled.comp.dxil"),
};

static SAMPLED_FLOAT_READBACK: Manifest = Manifest {
    entry: "main",
    workgroup_size: [1, 1, 1],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            min_size: 0,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::StorageWrite,
            min_size: 64,
        },
    ],
    #[cfg(feature = "vulkan")]
    spirv: include_bytes!("../shaders/sampled_float.comp.spv"),
    #[cfg(all(feature = "d3d12", windows))]
    dxil: include_bytes!("../shaders/dxil/sampled_float.comp.dxil"),
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

    /// Reviewed compute asset: four nearest/clamp samples, packed as RGBA8
    /// into exactly 16 storage bytes. Private acceptance dispatches one invocation.
    pub const fn sampled_readback() -> Self {
        Self {
            manifest: &SAMPLED_READBACK,
        }
    }

    /// Reviewed compute asset: four nearest/clamp samples preserve all RGBA
    /// FP32 component bits in exactly 64 storage bytes. RGBA16F sampling needs
    /// no shader FP16 feature. Private acceptance dispatches one invocation.
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

    #[cfg(feature = "vulkan")]
    pub(crate) fn spirv(&self) -> &'static [u8] {
        self.manifest.spirv
    }

    #[cfg(all(feature = "d3d12", windows))]
    pub(crate) fn dxil(&self) -> &'static [u8] {
        self.manifest.dxil
    }
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramError {
    BindingCount,
    MissingSlot,
    DuplicateSlot,
    UsageMismatch,
    InvalidAlignment,
    InvalidRange,
    OutOfBounds,
    InvalidExtent,
    InvalidViewRange,
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BindingCount => "binding collection must cover the complete Program layout",
            Self::MissingSlot => "binding collection does not provide a required Program slot",
            Self::DuplicateSlot => "binding collection contains a duplicate Program slot",
            Self::UsageMismatch => "resource kind or usage does not match the requested operation",
            Self::InvalidAlignment => "buffer offset does not meet Device binding alignment",
            Self::InvalidRange => "buffer range is outside the shader or Device limits",
            Self::OutOfBounds => "binding range exceeds the logical buffer",
            Self::InvalidExtent => "image extent must be nonzero and within Device limits",
            Self::InvalidViewRange => "image View must cover mip zero and layer zero exactly once",
        })
    }
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
type ResolvedBinding<'a, 'b, B, V, S> = (BindingLayout, &'a Binding<'b, B, V, S>);

/// Resolve directly from caller input in manifest order without a validation
/// scratch allocation. The trusted layout has unique slots: equal count and one
/// input per slot prove complete coverage and reject unknown/duplicate inputs.
/// Callers must consume every result successfully before publishing native
/// bindings. Each selected pair already has matching group/binding values.
#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn resolve_bindings<'a, 'b, B, V, S>(
    layout: &'a [BindingLayout],
    input: &'a [Binding<'b, B, V, S>],
) -> Result<
    impl Iterator<Item = Result<ResolvedBinding<'a, 'b, B, V, S>, ProgramError>>,
    ProgramError,
> {
    if layout.len() != input.len() {
        return Err(ProgramError::BindingCount);
    }
    Ok(layout.iter().copied().map(move |slot| {
        let mut matching = input
            .iter()
            .filter(|binding| binding.group == slot.group && binding.binding == slot.binding);
        let binding = matching.next().ok_or(ProgramError::MissingSlot)?;
        if matching.next().is_some() {
            return Err(ProgramError::DuplicateSlot);
        }
        Ok((slot, binding))
    }))
}

/// The resolver selected this immutable descriptor for `layout`. Check its
/// external resource usage/range against the real Device limits once; slot
/// equality is already established and is not revalidated here.
#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn validate_buffer_binding(
    layout: BindingLayout,
    usage: BufferUsage,
    buffer_size: u64,
    desc: BufferRange,
    alignment: u64,
    max_range: u64,
) -> Result<(), ProgramError> {
    if Some(usage) != layout.kind.buffer_usage() {
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

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn validate_image_descriptor(
    desc: ImageDescriptor,
    max_dimension: u32,
) -> Result<(), ProgramError> {
    if desc.width == 0
        || desc.height == 0
        || desc.width > max_dimension
        || desc.height > max_dimension
    {
        return Err(ProgramError::InvalidExtent);
    }
    Ok(())
}

#[cfg(any(test, feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn validate_image_view_descriptor(
    format: ImageFormat,
    desc: ImageViewDescriptor,
) -> Result<(), ProgramError> {
    if desc.base_mip_level != 0
        || desc.mip_level_count != 1
        || desc.base_array_layer != 0
        || desc.array_layer_count != 1
    {
        return Err(ProgramError::InvalidViewRange);
    }
    if desc.usage == ImageViewUsage::DepthAttachment && format != ImageFormat::D32Float {
        return Err(ProgramError::UsageMismatch);
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

#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) const SAMPLE_TEST_DATA: [[u8; 16]; 2] = [
    [
        5, 17, 29, 43, 61, 79, 97, 113, 127, 149, 173, 199, 211, 223, 239, 251,
    ],
    [
        251, 239, 223, 211, 199, 173, 149, 127, 113, 97, 79, 61, 43, 29, 17, 5,
    ],
];

// Exact finite half encodings for the row-major RGBA components
// [-2, -1, -0.5, -0.25, 0, 0.25, 0.5, 1, 1.5, 2, 3, 4, 8, 16, 32, 64].
// The second image reverses component order. Avoid denormals, NaNs and negative
// zero so this fixture checks storage conversion and sampling without relying
// on optional shader floating-point controls or a runtime half converter.
#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) const SAMPLE_FLOAT_TEST_DATA: [[u8; 32]; 2] = [
    [
        0x00, 0xc0, 0x00, 0xbc, 0x00, 0xb8, 0x00, 0xb4, 0x00, 0x00, 0x00, 0x34, 0x00, 0x38, 0x00,
        0x3c, 0x00, 0x3e, 0x00, 0x40, 0x00, 0x42, 0x00, 0x44, 0x00, 0x48, 0x00, 0x4c, 0x00, 0x50,
        0x00, 0x54,
    ],
    [
        0x00, 0x54, 0x00, 0x50, 0x00, 0x4c, 0x00, 0x48, 0x00, 0x44, 0x00, 0x42, 0x00, 0x40, 0x00,
        0x3e, 0x00, 0x3c, 0x00, 0x38, 0x00, 0x34, 0x00, 0x00, 0x00, 0xb4, 0x00, 0xb8, 0x00, 0xbc,
        0x00, 0xc0,
    ],
];

/// Expected FP32 bit patterns, independently specified from the half payload.
#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) const SAMPLE_FLOAT_EXPECTED: [[u32; 16]; 2] = [
    [
        0xc000_0000,
        0xbf80_0000,
        0xbf00_0000,
        0xbe80_0000,
        0x0000_0000,
        0x3e80_0000,
        0x3f00_0000,
        0x3f80_0000,
        0x3fc0_0000,
        0x4000_0000,
        0x4040_0000,
        0x4080_0000,
        0x4100_0000,
        0x4180_0000,
        0x4200_0000,
        0x4280_0000,
    ],
    [
        0x4280_0000,
        0x4200_0000,
        0x4180_0000,
        0x4100_0000,
        0x4080_0000,
        0x4040_0000,
        0x4000_0000,
        0x3fc0_0000,
        0x3f80_0000,
        0x3f00_0000,
        0x3e80_0000,
        0x0000_0000,
        0xbe80_0000,
        0xbf00_0000,
        0xbf80_0000,
        0xc000_0000,
    ],
];

// Depth acceptance clears each complete image to one exact finite value, then
// compares the R component of all four samples from sampled_float_readback.
#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) const DEPTH_TEST_VALUES: [f32; 2] = [0.25, 0.75];

#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
pub(crate) const DEPTH_TEST_EXPECTED: [u32; 2] = [0x3e80_0000, 0x3f40_0000];

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn complete_binding_collection_accepts_caller_order_and_rejects_malformed_slots() {
        let layout = ManagedProgram::sampled_readback().bindings();
        let image = 11;
        let sampler = 22;
        let buffer = 33;
        let input = [
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: &buffer,
                    offset: 256,
                    size: 16,
                },
            },
            Binding {
                group: 0,
                binding: 1,
                resource: BindingResource::Sampler(&sampler),
            },
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::ImageView(&image),
            },
        ];
        let resolve = |input: &[Binding<'_, i32, i32, i32>]| {
            resolve_bindings(layout, input).and_then(|resolved| {
                resolved
                    .map(|entry| {
                        entry.map(|(slot, binding)| {
                            let value = match binding.resource {
                                BindingResource::Buffer { buffer, .. } => *buffer,
                                BindingResource::ImageView(view) => *view,
                                BindingResource::Sampler(sampler) => *sampler,
                            };
                            (slot.group, slot.binding, value)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
        };
        assert_eq!(
            resolve(&input),
            Ok(vec![(0, 0, 11), (0, 1, 22), (1, 0, 33)])
        );
        assert_eq!(resolve(&input[..2]), Err(ProgramError::BindingCount));
        let malformed = |slots: [(u32, u32); 3]| {
            slots.map(|(group, binding)| Binding {
                group,
                binding,
                resource: BindingResource::<i32, i32, i32>::ImageView(&image),
            })
        };
        assert_eq!(
            resolve(&malformed([(0, 0), (0, 0), (1, 0)])),
            Err(ProgramError::DuplicateSlot)
        );
        // A late missing/unknown slot must fail even after earlier slots resolved.
        assert_eq!(
            resolve(&malformed([(0, 0), (0, 1), (2, 0)])),
            Err(ProgramError::MissingSlot)
        );
        assert_eq!(
            resolve(&malformed([(0, 0), (0, 2), (1, 0)])),
            Err(ProgramError::MissingSlot)
        );
    }

    #[test]
    fn typed_buffer_ranges_use_logical_size_and_the_actual_device_limits() {
        let range = BufferRange {
            offset: 256,
            size: 16,
        };
        for layout in ManagedProgram::uniform_readback().bindings() {
            let check = |usage, size, descriptor| {
                validate_buffer_binding(*layout, usage, size, descriptor, 256, 65_536)
            };
            let usage = layout.kind.buffer_usage().unwrap();
            assert_eq!(check(usage, 272, range), Ok(()));
            assert_eq!(check(usage, 271, range), Err(ProgramError::OutOfBounds));
            let wrong = match usage {
                BufferUsage::Uniform => BufferUsage::Storage,
                BufferUsage::Storage => BufferUsage::Uniform,
            };
            assert_eq!(check(wrong, 272, range), Err(ProgramError::UsageMismatch));
            assert_eq!(
                check(usage, 512, BufferRange { offset: 1, ..range }),
                Err(ProgramError::InvalidAlignment)
            );
            for size in [0, 15, 65_537] {
                assert_eq!(
                    check(usage, u64::MAX, BufferRange { size, ..range }),
                    Err(ProgramError::InvalidRange)
                );
            }
            assert_eq!(
                validate_buffer_binding(
                    *layout,
                    usage,
                    u64::MAX,
                    BufferRange {
                        offset: u64::MAX - 15,
                        ..range
                    },
                    1,
                    65_536
                ),
                Err(ProgramError::OutOfBounds)
            );
        }
        assert_eq!(
            validate_buffer_binding(
                ManagedProgram::sampled_readback().bindings()[0],
                BufferUsage::Uniform,
                272,
                range,
                256,
                65_536
            ),
            Err(ProgramError::UsageMismatch)
        );
    }

    #[test]
    fn image_and_view_creation_enforce_the_supported_extent_and_subresources() {
        let desc = ImageDescriptor {
            width: 2,
            height: 2,
            format: ImageFormat::Rgba8Unorm,
        };
        assert_eq!(validate_image_descriptor(desc, 2), Ok(()));
        for (width, height) in [(0, 2), (2, 0), (3, 2), (2, 3)] {
            assert_eq!(
                validate_image_descriptor(
                    ImageDescriptor {
                        width,
                        height,
                        ..desc
                    },
                    2
                ),
                Err(ProgramError::InvalidExtent)
            );
        }
        let sampled = ImageViewDescriptor::default();
        let depth = ImageViewDescriptor {
            usage: ImageViewUsage::DepthAttachment,
            ..sampled
        };
        for format in [
            ImageFormat::Rgba8Unorm,
            ImageFormat::Rgba16Float,
            ImageFormat::D32Float,
        ] {
            assert_eq!(validate_image_view_descriptor(format, sampled), Ok(()));
            assert_eq!(
                validate_image_view_descriptor(format, depth),
                if format == ImageFormat::D32Float {
                    Ok(())
                } else {
                    Err(ProgramError::UsageMismatch)
                }
            );
        }
        let view = depth;
        for invalid in [
            ImageViewDescriptor {
                base_mip_level: 1,
                ..view
            },
            ImageViewDescriptor {
                mip_level_count: 0,
                ..view
            },
            ImageViewDescriptor {
                mip_level_count: 2,
                ..view
            },
            ImageViewDescriptor {
                base_array_layer: 1,
                ..view
            },
            ImageViewDescriptor {
                array_layer_count: 0,
                ..view
            },
            ImageViewDescriptor {
                array_layer_count: 2,
                ..view
            },
        ] {
            assert_eq!(
                validate_image_view_descriptor(ImageFormat::D32Float, invalid),
                Err(ProgramError::InvalidViewRange)
            );
        }
    }
}
