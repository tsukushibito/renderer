//! Pass declarations, access purposes and immutable execution operands.

use std::{ops::Range, sync::Arc};

use super::{BufferHandle, GraphError, GraphErrorKind, ImageHandle, ImageView};

/// How an attachment begins. Only a clear operation carries a clear value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadOp<T> {
    Load,
    Clear(T),
    Discard,
}

/// A single image subresource and the operations applied to its contents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attachment<T> {
    pub view: ImageView,
    pub load: LoadOp<T>,
    pub store: StoreOp,
}

impl<T> Attachment<T> {
    pub(super) fn ops(&self) -> AttachmentOps {
        AttachmentOps {
            load: match self.load {
                LoadOp::Load => LoadAction::Load,
                LoadOp::Clear(_) => LoadAction::Clear,
                LoadOp::Discard => LoadAction::Discard,
            },
            store: self.store,
        }
    }
}

/// Borrowed attachment inputs. A pass owns copies after construction. The
/// Program determines its color output count/formats; depth is optional except
/// for a depth-only Program, which requires it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterAttachments<'a> {
    pub colors: &'a [Attachment<[f32; 4]>],
    /// Optional single-sample destination for each color slot. An empty slice
    /// leaves all colors unresolved.
    pub color_resolves: &'a [Option<ImageView>],
    /// Depth clear values must be zero or normal finite values in `[0, 1]`.
    /// Subnormal values are unsupported across the native clear paths.
    pub depth: Option<Attachment<f32>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AttachmentOperand {
    pub view: ImageView,
    pub resolve: Option<ImageView>,
    pub ops: AttachmentOps,
    /// Color RGBA or depth in the first component; meaningful only for Clear.
    #[cfg_attr(
        not(any(test, feature = "vulkan", all(feature = "d3d12", windows))),
        allow(dead_code, reason = "clear payload is consumed by native lowering")
    )]
    pub clear: [f32; 4],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoadAction {
    Load,
    Clear,
    Discard,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOp {
    Store,
    Discard,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AttachmentOps {
    pub load: LoadAction,
    pub store: StoreOp,
}

/// Trusted built-in shader transformations; no arbitrary SPIR-V is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputeTransform {
    Invert,
    SwapRedBlue,
}

/// Immutable per-draw values for the built-in mesh pipeline. Matrices are
/// column-major and multiply a column vector: `clip = transform * position`.
/// This parameter block is independent of asset handles and frame ownership.
/// Construction establishes finite components; accessors cannot invalidate them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterParameters {
    transform: [[f32; 4]; 4],
    tint: [f32; 4],
}

impl RasterParameters {
    /// Establish finite, immutable draw constants once, before Graph capture.
    pub fn new(transform: [[f32; 4]; 4], tint: [f32; 4]) -> Result<Self, GraphError> {
        if transform
            .iter()
            .flatten()
            .chain(&tint)
            .any(|value| !value.is_finite())
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "non-finite raster transform or tint",
            ));
        }
        Ok(Self { transform, tint })
    }

    pub fn transform(&self) -> &[[f32; 4]; 4] {
        &self.transform
    }

    pub fn tint(&self) -> &[f32; 4] {
        &self.tint
    }
}

impl Default for RasterParameters {
    fn default() -> Self {
        Self {
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            tint: [1.0; 4],
        }
    }
}

/// Owned per-draw inline words. Construction copies at most 128 bytes without
/// allocating; Graph captures this immutable value into its prepared draw slot.
/// Shader-specific values and their interpretation belong to the trusted Program.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RasterConstants {
    words: [u32; 32],
    len: u8,
}

impl RasterConstants {
    pub fn new(words: &[u32]) -> Result<Self, GraphError> {
        if words.len() > 32 {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster inline constants exceed 128 bytes",
            ));
        }
        let mut result = Self::default();
        result.words[..words.len()].copy_from_slice(words);
        result.len = words.len() as u8;
        Ok(result)
    }
    pub fn words(&self) -> &[u32] {
        &self.words[..usize::from(self.len)]
    }
}
impl From<RasterParameters> for RasterConstants {
    fn from(parameters: RasterParameters) -> Self {
        let mut words = [0; 32];
        for (word, value) in words.iter_mut().zip(
            parameters
                .transform
                .iter()
                .flatten()
                .chain(&parameters.tint),
        ) {
            *word = value.to_bits();
        }
        Self { words, len: 20 }
    }
}

