//! Logical resource identity, descriptions, snapshots and construction boundaries.

use std::ops::Range;

use super::{AccessRange, Graph, GraphError, GraphErrorKind, ImageAspect};

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

impl Graph {
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
    pub(super) fn buffer_id(&self, handle: BufferHandle) -> Result<usize, GraphError> {
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
    pub(super) fn image_id(&self, handle: ImageHandle) -> Result<usize, GraphError> {
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
    pub(super) fn buffer_range(
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
    pub(super) fn image_desc_at(&self, id: usize) -> Result<ImageDesc, GraphError> {
        match self.resources[id].desc {
            ResourceDesc::Image(d) => Ok(d),
            _ => Err(self.resource_error(GraphErrorKind::InvalidHandle, "expected image", id)),
        }
    }
    pub(super) fn color(&self, id: usize) -> Result<(), GraphError> {
        if self.image_desc_at(id)?.format != ImageFormat::Rgba8Unorm {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "operation requires RGBA8_UNORM",
                id,
            ));
        }
        Ok(())
    }
    pub(super) fn compatible_color_pair(&self, src: usize, dst: usize) -> Result<(), GraphError> {
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

    pub(super) fn resource_error(
        &self,
        kind: GraphErrorKind,
        message: impl Into<String>,
        id: usize,
    ) -> GraphError {
        self.resource_context(GraphError::new(kind, message), id)
    }
    pub(super) fn resource_context(&self, error: GraphError, id: usize) -> GraphError {
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
    pub(super) fn access_range(&self, id: usize, range: Range<u64>) -> AccessRange {
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
