//! GPU-independent declarations and analysis for the initial single-queue graph.
//!
//! Resources are mutable: reads consume the latest preceding write in declaration
//! order, including exact buffer ranges. Forward explicit dependencies constrain
//! scheduling, but never change which contents a read consumes. Every declaration,
//! including dead work, is validated. Only live operations are executed.
//! Imports describe runtime-owned resources and availability; they do not promise
//! CPU readiness. Image support is deliberately one complete mip/layer/aspect.

use std::{
    collections::BTreeMap,
    fmt,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

mod compile;

pub use compile::CompiledGraph;

static NEXT_GRAPH: AtomicU64 = AtomicU64::new(1);

/// A graph-local buffer; handles cannot be moved between graph instances.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct BufferHandle {
    pub(crate) graph: u64,
    pub(crate) index: usize,
}
/// A graph-local complete image.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ImageHandle {
    pub(crate) graph: u64,
    pub(crate) index: usize,
}
/// A graph-local operation declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PassHandle {
    pub(crate) graph: u64,
    pub(crate) index: usize,
}

/// Canonical identity inside a runtime; never a native API handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ResourceKey {
    pub(crate) owner: u64,
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferDesc {
    pub size: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFormat {
    Rgba8Unorm,
    Depth32Float,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageDesc {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
}

impl ImageDesc {
    pub fn rgba8(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: ImageFormat::Rgba8Unorm,
        }
    }
    pub fn depth32(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: ImageFormat::Depth32Float,
        }
    }
    pub fn byte_len(self) -> Option<u64> {
        u64::from(self.width)
            .checked_mul(u64::from(self.height))?
            .checked_mul(4)
    }
}

/// Runtime-created snapshot. A pending producer is an availability condition,
/// not a successful CPU wait. Submission revalidates its content revision;
/// read-only uses preserve it and rebase native synchronization state at submit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBuffer {
    pub(crate) key: ResourceKey,
    pub(crate) revision: u64,
    pub(crate) desc: BufferDesc,
    pub(crate) initialized: Vec<Range<u64>>,
}
impl ImportBuffer {
    pub fn key(&self) -> ResourceKey {
        self.key
    }
    pub fn desc(&self) -> BufferDesc {
        self.desc
    }
}