/// One indexed triangle draw. A validated binding set follows the selected
/// Raster Program; `None` is accepted only for Programs with no binding slots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterDraw {
    pub bindings: Option<super::RasterBindings>,
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    /// Offset in u32 indices from the start of `indices`.
    pub first_index: u32,
    pub index_count: u32,
    /// Nonzero count of instances. The shader's first instance ID is zero.
    pub instance_count: u32,
    pub parameters: RasterConstants,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassKind {
    Copy,
    Compute,
    Raster,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessKind {
    Read,
    Write,
    ReadWrite,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessPurpose {
    CopySource,
    CopyDestination,
    Uniform,
    StorageRead,
    StorageWrite,
    StorageReadWrite,
    Sampled,
    Vertex,
    Index,
    /// Device reads of a renderer-encoded indirect argument buffer.
    Indirect,
    ColorAttachment,
    ColorResolve,
    DepthAttachment,
}
impl AccessPurpose {
    pub(crate) const fn default_stages(self) -> crate::program::ShaderStages {
        use crate::program::ShaderStages;
        match self {
            Self::Uniform | Self::StorageRead | Self::StorageWrite | Self::StorageReadWrite => {
                ShaderStages::COMPUTE
            }
            Self::Sampled => ShaderStages::FRAGMENT,
            _ => ShaderStages::EMPTY,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageAspect {
    Color,
    Depth,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccessRange {
    Buffer(Range<u64>),
    Image {
        mip: u32,
        layer: u32,
        aspect: ImageAspect,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceHandle {
    Buffer(BufferHandle),
    Image(ImageHandle),
}
impl From<BufferHandle> for ResourceHandle {
    fn from(value: BufferHandle) -> Self {
        Self::Buffer(value)
    }
}
impl From<ImageHandle> for ResourceHandle {
    fn from(value: ImageHandle) -> Self {
        Self::Image(value)
    }
}

/// A declaration-only access. Native execution rejects declaration-only passes;
/// supported operation builders derive their accesses from encoded operands.
#[derive(Clone, Debug)]
pub struct Access {
    pub resource: ResourceHandle,
    pub range: AccessRange,
    pub kind: AccessKind,
    pub purpose: AccessPurpose,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "access purposes are lowered by the Vulkan runtime"
    )
)]
pub(crate) struct ResourceAccess {
    pub stages: crate::program::ShaderStages,
    pub resource: usize,
    pub range: Range<u64>,
    pub read: bool,
    pub write: bool,
    /// A write establishes every unit in its declared range.
    pub initializes: bool,
    /// The range is undefined after this operation (attachment store discard).
    pub discard_after: bool,
    pub purpose: AccessPurpose,
}

/// Owned indexed draw operands, including its canonical material descriptors.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(dead_code, reason = "native batch recording operands")
)]
pub(crate) struct RasterDrawOperands {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "descriptor-bound native Raster is subsequent work"
        )
    )]
    pub bindings: Range<usize>,
    pub vertices: usize,
    pub indices: usize,
    pub first_index: u32,
    pub index_count: u32,
    pub instance_count: u32,
    pub parameters: RasterConstants,
}
/// Internal provenance for trusted immutable streams. Public byte uploads
/// cannot establish this metadata.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "vulkan"), allow(dead_code))]
pub(crate) enum MeshUpload {
    Vertices,
    Indices { byte_len: u64, maximum: u32 },
}

/// Consecutive draws submitted with one pipeline and resource layout.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(any(test, feature = "vulkan", all(feature = "d3d12", windows))),
    allow(
        dead_code,
        reason = "ordered Program groups are consumed by native recording"
    )
)]
pub(crate) struct RasterBatchOperands {
    pub program: crate::program::ManagedRasterProgram,
    pub draws: Range<usize>,
    /// When present, the sole draw in `draws` supplies bindings and proves
    /// the index prefix needed by the real commands in the argument buffer.
    pub indirect: Option<IndirectDrawOperands>,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(any(test, feature = "vulkan", all(feature = "d3d12", windows))),
    allow(
        dead_code,
        reason = "indirect command operands are consumed by native recording"
    )
)]
pub(crate) struct IndirectDrawOperands {
    pub buffer: usize,
    pub command_count: u32,
}

#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "native-free operation operands are consumed by the optional Vulkan runtime"
    )
)]
pub(crate) enum Operation {
    UploadBuffer {
        dst: usize,
        offset: u64,
        data: UploadSource,
        mesh: Option<MeshUpload>,
    },
    CopyBuffer {
        src: usize,
        dst: usize,
        src_offset: u64,
        dst_offset: u64,
        size: u64,
    },
    UploadImage {
        dst: usize,
        data: UploadSource,
    },
    CopyImage {
        src: usize,
        dst: usize,
    },
    ReadbackBuffer {
        src: usize,
        offset: u64,
        size: u64,
    },
    ReadbackImage {
        src: usize,
    },
    Compute {
        src: usize,
        dst: usize,
        transform: ComputeTransform,
    },
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "native managed dispatch is subsequent work")
    )]
    Dispatch {
        program: crate::program::ManagedProgram,
        bindings: Range<usize>,
        workgroups: [u32; 3],
    },
    Raster {
        batches: Range<usize>,
        /// All direct draws in the scope, including every Program batch.
        draws: Range<usize>,
        colors: Range<usize>,
        depth: Option<usize>,
    },
    AttachmentPass {
        colors: Range<usize>,
        depth: Option<usize>,
    },
    Composite {
        src: usize,
        dst: usize,
    },
    Declared(PassKind),
}
#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "encoded operations are consumed by the optional Vulkan runtime"
    )
)]
pub(crate) struct Pass {
    pub name: Range<usize>,
    pub operation: Operation,
    pub accesses: Range<usize>,
    pub side_effect: bool,
    pub dependencies: DependencyList,
}

/// Indices into the prepared dependency arena, in original insertion order.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DependencyList {
    pub head: Option<usize>,
    pub tail: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Dependency {
    pub target: usize,
    pub next: Option<usize>,
}

impl DependencyList {
    pub fn iter(self, arena: &[Dependency]) -> impl Iterator<Item = usize> {
        let mut next = self.head;
        std::iter::from_fn(move || {
            let node = &arena[next?];
            next = node.next;
            Some(node.target)
        })
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(
    not(any(test, feature = "vulkan", all(feature = "d3d12", windows))),
    allow(
        dead_code,
        reason = "upload bytes are read by optional native lowering"
    )
)]
pub(crate) enum UploadSource {
    Frame(Range<usize>),
    Shared {
        bytes: Arc<[u8]>,
        source: Range<usize>,
    },
}
