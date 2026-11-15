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

/// Borrowed attachment inputs. An attachment pass owns copies after construction;
/// the built-in mesh pipeline currently requires one color and one depth target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterAttachments<'a> {
    pub colors: &'a [Attachment<[f32; 4]>],
    pub depth: Option<Attachment<f32>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AttachmentOperand {
    pub view: ImageView,
    pub ops: AttachmentOps,
    /// Color RGBA or depth in the first component; meaningful only for Clear.
    pub clear: [f32; 4],
}

/// The already-established Clear/Store shape used by existing native lowering.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "fixed clear operands are consumed by Vulkan lowering"
    )
)]
pub(crate) struct RasterAttachmentOperands {
    pub color: usize,
    pub depth: usize,
    pub clear_color: [f32; 4],
    pub clear_depth: f32,
    pub color_ops: AttachmentOps,
    pub depth_ops: AttachmentOps,
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterParameters {
    pub transform: [[f32; 4]; 4],
    pub tint: [f32; 4],
}

/// Finite, immutable per-draw values. The private field carries the creation
/// guarantee from a captured engine frame into the internal batch builder.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ValidatedRasterParameters(RasterParameters);

impl ValidatedRasterParameters {
    pub(crate) fn get(self) -> RasterParameters {
        self.0
    }

    pub(crate) fn new(parameters: RasterParameters) -> Result<Self, GraphError> {
        if parameters
            .transform
            .iter()
            .flatten()
            .chain(&parameters.tint)
            .any(|value| !value.is_finite())
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "non-finite raster transform or tint",
            ));
        }
        Ok(Self(parameters))
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

/// One indexed triangle draw. Its borrowed descriptors follow the selected
/// Raster Program layout and are owned by the plan after construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterDraw<'a> {
    pub bindings: &'a [super::Binding],
    pub vertices: BufferHandle,
    pub indices: BufferHandle,
    pub index_count: u32,
    pub parameters: RasterParameters,
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
    ColorAttachment,
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

#[derive(Clone, Debug)]
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
    pub index_count: u32,
    pub parameters: RasterParameters,
}
/// Internal provenance for trusted immutable streams. Public byte uploads
/// cannot establish this metadata.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "vulkan"), allow(dead_code))]
pub(crate) enum MeshUpload {
    Vertices,
    Indices { byte_len: u64, maximum: u32 },
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
        program: crate::program::ManagedRasterProgram,
        draws: Range<usize>,
        #[cfg_attr(
            not(test),
            allow(dead_code, reason = "owned general graphics attachments")
        )]
        colors: Range<usize>,
        #[cfg_attr(
            not(test),
            allow(dead_code, reason = "owned general graphics attachments")
        )]
        depth: Option<usize>,
        targets: Option<RasterAttachmentOperands>,
    },
    AttachmentPass {
        #[cfg_attr(
            not(test),
            allow(
                dead_code,
                reason = "owned attachment Views for general native graphics are subsequent work"
            )
        )]
        colors: Range<usize>,
        #[cfg_attr(
            not(test),
            allow(
                dead_code,
                reason = "owned attachment Views for general native graphics are subsequent work"
            )
        )]
        depth: Option<usize>,
        simple_clear: Option<RasterAttachmentOperands>,
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