/// Runtime image contents, independent of the latest scheduled layout/access.
/// Read-only uses preserve this snapshot; writes or invalidation make it stale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportImage {
    pub(crate) key: ResourceKey,
    pub(crate) revision: u64,
    pub(crate) desc: ImageDesc,
    pub(crate) initialized: bool,
}
impl ImportImage {
    pub fn key(&self) -> ResourceKey {
        self.key
    }
    pub fn desc(&self) -> ImageDesc {
        self.desc
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadOp {
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
pub struct AttachmentOps {
    pub load: LoadOp,
    pub store: StoreOp,
}
impl AttachmentOps {
    pub const CLEAR_STORE: Self = Self {
        load: LoadOp::Clear,
        store: StoreOp::Store,
    };
    pub const LOAD_STORE: Self = Self {
        load: LoadOp::Load,
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

/// Native-independent categories for handling graph construction and analysis errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GraphErrorKind {
    /// Descriptors, values, access purposes or ranges are incompatible or invalid.
    InvalidInput,
    /// A handle has the current owner but no matching declaration of its kind.
    InvalidHandle,
    /// A resource or pass handle belongs to another graph.
    WrongOwner,
    /// The requested operation or subresource granularity is not supported.
    Unsupported,
    /// Repeated imports disagree about the same canonical runtime resource.
    InconsistentImport,
    /// A read or export requires contents not established by earlier declarations.
    UninitializedRead,
    /// Data, hazard or explicit dependencies contain a directed cycle.
    DependencyCycle,
    /// Application-defined failure created with [`GraphError::custom`].
    Custom,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct GraphErrorContext {
    pass: Option<PassHandle>,
    resource: Option<ResourceHandle>,
    pass_name: Option<String>,
    resource_name: Option<String>,
    range: Option<AccessRange>,
    cycle_passes: Vec<PassHandle>,
}

/// A classified error with optional owned graph context. Handles identify the
/// declaration involved; retaining an error does not retain its graph or GPU data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphError {
    pub message: String,
    kind: GraphErrorKind,
    context: Option<Box<GraphErrorContext>>,
}
impl GraphError {
    /// Creates an application-defined error, for example from a graph extension.
    pub fn custom(message: impl Into<String>) -> Self {
        Self::new(GraphErrorKind::Custom, message)
    }

    pub fn kind(&self) -> GraphErrorKind {
        self.kind
    }
    /// An existing pass involved in the error. Failed builders have no pass yet.
    pub fn pass(&self) -> Option<PassHandle> {
        self.context.as_ref().and_then(|context| context.pass)
    }
    /// The resource involved, when known; this may itself be an invalid handle.
    pub fn resource(&self) -> Option<ResourceHandle> {
        self.context.as_ref().and_then(|context| context.resource)
    }
    /// The existing pass name, or the requested name of a rejected builder.
    pub fn pass_name(&self) -> Option<&str> {
        self.context.as_ref()?.pass_name.as_deref()
    }
    /// The known resource name. Foreign handles never resolve names in this graph.
    pub fn resource_name(&self) -> Option<&str> {
        self.context.as_ref()?.resource_name.as_deref()
    }
    /// The offending access or undefined contents, when a range is representable.
    pub fn range(&self) -> Option<&AccessRange> {
        self.context.as_ref()?.range.as_ref()
    }
    /// The directed cycle, including its repeated first pass at the end.
    /// Names may be duplicated; these handles identify the actual participants.
    pub fn cycle_passes(&self) -> &[PassHandle] {
        self.context
            .as_ref()
            .map_or(&[], |context| context.cycle_passes.as_slice())
    }

    pub(crate) fn new(kind: GraphErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            context: None,
        }
    }
    fn context_mut(&mut self) -> &mut GraphErrorContext {
        self.context.get_or_insert_with(Box::default)
    }
    pub(crate) fn with_pass_name(mut self, name: &str) -> Self {
        self.context_mut().pass_name = Some(name.into());
        self
    }
    fn with_resource_name(mut self, name: &str) -> Self {
        self.context_mut().resource_name = Some(name.into());
        self
    }
    fn with_pass(mut self, handle: PassHandle, name: Option<&str>) -> Self {
        let context = self.context_mut();
        context.pass = Some(handle);
        context.pass_name = name.map(str::to_owned);
        self
    }
    pub(crate) fn with_resource(mut self, handle: ResourceHandle, name: Option<&str>) -> Self {
        let context = self.context_mut();
        context.resource = Some(handle);
        context.resource_name = name.map(str::to_owned);
        self
    }
    fn with_range(mut self, range: AccessRange) -> Self {
        self.context_mut().range = Some(range);
        self
    }
}
impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for GraphError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceDesc {
    Buffer(BufferDesc),
    Image(ImageDesc),
}
impl ResourceDesc {
    /// Compiler validity units: byte intervals for buffers, one unit for images.
    pub(crate) fn units(self) -> u64 {
        match self {
            Self::Buffer(d) => d.size,
            Self::Image(_) => 1,
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Resource {
    pub name: String,
    pub desc: ResourceDesc,
    pub import: Option<ResourceKey>,
    pub import_revision: u64,
    pub initial: Vec<Range<u64>>,
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
pub(crate) struct RasterDraw {
    pub vertices: usize,
    pub indices: usize,
    pub index_count: u32,
    pub parameters: RasterParameters,
}
/// Created only from immutable `MeshData`, never from a caller-supplied flag.
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
        draws: Arc<[RasterDraw]>,
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

/// An append-only graph. Handles never become stale within their own graph;
/// each new graph receives a distinct identity. No pass callbacks run at build.
#[derive(Debug)]
pub struct Graph {
    pub(crate) id: u64,
    pub(crate) resources: Vec<Resource>,
    pub(crate) passes: Vec<Pass>,
    pub(crate) exports: Vec<usize>,
    exported: Vec<bool>,
    imports: BTreeMap<ResourceKey, usize>,
}
impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    pub fn new() -> Self {
        let id = NEXT_GRAPH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("graph identity exhausted");
        Self {
            id,
            resources: Vec::new(),
            passes: Vec::new(),
            exports: Vec::new(),
            exported: Vec::new(),
            imports: BTreeMap::new(),
        }
    }
    pub fn buffer(
        &mut self,
        name: impl Into<String>,
        desc: BufferDesc,
    ) -> Result<BufferHandle, GraphError> {
        let name = name.into();
        if desc.size == 0 {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "buffer size must be nonzero",
            )
            .with_resource_name(&name));
        }
        let index = self.resources.len();
        self.resources.push(Resource {
            name,
            desc: ResourceDesc::Buffer(desc),
            import: None,
            import_revision: 0,
            initial: Vec::new(),
        });
        Ok(BufferHandle {
            graph: self.id,
            index,
        })
    }
    pub fn image(
        &mut self,
        name: impl Into<String>,
        desc: ImageDesc,
    ) -> Result<ImageHandle, GraphError> {
        let name = name.into();
        if desc.width == 0 || desc.height == 0 || desc.byte_len().is_none() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "image extent must be nonzero and its byte size representable",
            )
            .with_resource_name(&name));
        }
        let index = self.resources.len();
        self.resources.push(Resource {
            name,
            desc: ResourceDesc::Image(desc),
            import: None,
            import_revision: 0,
            initial: Vec::new(),
        });
        Ok(ImageHandle {
            graph: self.id,
            index,
        })
    }
    /// Returns the immutable description of a graph-local image. This checks
    /// graph ownership without initializing a native backend or promising contents.
    pub fn image_desc(&self, image: ImageHandle) -> Result<ImageDesc, GraphError> {
        self.image_desc_at(self.image_id(image)?)
    }

    pub fn import_buffer(
        &mut self,
        name: impl Into<String>,
        source: &ImportBuffer,
    ) -> Result<BufferHandle, GraphError> {
        let index = self.import(
            name.into(),
            ResourceDesc::Buffer(source.desc),
            source.key,
            source.revision,
            source.initialized.clone(),
        )?;
        Ok(BufferHandle {
            graph: self.id,
            index,
        })
    }
    pub fn import_image(
        &mut self,
        name: impl Into<String>,
        source: &ImportImage,
    ) -> Result<ImageHandle, GraphError> {
        let initial = if source.initialized {
            std::iter::once(0..1).collect()
        } else {
            Vec::new()
        };
        let index = self.import(
            name.into(),
            ResourceDesc::Image(source.desc),
            source.key,
            source.revision,
            initial,
        )?;
        Ok(ImageHandle {
            graph: self.id,
            index,
        })
    }
    fn import(
        &mut self,
        name: String,
        desc: ResourceDesc,
        key: ResourceKey,
        revision: u64,
        initial: Vec<Range<u64>>,
    ) -> Result<usize, GraphError> {
        if let Some(&index) = self.imports.get(&key) {
            let existing = &self.resources[index];
            if existing.desc != desc
                || existing.import_revision != revision
                || existing.initial != initial
            {
                return Err(self.resource_error(
                    GraphErrorKind::InconsistentImport,
                    "inconsistent repeated import snapshot",
                    index,
                ));
            }
            return Ok(index);
        }
        if desc.units() == 0
            || initial
                .iter()
                .any(|r| r.start >= r.end || r.end > desc.units())
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "invalid runtime import description or initialized range",
            )
            .with_resource_name(&name));
        }
        let index = self.resources.len();
        self.resources.push(Resource {
            name,
            desc,
            import: Some(key),
            import_revision: revision,
            initial,
        });
        self.imports.insert(key, index);
        Ok(index)
    }
    pub fn export_buffer(&mut self, buffer: BufferHandle) -> Result<(), GraphError> {
        let id = self.buffer_id(buffer)?;
        self.export(id);
        Ok(())
    }
    pub fn export_image(&mut self, image: ImageHandle) -> Result<(), GraphError> {
        let id = self.image_id(image)?;
        self.export(id);
        Ok(())
    }
    fn export(&mut self, id: usize) {
        if self.exported.len() <= id {
            self.exported.resize(id + 1, false);
        }
        if !self.exported[id] {
            self.exported[id] = true;
            self.exports.push(id);
        }
    }
    /// `pass` must execute after `dependency`. Forward edges are allowed and
    /// compile rejects cycles, including cycles in otherwise dead declarations.
    pub fn depends_on(
        &mut self,
        pass: PassHandle,
        dependency: PassHandle,
    ) -> Result<(), GraphError> {
        let pass = self.pass_id(pass)?;
        let dependency = self.pass_id(dependency)?;
        if !self.passes[pass].dependencies.contains(&dependency) {
            self.passes[pass].dependencies.push(dependency);
        }
        Ok(())
    }
    pub fn compile(&self) -> Result<CompiledGraph, GraphError> {
        compile::compile(self)
    }

    pub fn upload_buffer(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<PassHandle, GraphError> {
        self.upload_buffer_shared(name, dst, offset, bytes.into().into())
    }

    /// Retains shared immutable bytes without copying them during compilation.
    /// Reuse the same allocation when building graphs with the same upload data.
    pub fn upload_buffer_shared(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: Arc<[u8]>,
    ) -> Result<PassHandle, GraphError> {
        let source = 0..bytes.len();
        self.upload_buffer_slice(name, dst, offset, bytes, source)
    }

    /// Retain a slice of shared immutable input without allocating a chunk copy.
    /// The CPU source offset is independent of the aligned GPU destination offset.
    pub(crate) fn upload_buffer_slice(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: Arc<[u8]>,
        source: Range<usize>,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.buffer_id(dst)?;
            let slice = bytes.get(source.clone()).ok_or_else(|| {
                graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    format!("upload source range {source:?} exceeds shared input length {} or is reversed", bytes.len()),
                    dst,
                )
            })?;
            let range = graph.buffer_range(dst, offset, slice.len() as u64, true)?;
            Ok((
                Operation::UploadBuffer { dst, offset, bytes, source, mesh: None },
                vec![write(dst, range, AccessPurpose::CopyDestination)],
                false,
            ))
        })
    }
    /// Internal immutable-mesh upload. The destination has exactly this mesh
    /// stream's size, and each chunk preserves its byte position. No proof or
    /// purportedly validated flag is accepted from the public Graph API.
    pub(crate) fn upload_mesh_slice(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        data: &crate::engine::MeshData,
        indices: bool,
        source: Range<usize>,
    ) -> Result<PassHandle, GraphError> {
        let id = self.buffer_id(dst)?;
        let bytes = if indices {
            &data.indices
        } else {
            &data.vertices
        };
        if self.resources[id].desc.units() != bytes.len() as u64
            || !source.start.is_multiple_of(4)
            || !source.end.is_multiple_of(4)
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "managed mesh upload must preserve the stream size and aligned byte positions",
                id,
            ));
        }
        let pass =
            self.upload_buffer_slice(name, dst, source.start as u64, Arc::clone(bytes), source)?;
        let Operation::UploadBuffer { mesh, .. } = &mut self.passes[pass.index].operation else {
            unreachable!("newly constructed upload")
        };
        *mesh = Some(if indices {
            MeshUpload::Indices {
                byte_len: bytes.len() as u64,
                maximum: data.index_maximum,
            }
        } else {
            MeshUpload::Vertices
        });
        Ok(pass)
    }

    pub fn copy_buffer(
        &mut self,
        name: impl Into<String>,
        src: BufferHandle,
        src_offset: u64,
        dst: BufferHandle,
        dst_offset: u64,
        size: u64,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.buffer_id(src)?;
            let dst = graph.buffer_id(dst)?;
            let source = graph.buffer_range(src, src_offset, size, true)?;
            let target = graph.buffer_range(dst, dst_offset, size, true)?;
            if src == dst && overlaps(&source, &target) {
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping same-buffer copy",
                        dst,
                    )
                    .with_range(AccessRange::Buffer(target)));
            }
            Ok((
                Operation::CopyBuffer {
                    src,
                    dst,
                    src_offset,
                    dst_offset,
                    size,
                },
                vec![
                    read(src, source, AccessPurpose::CopySource),
                    write(dst, target, AccessPurpose::CopyDestination),
                ],
                false,
            ))
        })
    }
    pub fn readback_buffer(
        &mut self,
        name: impl Into<String>,
        src: BufferHandle,
        offset: u64,
        size: u64,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.buffer_id(src)?;
            let range = graph.buffer_range(src, offset, size, true)?;
            Ok((
                Operation::ReadbackBuffer { src, offset, size },
                vec![read(src, range, AccessPurpose::CopySource)],
                true,
            ))
        })
    }
    pub fn upload_image(
        &mut self,
        name: impl Into<String>,
        dst: ImageHandle,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<PassHandle, GraphError> {
        self.upload_image_shared(name, dst, bytes.into().into())
    }

    /// Retains one complete packed RGBA8 image as shared immutable upload data.
    pub fn upload_image_shared(
        &mut self,
        name: impl Into<String>,
        dst: ImageHandle,
        bytes: Arc<[u8]>,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.image_id(dst)?;
            graph.color(dst)?;
            if graph.image_desc_at(dst)?.byte_len() != Some(bytes.len() as u64) {
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "image upload must provide every packed RGBA8 texel",
                        dst,
                    )
                    .with_range(graph.access_range(dst, 0..1)));
            }
            Ok((
                Operation::UploadImage { dst, bytes },
                vec![write(dst, 0..1, AccessPurpose::CopyDestination)],
                false,
            ))
        })
    }
    pub fn copy_image(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::CopyImage { src, dst },
                vec![
                    read(src, 0..1, AccessPurpose::CopySource),
                    write(dst, 0..1, AccessPurpose::CopyDestination),
                ],
                false,
            ))
        })
    }
    pub fn readback_image(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            graph.color(src)?;
            Ok((
                Operation::ReadbackImage { src },
                vec![read(src, 0..1, AccessPurpose::CopySource)],
                true,
            ))
        })
    }
    pub fn compute(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
        transform: ComputeTransform,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::Compute {
                    src,
                    dst,
                    transform,
                },
                vec![
                    read(src, 0..1, AccessPurpose::StorageRead),
                    write(dst, 0..1, AccessPurpose::StorageWrite),
                ],
                false,
            ))
        })
    }
    /// Draw u32 indexed triangles using tightly packed seven-f32 vertices
    /// (`xyz`, then linear `rgba`). The runtime checks CPU-known index values
    /// against the vertex extent and rejects unknown index contents.
    #[allow(clippy::too_many_arguments)]
    pub fn raster(
        &mut self,
        name: impl Into<String>,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
        color: ImageHandle,
        depth: ImageHandle,
        color_ops: AttachmentOps,
        depth_ops: AttachmentOps,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) -> Result<PassHandle, GraphError> {
        self.raster_with_parameters(
            name,
            vertices,
            indices,
            index_count,
            color,
            depth,
            color_ops,
            depth_ops,
            clear_color,
            clear_depth,
            RasterParameters::default(),
        )
    }

    /// Adds a built-in draw with owned transform and tint values. It neither
    /// retains a scene borrow nor permits arbitrary shader/native commands.
    #[allow(clippy::too_many_arguments)]
    pub fn raster_with_parameters(
        &mut self,
        name: impl Into<String>,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
        color: ImageHandle,
        depth: ImageHandle,
        color_ops: AttachmentOps,
        depth_ops: AttachmentOps,
        clear_color: [f32; 4],
        clear_depth: f32,
        parameters: RasterParameters,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let draw = graph.raster_draw(vertices, indices, index_count, parameters)?;
            let vertices = draw.vertices;
            let indices = draw.indices;
            let color = graph.image_id(color)?;
            let depth = graph.image_id(depth)?;
            graph.color(color)?;
            let c = graph.image_desc_at(color)?;
            let d = graph.image_desc_at(depth)?;
            if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "raster needs matching RGBA8 color and D32 depth extents",
                    depth,
                ));
            }
            let vs = graph.resources[vertices].desc.units();
            let ix = u64::from(draw.index_count) * 4;
            if !clear_depth.is_finite()
                || !(0.0..=1.0).contains(&clear_depth)
                || clear_color.iter().any(|x| !x.is_finite())
            {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "invalid attachment clear value",
                ));
            }
            Ok((
                Operation::Raster {
                    vertices,
                    indices,
                    index_count,
                    color,
                    depth,
                    color_ops,
                    depth_ops,
                    clear_color,
                    clear_depth,
                    parameters,
                },
                vec![
                    read(vertices, 0..vs, AccessPurpose::Vertex),
                    read(indices, 0..ix, AccessPurpose::Index),
                    attachment(color, color_ops, AccessPurpose::ColorAttachment),
                    attachment(depth, depth_ops, AccessPurpose::DepthAttachment),
                ],
                false,
            ))
        })
    }

    /// Engine-only opaque list: clear once and store both complete targets.
    /// Draw order is retained. External buffer dependencies precede the single
    /// rendering scope; this operation admits no arbitrary shader side effects.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raster_batch(
        &mut self,
        name: impl Into<String>,
        draws: Vec<(BufferHandle, BufferHandle, u32, RasterParameters)>,
        color: ImageHandle,
        depth: ImageHandle,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) -> Result<PassHandle, GraphError> {
        if draws.is_empty() {
            return self.clear_attachments(name, color, depth, clear_color, clear_depth);
        }
        self.build_pass(name, |graph| {
            let color = graph.image_id(color)?;
            let depth = graph.image_id(depth)?;
            graph.color(color)?;
            let c = graph.image_desc_at(color)?;
            let d = graph.image_desc_at(depth)?;
            if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "raster batch needs matching RGBA8 color and D32 depth extents",
                    depth,
                ));
            }
            if !clear_depth.is_finite()
                || !(0.0..=1.0).contains(&clear_depth)
                || clear_color.iter().any(|x| !x.is_finite())
            {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "invalid attachment clear value",
                ));
            }
            let mut encoded = Vec::with_capacity(draws.len());
            // Preserve both purposes if a buffer supplies vertices and indices.
            let mut buffers = BTreeMap::<usize, [u64; 2]>::new();
            for (vertices, indices, index_count, parameters) in draws {
                let draw = graph.raster_draw(vertices, indices, index_count, parameters)?;
                let vs = graph.resources[draw.vertices].desc.units();
                let ix = u64::from(draw.index_count) * 4;
                buffers.entry(draw.vertices).or_default()[0] = vs;
                let index_range = &mut buffers.entry(draw.indices).or_default()[1];
                *index_range = (*index_range).max(ix);
                encoded.push(draw);
            }
            let mut accesses = Vec::with_capacity(buffers.len() * 2 + 2);
            for (resource, [vertices, indices]) in buffers {
                if vertices != 0 {
                    accesses.push(read(resource, 0..vertices, AccessPurpose::Vertex));
                }
                if indices != 0 {
                    accesses.push(read(resource, 0..indices, AccessPurpose::Index));
                }
            }
            accesses.push(attachment(
                color,
                AttachmentOps::CLEAR_STORE,
                AccessPurpose::ColorAttachment,
            ));
            accesses.push(attachment(
                depth,
                AttachmentOps::CLEAR_STORE,
                AccessPurpose::DepthAttachment,
            ));
            Ok((
                Operation::RasterBatch {
                    draws: encoded.into(),
                    color,
                    depth,
                    clear_color,
                    clear_depth,
                },
                accesses,
                false,
            ))
        })
    }

    /// Initializes a complete color/depth target even when a view has no draws.
    pub fn clear_attachments(
        &mut self,
        name: impl Into<String>,
        color: ImageHandle,
        depth: ImageHandle,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let color = graph.image_id(color)?;
            let depth = graph.image_id(depth)?;
            graph.color(color)?;
            let c = graph.image_desc_at(color)?;
            let d = graph.image_desc_at(depth)?;
            if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "clear needs matching RGBA8 color and D32 depth extents",
                    depth,
                ));
            }
            if !clear_depth.is_finite()
                || !(0.0..=1.0).contains(&clear_depth)
                || clear_color.iter().any(|x| !x.is_finite())
            {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "invalid attachment clear value",
                ));
            }
            Ok((
                Operation::ClearAttachments {
                    color,
                    depth,
                    clear_color,
                    clear_depth,
                },
                vec![
                    attachment(
                        color,
                        AttachmentOps::CLEAR_STORE,
                        AccessPurpose::ColorAttachment,
                    ),
                    attachment(
                        depth,
                        AttachmentOps::CLEAR_STORE,
                        AccessPurpose::DepthAttachment,
                    ),
                ],
                false,
            ))
        })
    }
    pub fn composite(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::Composite { src, dst },
                vec![
                    read(src, 0..1, AccessPurpose::Sampled),
                    write(dst, 0..1, AccessPurpose::ColorAttachment),
                ],
                false,
            ))
        })
    }
    /// Represent analysis-only work. A Vulkan runtime must explicitly reject
    /// live declaration-only operations rather than report successful execution.
    pub fn declare_pass(
        &mut self,
        name: impl Into<String>,
        kind: PassKind,
        accesses: Vec<Access>,
        side_effect: bool,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let mut encoded = Vec::with_capacity(accesses.len());
            for access in accesses {
                let id = match access.resource {
                    ResourceHandle::Buffer(h) => graph.buffer_id(h)?,
                    ResourceHandle::Image(h) => graph.image_id(h)?,
                };
                let range = match (&access.range, graph.resources[id].desc) {
                    (AccessRange::Buffer(r), ResourceDesc::Buffer(_)) => graph.buffer_range(
                        id,
                        r.start,
                        r.end
                            .checked_sub(r.start)
                            .ok_or_else(|| graph.resource_error(GraphErrorKind::InvalidInput, "reversed byte range", id)
                                .with_range(access.range.clone()))?,
                        false,
                    )?,
                    (
                        AccessRange::Image {
                            mip: 0,
                            layer: 0,
                            aspect,
                        },
                        ResourceDesc::Image(d),
                    ) if *aspect
                        == if d.format == ImageFormat::Rgba8Unorm {
                            ImageAspect::Color
                        } else {
                            ImageAspect::Depth
                        } =>
                    {
                        0..1
                    }
                    _ => {
                        return Err(graph.resource_error(GraphErrorKind::Unsupported,
                            "unsupported range: only complete mip 0/layer 0 and its matching aspect are supported",
                            id,
                        ).with_range(access.range.clone()));
                    }
                };
                validate_purpose(kind, graph.resources[id].desc, access.kind, access.purpose)
                    .map_err(|error| graph.resource_context(error, id).with_range(access.range.clone()))?;
                encoded.push(ResourceAccess {
                    resource: id,
                    range,
                    read: access.kind != AccessKind::Write,
                    write: access.kind != AccessKind::Read,
                    initializes: access.kind != AccessKind::Read,
                    discard_after: false,
                    purpose: access.purpose,
                });
            }
            for (i, a) in encoded.iter().enumerate() {
                for b in &encoded[i + 1..] {
                    if a.resource == b.resource && overlaps(&a.range, &b.range) && (a.write || b.write)
                    {
                        return Err(graph.resource_error(GraphErrorKind::InvalidInput,
                            "overlapping pass accesses must use one read_write declaration",
                            a.resource,
                        ).with_range(graph.access_range(a.resource, a.range.clone())));
                    }
                }
            }
            Ok((Operation::Declared(kind), encoded, side_effect))
        })
    }
    fn build_pass(
        &mut self,
        name: impl Into<String>,
        build: impl FnOnce(&Self) -> Result<(Operation, Vec<ResourceAccess>, bool), GraphError>,
    ) -> Result<PassHandle, GraphError> {
        let name = name.into();
        let (operation, accesses, side_effect) =
            build(self).map_err(|error| error.with_pass_name(&name))?;
        Ok(self.push(name, operation, accesses, side_effect))
    }

    fn push(
        &mut self,
        name: impl Into<String>,
        operation: Operation,
        accesses: Vec<ResourceAccess>,
        side_effect: bool,
    ) -> PassHandle {
        let index = self.passes.len();
        self.passes.push(Pass {
            name: name.into(),
            operation,
            accesses,
            side_effect,
            dependencies: Vec::new(),
        });
        PassHandle {
            graph: self.id,
            index,
        }
    }
    fn buffer_id(&self, handle: BufferHandle) -> Result<usize, GraphError> {
        if handle.graph != self.id {
            return Err(
                GraphError::new(GraphErrorKind::WrongOwner, "foreign graph buffer handle")
                    .with_resource(handle.into(), None),
            );
        }
        if !matches!(
            self.resources.get(handle.index).map(|r| r.desc),
            Some(ResourceDesc::Buffer(_))
        ) {
            return Err(GraphError::new(
                GraphErrorKind::InvalidHandle,
                "undefined or wrong-kind graph buffer handle",
            )
            .with_resource(
                handle.into(),
                self.resources.get(handle.index).map(|r| r.name.as_str()),
            ));
        }
        Ok(handle.index)
    }
    fn image_id(&self, handle: ImageHandle) -> Result<usize, GraphError> {
        if handle.graph != self.id {
            return Err(
                GraphError::new(GraphErrorKind::WrongOwner, "foreign graph image handle")
                    .with_resource(handle.into(), None),
            );
        }
        if !matches!(
            self.resources.get(handle.index).map(|r| r.desc),
            Some(ResourceDesc::Image(_))
        ) {
            return Err(GraphError::new(
                GraphErrorKind::InvalidHandle,
                "undefined or wrong-kind graph image handle",
            )
            .with_resource(
                handle.into(),
                self.resources.get(handle.index).map(|r| r.name.as_str()),
            ));
        }
        Ok(handle.index)
    }
    fn pass_id(&self, handle: PassHandle) -> Result<usize, GraphError> {
        if handle.graph != self.id {
            return Err(
                GraphError::new(GraphErrorKind::WrongOwner, "foreign graph pass handle")
                    .with_pass(handle, None),
            );
        }
        if handle.index >= self.passes.len() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidHandle,
                "undefined graph pass handle",
            )
            .with_pass(handle, None));
        }
        Ok(handle.index)
    }
    fn buffer_range(
        &self,
        id: usize,
        offset: u64,
        size: u64,
        copy_alignment: bool,
    ) -> Result<Range<u64>, GraphError> {
        let end = offset.checked_add(size).ok_or_else(|| {
            self.resource_error(GraphErrorKind::InvalidInput, "byte range overflow", id)
        })?;
        if size == 0
            || end > self.resources[id].desc.units()
            || (copy_alignment && !offset.is_multiple_of(4))
        {
            return Err(self.resource_error(GraphErrorKind::InvalidInput,
                "nonempty byte range must fit the buffer; copy offsets need four-byte alignment",
                id,
            ).with_range(AccessRange::Buffer(offset..end)));
        }
        Ok(offset..end)
    }
    fn raster_draw(
        &self,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
        parameters: RasterParameters,
    ) -> Result<RasterDraw, GraphError> {
        let vertices = self.buffer_id(vertices)?;
        let indices = self.buffer_id(indices)?;
        let vs = self.resources[vertices].desc.units();
        let ix = u64::from(index_count) * 4;
        if vs < 28
            || !vs.is_multiple_of(28)
            || index_count == 0
            || !index_count.is_multiple_of(3)
            || ix > self.resources[indices].desc.units()
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "raster requires seven-f32 vertices and nonzero triangle u32 indices within the index buffer",
                if vs < 28 || !vs.is_multiple_of(28) { vertices } else { indices },
            ));
        }
        if parameters
            .transform
            .iter()
            .flatten()
            .chain(&parameters.tint)
            .any(|x| !x.is_finite())
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "non-finite raster transform or tint",
            ));
        }
        Ok(RasterDraw {
            vertices,
            indices,
            index_count,
            parameters,
        })
    }

    fn image_desc_at(&self, id: usize) -> Result<ImageDesc, GraphError> {
        match self.resources[id].desc {
            ResourceDesc::Image(d) => Ok(d),
            _ => Err(self.resource_error(GraphErrorKind::InvalidHandle, "expected image", id)),
        }
    }
    fn color(&self, id: usize) -> Result<(), GraphError> {
        if self.image_desc_at(id)?.format != ImageFormat::Rgba8Unorm {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "operation requires RGBA8_UNORM",
                id,
            ));
        }
        Ok(())
    }
    fn compatible_color_pair(&self, src: usize, dst: usize) -> Result<(), GraphError> {
        self.color(src)?;
        self.color(dst)?;
        if src == dst || self.image_desc_at(src)? != self.image_desc_at(dst)? {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "image operation requires distinct images with equal RGBA8 descriptions",
                dst,
            ));
        }
        Ok(())
    }

    fn resource_error(
        &self,
        kind: GraphErrorKind,
        message: impl Into<String>,
        id: usize,
    ) -> GraphError {
        self.resource_context(GraphError::new(kind, message), id)
    }
    fn resource_context(&self, error: GraphError, id: usize) -> GraphError {
        let Some(resource) = self.resources.get(id) else {
            return error;
        };
        let handle = match resource.desc {
            ResourceDesc::Buffer(_) => BufferHandle {
                graph: self.id,
                index: id,
            }
            .into(),
            ResourceDesc::Image(_) => ImageHandle {
                graph: self.id,
                index: id,
            }
            .into(),
        };
        error.with_resource(handle, Some(&resource.name))
    }
    fn access_range(&self, id: usize, range: Range<u64>) -> AccessRange {
        match self.resources[id].desc {
            ResourceDesc::Buffer(_) => AccessRange::Buffer(range),
            ResourceDesc::Image(desc) => AccessRange::Image {
                mip: 0,
                layer: 0,
                aspect: if desc.format == ImageFormat::Rgba8Unorm {
                    ImageAspect::Color
                } else {
                    ImageAspect::Depth
                },
            },
        }
    }
}

