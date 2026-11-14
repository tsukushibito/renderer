//! Pass declarations, access purposes and immutable execution operands.

use std::{ops::Range, sync::Arc};

use super::{BufferHandle, GraphError, GraphErrorKind, ImageHandle};

/// How an attachment begins. Only a clear operation carries a clear value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadOp<T> {
    Load,
    Clear(T),
    Discard,
}

/// A complete image and the operations applied to its contents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attachment<T> {
    pub image: ImageHandle,
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

/// Named color and depth targets for the built-in mesh pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterAttachments {
    pub color: Attachment<[f32; 4]>,
    pub depth: Attachment<f32>,
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
impl AttachmentOps {
    pub const CLEAR_STORE: Self = Self {
        load: LoadAction::Clear,
        store: StoreOp::Store,
    };
    #[cfg(test)]
    pub const LOAD_STORE: Self = Self {
        load: LoadAction::Load,
        store: StoreOp::Store,
    };
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

/// One indexed triangle draw using the built-in mesh shader ABI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterDraw {
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
    StorageRead,
    StorageWrite,
    StorageReadWrite,
    Sampled,
    Vertex,
    Index,
    ColorAttachment,
    DepthAttachment,
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

/// Owned operands for one fixed-pipeline draw in an engine opaque pass.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(dead_code, reason = "native batch recording operands")
)]
pub(crate) struct RasterDrawOperands {
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
    /// Retains a culled pass's metadata without retaining its execution payload.
    Culled,
    UploadBuffer {
        dst: usize,
        offset: u64,
        bytes: Arc<[u8]>,
        source: Range<usize>,
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
        bytes: Arc<[u8]>,
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
    Raster {
        vertices: usize,
        indices: usize,
        index_count: u32,
        color: usize,
        depth: usize,
        color_ops: AttachmentOps,
        depth_ops: AttachmentOps,
        clear_color: [f32; 4],
        clear_depth: f32,
        parameters: RasterParameters,
    },
    RasterBatch {
        draws: Arc<[RasterDrawOperands]>,
        color: usize,
        depth: usize,
        clear_color: [f32; 4],
        clear_depth: f32,
    },
    ClearAttachments {
        color: usize,
        depth: usize,
        clear_color: [f32; 4],
        clear_depth: f32,
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
    pub name: String,
    pub operation: Operation,
    pub accesses: Vec<ResourceAccess>,
    pub side_effect: bool,
    pub dependencies: Vec<usize>,
}
