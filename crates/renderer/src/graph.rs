//! GPU-independent declarations and analysis for the initial single-queue graph.
//!
//! Resources are mutable: reads consume the latest preceding write in declaration
//! order, including exact buffer ranges. Forward explicit dependencies constrain
//! scheduling, but never change which contents a read consumes. Every declaration,
//! including dead work, is validated. Only live operations are executed.
//! Imports describe runtime-owned resources and availability; they do not promise
//! CPU readiness. Image support is deliberately one complete mip/layer/aspect.

use std::{
    fmt,
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
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
/// not a successful CPU wait. Runtime submission revalidates its revision.
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphError {
    pub message: String,
}
impl GraphError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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
        bytes: Vec<u8>,
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
        bytes: Vec<u8>,
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
        }
    }
    pub fn buffer(
        &mut self,
        name: impl Into<String>,
        desc: BufferDesc,
    ) -> Result<BufferHandle, GraphError> {
        if desc.size == 0 {
            return Err(GraphError::new("buffer size must be nonzero"));
        }
        let index = self.resources.len();
        self.resources.push(Resource {
            name: name.into(),
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
        if desc.width == 0 || desc.height == 0 || desc.byte_len().is_none() {
            return Err(GraphError::new(
                "image extent must be nonzero and its byte size representable",
            ));
        }
        let index = self.resources.len();
        self.resources.push(Resource {
            name: name.into(),
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
        if let Some((index, existing)) = self
            .resources
            .iter()
            .enumerate()
            .find(|(_, r)| r.import == Some(key))
        {
            if existing.desc != desc
                || existing.import_revision != revision
                || existing.initial != initial
            {
                return Err(GraphError::new("inconsistent repeated import snapshot"));
            }
            return Ok(index);
        }
        if desc.units() == 0
            || initial
                .iter()
                .any(|r| r.start >= r.end || r.end > desc.units())
        {
            return Err(GraphError::new(
                "invalid runtime import description or initialized range",
            ));
        }
        let index = self.resources.len();
        self.resources.push(Resource {
            name,
            desc,
            import: Some(key),
            import_revision: revision,
            initial,
        });
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
        if !self.exports.contains(&id) {
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
        let dst = self.buffer_id(dst)?;
        let bytes = bytes.into();
        let range = self.buffer_range(dst, offset, bytes.len() as u64, true)?;
        Ok(self.push(
            name,
            Operation::UploadBuffer { dst, offset, bytes },
            vec![write(dst, range, AccessPurpose::CopyDestination)],
            false,
        ))
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
        let src = self.buffer_id(src)?;
        let dst = self.buffer_id(dst)?;
        let source = self.buffer_range(src, src_offset, size, true)?;
        let target = self.buffer_range(dst, dst_offset, size, true)?;
        if src == dst && overlaps(&source, &target) {
            return Err(GraphError::new("overlapping same-buffer copy"));
        }
        Ok(self.push(
            name,
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
    }
    pub fn readback_buffer(
        &mut self,
        name: impl Into<String>,
        src: BufferHandle,
        offset: u64,
        size: u64,
    ) -> Result<PassHandle, GraphError> {
        let src = self.buffer_id(src)?;
        let range = self.buffer_range(src, offset, size, true)?;
        Ok(self.push(
            name,
            Operation::ReadbackBuffer { src, offset, size },
            vec![read(src, range, AccessPurpose::CopySource)],
            true,
        ))
    }
    pub fn upload_image(
        &mut self,
        name: impl Into<String>,
        dst: ImageHandle,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<PassHandle, GraphError> {
        let dst = self.image_id(dst)?;
        self.color(dst)?;
        let bytes = bytes.into();
        if self.image_desc(dst)?.byte_len() != Some(bytes.len() as u64) {
            return Err(GraphError::new(
                "image upload must provide every packed RGBA8 texel",
            ));
        }
        Ok(self.push(
            name,
            Operation::UploadImage { dst, bytes },
            vec![write(dst, 0..1, AccessPurpose::CopyDestination)],
            false,
        ))
    }
    pub fn copy_image(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        let src = self.image_id(src)?;
        let dst = self.image_id(dst)?;
        self.compatible_color_pair(src, dst)?;
        Ok(self.push(
            name,
            Operation::CopyImage { src, dst },
            vec![
                read(src, 0..1, AccessPurpose::CopySource),
                write(dst, 0..1, AccessPurpose::CopyDestination),
            ],
            false,
        ))
    }
    pub fn readback_image(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        let src = self.image_id(src)?;
        self.color(src)?;
        Ok(self.push(
            name,
            Operation::ReadbackImage { src },
            vec![read(src, 0..1, AccessPurpose::CopySource)],
            true,
        ))
    }
    pub fn compute(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
        transform: ComputeTransform,
    ) -> Result<PassHandle, GraphError> {
        let src = self.image_id(src)?;
        let dst = self.image_id(dst)?;
        self.compatible_color_pair(src, dst)?;
        Ok(self.push(
            name,
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
        let vertices = self.buffer_id(vertices)?;
        let indices = self.buffer_id(indices)?;
        let color = self.image_id(color)?;
        let depth = self.image_id(depth)?;
        self.color(color)?;
        let c = self.image_desc(color)?;
        let d = self.image_desc(depth)?;
        if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
            return Err(GraphError::new(
                "raster needs matching RGBA8 color and D32 depth extents",
            ));
        }
        let vs = self.resources[vertices].desc.units();
        let ix = u64::from(index_count) * 4;
        if vs < 28
            || !vs.is_multiple_of(28)
            || index_count == 0
            || !index_count.is_multiple_of(3)
            || ix > self.resources[indices].desc.units()
        {
            return Err(GraphError::new(
                "raster requires seven-f32 vertices and nonzero triangle u32 indices within the index buffer",
            ));
        }
        if !clear_depth.is_finite()
            || !(0.0..=1.0).contains(&clear_depth)
            || clear_color.iter().any(|x| !x.is_finite())
        {
            return Err(GraphError::new("invalid attachment clear value"));
        }
        Ok(self.push(
            name,
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
            },
            vec![
                read(vertices, 0..vs, AccessPurpose::Vertex),
                read(indices, 0..ix, AccessPurpose::Index),
                attachment(color, color_ops, AccessPurpose::ColorAttachment),
                attachment(depth, depth_ops, AccessPurpose::DepthAttachment),
            ],
            false,
        ))
    }
    pub fn composite(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        let src = self.image_id(src)?;
        let dst = self.image_id(dst)?;
        self.compatible_color_pair(src, dst)?;
        Ok(self.push(
            name,
            Operation::Composite { src, dst },
            vec![
                read(src, 0..1, AccessPurpose::Sampled),
                write(dst, 0..1, AccessPurpose::ColorAttachment),
            ],
            false,
        ))
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
        let mut encoded = Vec::with_capacity(accesses.len());
        for access in accesses {
            let id = match access.resource {
                ResourceHandle::Buffer(h) => self.buffer_id(h)?,
                ResourceHandle::Image(h) => self.image_id(h)?,
            };
            let range = match (&access.range, self.resources[id].desc) {
                (AccessRange::Buffer(r), ResourceDesc::Buffer(_)) => self.buffer_range(
                    id,
                    r.start,
                    r.end
                        .checked_sub(r.start)
                        .ok_or_else(|| GraphError::new("reversed byte range"))?,
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
                    return Err(GraphError::new(
                        "unsupported range: only complete mip 0/layer 0 and its matching aspect are supported",
                    ));
                }
            };
            validate_purpose(kind, self.resources[id].desc, access.kind, access.purpose)?;
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
                    return Err(GraphError::new(
                        "overlapping pass accesses must use one read_write declaration",
                    ));
                }
            }
        }
        Ok(self.push(name, Operation::Declared(kind), encoded, side_effect))
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
        if handle.graph != self.id
            || !matches!(
                self.resources.get(handle.index).map(|r| r.desc),
                Some(ResourceDesc::Buffer(_))
            )
        {
            return Err(GraphError::new(
                "undefined, wrong-kind or foreign graph buffer handle",
            ));
        }
        Ok(handle.index)
    }
    fn image_id(&self, handle: ImageHandle) -> Result<usize, GraphError> {
        if handle.graph != self.id
            || !matches!(
                self.resources.get(handle.index).map(|r| r.desc),
                Some(ResourceDesc::Image(_))
            )
        {
            return Err(GraphError::new(
                "undefined, wrong-kind or foreign graph image handle",
            ));
        }
        Ok(handle.index)
    }
    fn pass_id(&self, handle: PassHandle) -> Result<usize, GraphError> {
        if handle.graph != self.id || handle.index >= self.passes.len() {
            return Err(GraphError::new("undefined or foreign graph pass handle"));
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
        let end = offset
            .checked_add(size)
            .ok_or_else(|| GraphError::new("byte range overflow"))?;
        if size == 0
            || end > self.resources[id].desc.units()
            || (copy_alignment && !offset.is_multiple_of(4))
        {
            return Err(GraphError::new(
                "nonempty byte range must fit the buffer; copy offsets need four-byte alignment",
            ));
        }
        Ok(offset..end)
    }
    fn image_desc(&self, id: usize) -> Result<ImageDesc, GraphError> {
        match self.resources[id].desc {
            ResourceDesc::Image(d) => Ok(d),
            _ => Err(GraphError::new("expected image")),
        }
    }
    fn color(&self, id: usize) -> Result<(), GraphError> {
        if self.image_desc(id)?.format != ImageFormat::Rgba8Unorm {
            return Err(GraphError::new("operation requires RGBA8_UNORM"));
        }
        Ok(())
    }
    fn compatible_color_pair(&self, src: usize, dst: usize) -> Result<(), GraphError> {
        self.color(src)?;
        self.color(dst)?;
        if src == dst || self.image_desc(src)? != self.image_desc(dst)? {
            return Err(GraphError::new(
                "image operation requires distinct images with equal RGBA8 descriptions",
            ));
        }
        Ok(())
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
            "access purpose, kind, resource type and pass kind are incompatible",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(graph.import_buffer("stale alias", &changed).is_err());
        let image = ImportImage {
            key: import.key,
            revision: 9,
            desc: ImageDesc::rgba8(2, 2),
            initialized: true,
        };
        assert!(graph.import_image("wrong kind alias", &image).is_err());
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
}