pub(crate) fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}
fn read(resource: usize, range: Range<u64>, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        resource,
        range,
        read: true,
        write: false,
        initializes: false,
        discard_after: false,
        purpose,
    }
}
fn write(resource: usize, range: Range<u64>, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        resource,
        range,
        read: false,
        write: true,
        initializes: true,
        discard_after: false,
        purpose,
    }
}
fn attachment(resource: usize, ops: AttachmentOps, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        resource,
        range: 0..1,
        read: ops.load == LoadOp::Load,
        write: true,
        initializes: ops.load != LoadOp::Discard,
        discard_after: ops.store == StoreOp::Discard,
        purpose,
    }
}
fn validate_purpose(
    kind: PassKind,
    desc: ResourceDesc,
    access: AccessKind,
    purpose: AccessPurpose,
) -> Result<(), GraphError> {
    use AccessPurpose::*;
    let mode = match purpose {
        CopySource | StorageRead | Sampled | Vertex | Index => access == AccessKind::Read,
        CopyDestination | StorageWrite => access == AccessKind::Write,
        StorageReadWrite => access == AccessKind::ReadWrite,
        ColorAttachment | DepthAttachment => access != AccessKind::Read,
    };
    let domain = match purpose {
        CopySource | CopyDestination => kind == PassKind::Copy,
        StorageRead | StorageWrite | StorageReadWrite => kind == PassKind::Compute,
        Sampled => {
            kind == PassKind::Raster
                && matches!(
                    desc,
                    ResourceDesc::Image(ImageDesc {
                        format: ImageFormat::Rgba8Unorm,
                        ..
                    })
                )
        }
        Vertex | Index => kind == PassKind::Raster && matches!(desc, ResourceDesc::Buffer(_)),
        ColorAttachment => {
            kind == PassKind::Raster
                && matches!(
                    desc,
                    ResourceDesc::Image(ImageDesc {
                        format: ImageFormat::Rgba8Unorm,
                        ..
                    })
                )
        }
        DepthAttachment => {
            kind == PassKind::Raster
                && matches!(
                    desc,
                    ResourceDesc::Image(ImageDesc {
                        format: ImageFormat::Depth32Float,
                        ..
                    })
                )
        }
    };
    if !mode || !domain {
        return Err(GraphError::new(
            GraphErrorKind::InvalidInput,
            "access purpose, kind, resource type and pass kind are incompatible",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliced_uploads_share_backing_and_initialize_only_selected_bytes() {
        let input: Arc<[u8]> = Arc::from([99, 1, 2, 3, 4, 5, 6, 7, 8, 88]);
        let mut graph = Graph::new();
        let buffer = graph
            .buffer("sliced destination", BufferDesc { size: 8 })
            .unwrap();
        let first = graph
            .upload_buffer_slice("first chunk", buffer, 0, Arc::clone(&input), 1..5)
            .unwrap();
        let second = graph
            .upload_buffer_slice("second chunk", buffer, 4, Arc::clone(&input), 5..9)
            .unwrap();
        graph.export_buffer(buffer).unwrap();
        let plan = graph.compile().unwrap();
        for (pass, expected_source, expected_target) in [(first, 1..5, 0..4), (second, 5..9, 4..8)]
        {
            let Operation::UploadBuffer { bytes, source, .. } = &plan.passes[pass.index].operation
            else {
                panic!("live sliced upload must retain its source");
            };
            assert!(Arc::ptr_eq(bytes, &input));
            assert_eq!(*source, expected_source);
            assert_eq!(plan.passes[pass.index].accesses[0].range, expected_target);
        }
        let mut partial = Graph::new();
        let target = partial.buffer("partial", BufferDesc { size: 8 }).unwrap();
        partial
            .upload_buffer_slice("tail only", target, 4, input, 1..5)
            .unwrap();
        partial.export_buffer(target).unwrap();
        let error = partial.compile().unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
        assert_eq!(error.range(), Some(&AccessRange::Buffer(0..4)));
    }

    #[test]
    fn invalid_upload_slices_preserve_builder_context_without_a_pass() {
        let input: Arc<[u8]> = vec![0; 16].into();
        let mut graph = Graph::new();
        let buffer = graph.buffer("destination", BufferDesc { size: 8 }).unwrap();
        for source in [Range { start: 8, end: 4 }, 13..17, 4..4, 0..12] {
            let error = graph
                .upload_buffer_slice("invalid chunk", buffer, 0, Arc::clone(&input), source)
                .unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.pass_name(), Some("invalid chunk"));
            assert_eq!(error.resource_name(), Some("destination"));
            assert!(graph.passes.is_empty());
        }
        // Keep the existing copy contract: offsets are aligned, but a source
        // slice may start anywhere and need not contain a multiple of four bytes.
        graph
            .upload_buffer_slice("three bytes", buffer, 0, input, 1..4)
            .unwrap();
    }

    #[test]
    fn many_exports_keep_declaration_order_and_indexed_membership() {
        let mut graph = Graph::new();
        let bytes: Arc<[u8]> = vec![1; 4].into();
        let buffers = (0..4_096)
            .map(|_| {
                let buffer = graph.buffer("asset", BufferDesc { size: 4 }).unwrap();
                graph
                    .upload_buffer_shared("upload", buffer, 0, Arc::clone(&bytes))
                    .unwrap();
                buffer
            })
            .collect::<Vec<_>>();
        let unused = graph.buffer("unexported", BufferDesc { size: 4 }).unwrap();
        for &buffer in buffers.iter().rev() {
            graph.export_buffer(buffer).unwrap();
            graph.export_buffer(buffer).unwrap();
        }
        let expected = buffers
            .iter()
            .rev()
            .map(|buffer| buffer.index)
            .collect::<Vec<_>>();
        assert_eq!(graph.exports, expected);
        let compiled = graph.compile().unwrap();
        assert_eq!(compiled.exports, expected);
        assert_eq!(compiled.pass_count(), buffers.len());
        assert!(
            buffers
                .iter()
                .all(|buffer| compiled.has_export(buffer.index))
        );
        assert!(!compiled.has_export(unused.index));
        assert!(!compiled.has_export(usize::MAX));
    }

    #[test]
    fn compilation_shares_live_uploads_and_releases_culled_payloads() {
        let buffer_bytes: Arc<[u8]> = vec![1; 4].into();
        let image_bytes: Arc<[u8]> = vec![2; 4].into();
        let dead_buffer_bytes: Arc<[u8]> = vec![3; 4].into();
        let dead_image_bytes: Arc<[u8]> = vec![4; 4].into();
        let buffer_lifetime = Arc::downgrade(&buffer_bytes);
        let image_lifetime = Arc::downgrade(&image_bytes);
        let dead_buffer_lifetime = Arc::downgrade(&dead_buffer_bytes);
        let dead_image_lifetime = Arc::downgrade(&dead_image_bytes);
        let mut graph = Graph::new();
        let buffer = graph.buffer("live buffer", BufferDesc { size: 4 }).unwrap();
        let image = graph.image("live image", ImageDesc::rgba8(1, 1)).unwrap();
        let dead_buffer = graph.buffer("dead buffer", BufferDesc { size: 4 }).unwrap();
        let dead_image = graph.image("dead image", ImageDesc::rgba8(1, 1)).unwrap();
        let buffer_upload = graph
            .upload_buffer_shared("live buffer upload", buffer, 0, Arc::clone(&buffer_bytes))
            .unwrap();
        let image_upload = graph
            .upload_image_shared("live image upload", image, Arc::clone(&image_bytes))
            .unwrap();
        let dead_buffer_upload = graph
            .upload_buffer_shared(
                "dead buffer upload",
                dead_buffer,
                0,
                Arc::clone(&dead_buffer_bytes),
            )
            .unwrap();
        let dead_image_upload = graph
            .upload_image_shared(
                "dead image upload",
                dead_image,
                Arc::clone(&dead_image_bytes),
            )
            .unwrap();
        graph.export_buffer(buffer).unwrap();
        graph.export_image(image).unwrap();

        let plan = graph.compile().unwrap();
        let Operation::UploadBuffer { bytes, .. } = &plan.passes[buffer_upload.index].operation
        else {
            panic!("live buffer upload must remain executable");
        };
        assert!(Arc::ptr_eq(bytes, &buffer_bytes));
        let Operation::UploadImage { bytes, .. } = &plan.passes[image_upload.index].operation
        else {
            panic!("live image upload must remain executable");
        };
        assert!(Arc::ptr_eq(bytes, &image_bytes));
        assert!(matches!(
            plan.passes[dead_buffer_upload.index].operation,
            Operation::Culled
        ));
        assert!(matches!(
            plan.passes[dead_image_upload.index].operation,
            Operation::Culled
        ));

        drop((
            buffer_bytes,
            image_bytes,
            dead_buffer_bytes,
            dead_image_bytes,
        ));
        drop(graph);
        assert!(buffer_lifetime.upgrade().is_some());
        assert!(image_lifetime.upgrade().is_some());
        assert!(dead_buffer_lifetime.upgrade().is_none());
        assert!(dead_image_lifetime.upgrade().is_none());
        drop(plan);
        assert!(buffer_lifetime.upgrade().is_none());
        assert!(image_lifetime.upgrade().is_none());
    }

    #[test]
    fn typed_handles_reject_foreign_undefined_and_wrong_kind_resources() {
        let mut graph = Graph::new();
        let buffer = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
        let image = graph.image("rgba", ImageDesc::rgba8(2, 2)).unwrap();
        let mut other = Graph::new();
        assert!(
            other
                .upload_buffer("foreign", buffer, 0, vec![1; 4])
                .is_err()
        );
        assert!(
            graph
                .upload_buffer(
                    "undefined",
                    BufferHandle {
                        graph: graph.id,
                        index: 100
                    },
                    0,
                    vec![1; 4]
                )
                .is_err()
        );
        assert!(
            graph
                .upload_buffer(
                    "wrong kind",
                    BufferHandle {
                        graph: graph.id,
                        index: image.index
                    },
                    0,
                    vec![1; 4]
                )
                .is_err()
        );
        assert!(other.export_image(image).is_err());
    }

    #[test]
    fn zero_extents_overflows_and_invalid_byte_ranges_are_rejected() {
        let mut graph = Graph::new();
        assert!(graph.buffer("empty", BufferDesc { size: 0 }).is_err());
        assert!(graph.image("zero", ImageDesc::rgba8(0, 2)).is_err());
        assert!(
            graph
                .image("overflow", ImageDesc::rgba8(u32::MAX, u32::MAX))
                .is_err()
        );
        let b = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
        assert!(graph.upload_buffer("empty", b, 0, vec![]).is_err());
        assert!(graph.upload_buffer("alignment", b, 1, vec![0; 4]).is_err());
        assert!(graph.upload_buffer("past end", b, 16, vec![0]).is_err());
        assert!(
            graph
                .copy_buffer("overflow", b, u64::MAX - 3, b, 0, 8)
                .is_err()
        );
        assert!(graph.copy_buffer("overlap", b, 0, b, 4, 8).is_err());
    }

    #[test]
    fn repeated_imports_share_one_identity_and_reject_changed_snapshots() {
        let mut graph = Graph::new();
        let import = ImportBuffer {
            key: ResourceKey {
                owner: 4,
                index: 2,
                generation: 3,
            },
            revision: 9,
            desc: BufferDesc { size: 16 },
            initialized: std::iter::once(0..8).collect(),
        };
        let first = graph.import_buffer("first", &import).unwrap();
        let again = graph.import_buffer("again", &import).unwrap();
        assert_eq!(first, again);
        assert_eq!(graph.resources.len(), 1);
        let mut changed = import.clone();
        changed.revision += 1;
        let error = graph.import_buffer("stale alias", &changed).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
        assert_eq!(error.resource(), Some(first.into()));
        assert_eq!(error.resource_name(), Some("first"));
        let image = ImportImage {
            key: import.key,
            revision: 9,
            desc: ImageDesc::rgba8(2, 2),
            initialized: true,
        };
        assert!(graph.import_image("wrong kind alias", &image).is_err());
    }

    #[test]
    fn image_metadata_preserves_imports_and_rejects_invalid_local_handles() {
        let mut graph = Graph::new();
        let source = ImportImage {
            key: ResourceKey {
                owner: 7,
                index: 4,
                generation: 2,
            },
            revision: 3,
            desc: ImageDesc::depth32(9, 5),
            initialized: false,
        };
        let image = graph.import_image("imported depth", &source).unwrap();
        assert_eq!(graph.image_desc(image).unwrap(), source.desc);
        assert_eq!(graph.import_image("alias", &source).unwrap(), image);
        let mut changed = source.clone();
        changed.desc = ImageDesc::rgba8(9, 5);
        let error = graph
            .import_image("different description", &changed)
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
        assert_eq!(error.resource(), Some(image.into()));

        let buffer = graph.buffer("buffer", BufferDesc { size: 4 }).unwrap();
        for invalid in [
            ImageHandle {
                graph: graph.id,
                index: buffer.index,
            },
            ImageHandle {
                graph: graph.id,
                index: usize::MAX,
            },
        ] {
            let error = graph.image_desc(invalid).unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidHandle);
            assert_eq!(error.resource(), Some(invalid.into()));
        }
    }

    #[test]
    fn only_supported_whole_image_subresources_are_accepted() {
        let mut graph = Graph::new();
        let image = graph.image("rgba", ImageDesc::rgba8(2, 2)).unwrap();
        for range in [
            AccessRange::Image {
                mip: 1,
                layer: 0,
                aspect: ImageAspect::Color,
            },
            AccessRange::Image {
                mip: 0,
                layer: 1,
                aspect: ImageAspect::Color,
            },
            AccessRange::Image {
                mip: 0,
                layer: 0,
                aspect: ImageAspect::Depth,
            },
            AccessRange::Buffer(0..1),
        ] {
            assert!(
                graph
                    .declare_pass(
                        "unsupported",
                        PassKind::Compute,
                        vec![Access {
                            resource: image.into(),
                            range,
                            kind: AccessKind::Write,
                            purpose: AccessPurpose::StorageWrite
                        }],
                        false
                    )
                    .is_err()
            );
        }
        graph
            .declare_pass(
                "whole",
                PassKind::Compute,
                vec![Access {
                    resource: image.into(),
                    range: AccessRange::Image {
                        mip: 0,
                        layer: 0,
                        aspect: ImageAspect::Color,
                    },
                    kind: AccessKind::Write,
                    purpose: AccessPurpose::StorageWrite,
                }],
                true,
            )
            .unwrap();
        assert_eq!(graph.compile().unwrap().pass_count(), 1);
    }

    #[test]
    fn fixed_operations_check_binding_formats_extents_and_aliasing() {
        let mut graph = Graph::new();
        let a = graph.image("a", ImageDesc::rgba8(2, 2)).unwrap();
        let b = graph.image("b", ImageDesc::rgba8(3, 2)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        assert!(graph.upload_image("short", a, vec![0; 15]).is_err());
        assert!(
            graph
                .upload_image("depth bytes", depth, vec![0; 16])
                .is_err()
        );
        assert!(
            graph
                .compute("alias", a, a, ComputeTransform::Invert)
                .is_err()
        );
        assert!(
            graph
                .compute("extent", a, b, ComputeTransform::Invert)
                .is_err()
        );
        assert!(graph.composite("depth", depth, a).is_err());
        assert!(
            graph
                .declare_pass(
                    "bad binding",
                    PassKind::Compute,
                    vec![Access {
                        resource: a.into(),
                        range: AccessRange::Image {
                            mip: 0,
                            layer: 0,
                            aspect: ImageAspect::Color
                        },
                        kind: AccessKind::Write,
                        purpose: AccessPurpose::Sampled
                    }],
                    true
                )
                .is_err()
        );
    }

    fn mesh(graph: &mut Graph) -> (BufferHandle, BufferHandle, ImageHandle, ImageHandle) {
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        graph
            .upload_buffer("upload vertices", vertices, 0, vec![0; 84])
            .unwrap();
        graph
            .upload_buffer("upload indices", indices, 0, vec![0; 12])
            .unwrap();
        let color = graph.image("color", ImageDesc::rgba8(2, 2)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        (vertices, indices, color, depth)
    }

    #[test]
    fn attachment_load_needs_contents_and_store_discard_invalidates_them() {
        let mut graph = Graph::new();
        let (v, i, c, d) = mesh(&mut graph);
        graph
            .raster(
                "undefined load",
                v,
                i,
                3,
                c,
                d,
                AttachmentOps::LOAD_STORE,
                AttachmentOps::CLEAR_STORE,
                [0.0; 4],
                1.0,
            )
            .unwrap();
        assert!(
            graph
                .compile()
                .unwrap_err()
                .to_string()
                .contains("uninitialized")
        );

        let mut graph = Graph::new();
        let (v, i, c, d) = mesh(&mut graph);
        graph
            .raster(
                "discarded",
                v,
                i,
                3,
                c,
                d,
                AttachmentOps {
                    load: LoadOp::Clear,
                    store: StoreOp::Discard,
                },
                AttachmentOps::CLEAR_STORE,
                [0.0; 4],
                1.0,
            )
            .unwrap();
        graph.readback_image("read discarded", c).unwrap();
        assert!(
            graph
                .compile()
                .unwrap_err()
                .to_string()
                .contains("uninitialized")
        );
    }

    #[test]
    fn partial_raster_after_discard_does_not_claim_full_initialization() {
        let mut graph = Graph::new();
        let (v, i, c, d) = mesh(&mut graph);
        graph
            .raster(
                "partial",
                v,
                i,
                3,
                c,
                d,
                AttachmentOps {
                    load: LoadOp::Discard,
                    store: StoreOp::Store,
                },
                AttachmentOps::CLEAR_STORE,
                [0.0; 4],
                1.0,
            )
            .unwrap();
        graph.export_image(c).unwrap();
        assert!(
            graph
                .compile()
                .unwrap_err()
                .to_string()
                .contains("uninitialized")
        );
    }

    #[test]
    fn batch_requires_initialized_buffers_from_later_draws() {
        let mut graph = Graph::new();
        let (vertices, indices, color, depth) = mesh(&mut graph);
        let missing = graph
            .buffer("later vertices", BufferDesc { size: 84 })
            .unwrap();
        graph
            .raster_batch(
                "opaque batch",
                vec![
                    (vertices, indices, 3, RasterParameters::default()),
                    (missing, indices, 3, RasterParameters::default()),
                ],
                color,
                depth,
                [0.0; 4],
                1.0,
            )
            .unwrap();
        graph.export_image(color).unwrap();
        let error = graph.compile().unwrap_err().to_string();
        assert!(
            error.contains("later vertices") && error.contains("uninitialized"),
            "{error}"
        );
    }

    #[test]
    fn batch_same_buffer_can_supply_vertex_and_index_reads() {
        let mut graph = Graph::new();
        let (vertices, _, color, depth) = mesh(&mut graph);
        graph
            .raster_batch(
                "opaque batch",
                vec![(vertices, vertices, 3, RasterParameters::default()); 3],
                color,
                depth,
                [0.0; 4],
                1.0,
            )
            .unwrap();
        graph.export_image(color).unwrap();
        let compiled = graph.compile().unwrap();
        let dump = compiled.dump();
        assert!(dump.contains("purpose=Vertex") && dump.contains("purpose=Index"));
    }

    #[test]
    fn batch_rejections_preserve_builder_context_without_partial_declarations() {
        let mut graph = Graph::new();
        let (vertices, indices, color, depth) = mesh(&mut graph);
        let before = graph.passes.len();
        let error = graph
            .raster_batch(
                "invalid opaque input",
                vec![
                    (vertices, indices, 3, RasterParameters::default()),
                    (vertices, indices, 6, RasterParameters::default()),
                ],
                color,
                depth,
                [0.0; 4],
                1.0,
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("invalid opaque input"));
        assert_eq!(error.pass(), None);
        assert_eq!(error.resource(), Some(indices.into()));
        assert_eq!(error.resource_name(), Some("indices"));
        assert_eq!(graph.passes.len(), before);

        let mut other = Graph::new();
        let foreign = other
            .buffer("foreign vertices", BufferDesc { size: 84 })
            .unwrap();
        let error = graph
            .raster_batch(
                "foreign opaque input",
                vec![(foreign, indices, 3, RasterParameters::default())],
                color,
                depth,
                [0.0; 4],
                1.0,
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
        assert_eq!(error.pass_name(), Some("foreign opaque input"));
        assert_eq!(error.resource(), Some(foreign.into()));
        assert_eq!(error.resource_name(), None);
        assert_eq!(graph.passes.len(), before);
    }

    #[test]
    fn import_identity_survives_interleaved_transients_and_failed_aliases() {
        let mut graph = Graph::new();
        let sources = (0..64)
            .map(|index| ImportBuffer {
                key: ResourceKey {
                    owner: 7,
                    index,
                    generation: 1,
                },
                revision: u64::from(index) + 1,
                desc: BufferDesc { size: 12 },
                initialized: std::iter::once(0..12).collect(),
            })
            .collect::<Vec<_>>();
        let mut handles = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            graph.image("transient", ImageDesc::rgba8(2, 2)).unwrap();
            handles.push(
                graph
                    .import_buffer(format!("import {index}"), source)
                    .unwrap(),
            );
        }
        for (source, &handle) in sources.iter().zip(&handles).rev() {
            assert_eq!(graph.import_buffer("alias", source).unwrap(), handle);
        }
        let first = &sources[0];
        let image_alias = ImportImage {
            key: first.key,
            revision: first.revision,
            desc: ImageDesc::rgba8(1, 3),
            initialized: true,
        };
        let error = graph.import_image("wrong kind", &image_alias).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
        assert_eq!(error.resource(), Some(handles[0].into()));
        assert_eq!(error.resource_name(), Some("import 0"));
        assert_eq!(
            graph.import_buffer("still canonical", first).unwrap(),
            handles[0]
        );
        assert_eq!(graph.resources.len(), sources.len() * 2);
    }
}
