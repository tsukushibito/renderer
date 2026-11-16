//! Logical resource identity, descriptions, snapshots and construction boundaries.

use std::ops::Range;

use super::{AccessRange, Graph, GraphCapacityKind, GraphError, GraphErrorKind, ImageAspect};

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
/// One graph-local image subresource. Views retain the image's identity rather
/// than declaring another resource, so overlapping views share content/hazards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageView {
    pub(crate) image: ImageHandle,
    pub(crate) mip: u32,
    pub(crate) layer: u32,
    pub(crate) aspect: ImageAspect,
}
impl ImageView {
    pub fn image(self) -> ImageHandle {
        self.image
    }
    pub fn mip_level(self) -> u32 {
        self.mip
    }
    pub fn array_layer(self) -> u32 {
        self.layer
    }
    pub fn aspect(self) -> ImageAspect {
        self.aspect
    }
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
    Rgba16Float,
    Depth32Float,
}

impl ImageFormat {
    pub(crate) const fn texel_bytes(self) -> u64 {
        match self {
            Self::Rgba8Unorm | Self::Depth32Float => 4,
            Self::Rgba16Float => 8,
        }
    }

    pub(crate) const fn aspect(self) -> ImageAspect {
        match self {
            Self::Rgba8Unorm | Self::Rgba16Float => ImageAspect::Color,
            Self::Depth32Float => ImageAspect::Depth,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageDesc {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub mip_levels: u32,
    pub array_layers: u32,
}

impl ImageDesc {
    pub fn rgba8(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: ImageFormat::Rgba8Unorm,
            mip_levels: 1,
            array_layers: 1,
        }
    }
    pub fn depth32(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: ImageFormat::Depth32Float,
            mip_levels: 1,
            array_layers: 1,
        }
    }
    /// Scene-linear floating-point color, packed as four little-endian IEEE 754
    /// binary16 components per texel in RGBA order.
    pub fn rgba16_float(width: u32, height: u32) -> Self {
        Self {
            format: ImageFormat::Rgba16Float,
            ..Self::rgba8(width, height)
        }
    }
    pub fn byte_len(self) -> Option<u64> {
        if self.width == 0
            || self.height == 0
            || self.mip_levels == 0
            || self.mip_levels > self.maximum_mip_levels()
            || self.array_layers == 0
        {
            return None;
        }
        let mut bytes = 0u64;
        for mip in 0..self.mip_levels {
            let [width, height] = self.mip_extent(mip)?;
            let level = u64::from(width)
                .checked_mul(u64::from(height))?
                .checked_mul(self.format.texel_bytes())?;
            bytes = bytes.checked_add(level)?;
        }
        bytes.checked_mul(u64::from(self.array_layers))
    }

    pub(crate) fn is_single_subresource(self) -> bool {
        self.mip_levels == 1 && self.array_layers == 1
    }

    pub(crate) fn mip_extent(self, mip: u32) -> Option<[u32; 2]> {
        if self.width == 0
            || self.height == 0
            || mip >= self.mip_levels
            || mip >= self.maximum_mip_levels()
        {
            return None;
        }
        Some([
            self.width.checked_shr(mip)?.max(1),
            self.height.checked_shr(mip)?.max(1),
        ])
    }

    fn maximum_mip_levels(self) -> u32 {
        u32::BITS - self.width.max(self.height).leading_zeros()
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
    pub(crate) initialized: super::ImportRanges,
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
    /// Compiler validity units: buffer bytes or layer-major image subresources.
    pub(crate) fn units(self) -> u64 {
        match self {
            Self::Buffer(d) => d.size,
            Self::Image(desc) => u64::from(desc.mip_levels) * u64::from(desc.array_layers),
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Resource {
    pub name: Range<usize>,
    pub desc: ResourceDesc,
    pub import: Option<ResourceKey>,
    pub import_revision: u64,
    pub initial: Range<usize>,
}

impl Graph {
    fn add_resource(
        &mut self,
        name: &str,
        desc: ResourceDesc,
        import: Option<ResourceKey>,
        revision: u64,
        initial: &[Range<u64>],
    ) -> Result<usize, GraphError> {
        if self.resources.len() == self.capacity.resources {
            return Err(super::storage::capacity_error(
                GraphCapacityKind::Resources,
                self.capacity.resources,
                self.resources.len().checked_add(1),
            ));
        }
        if initial.len()
            > self
                .capacity
                .initial_ranges
                .saturating_sub(self.initial_ranges.len())
        {
            return Err(super::storage::capacity_error(
                GraphCapacityKind::InitialRanges,
                self.capacity.initial_ranges,
                self.initial_ranges.len().checked_add(initial.len()),
            ));
        }
        let name = self.store_name(name)?;
        let index = self.resources.len();
        let start = self.initial_ranges.len();
        let data = self.data_mut();
        data.initial_ranges.extend_from_slice(initial);
        data.resources.push(Resource {
            name,
            desc,
            import,
            import_revision: revision,
            initial: start..data.initial_ranges.len(),
        });
        data.exported.push(false);
        Ok(index)
    }
    pub fn buffer(&mut self, name: &str, desc: BufferDesc) -> Result<BufferHandle, GraphError> {
        if desc.size == 0 {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "buffer size must be nonzero",
            )
            .with_resource_name(name));
        }
        let index = self.add_resource(name, ResourceDesc::Buffer(desc), None, 0, &[])?;
        Ok(BufferHandle {
            graph: self.id,
            index,
        })
    }
    pub fn image(&mut self, name: &str, desc: ImageDesc) -> Result<ImageHandle, GraphError> {
        if desc.width == 0 || desc.height == 0 || desc.byte_len().is_none() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "image extent, mip/layer counts and total byte size must be valid",
            )
            .with_resource_name(name));
        }
        let index = self.add_resource(name, ResourceDesc::Image(desc), None, 0, &[])?;
        Ok(ImageHandle {
            graph: self.id,
            index,
        })
    }
    pub fn image_desc(&self, image: ImageHandle) -> Result<ImageDesc, GraphError> {
        self.image_desc_at(self.image_id(image)?)
    }
    /// Select one complete mip/layer without registering or allocating another
    /// resource. The aspect follows the image format and cannot be overridden.
    pub fn image_view(
        &self,
        image: ImageHandle,
        mip: u32,
        layer: u32,
    ) -> Result<ImageView, GraphError> {
        let id = self.image_id(image)?;
        let desc = self.image_desc_at(id)?;
        let aspect = desc.format.aspect();
        if mip >= desc.mip_levels || layer >= desc.array_layers {
            return Err(self
                .resource_error(
                    GraphErrorKind::InvalidInput,
                    "image View exceeds its mip/layer bounds",
                    id,
                )
                .with_range(AccessRange::Image { mip, layer, aspect }));
        }
        Ok(ImageView {
            image,
            mip,
            layer,
            aspect,
        })
    }
    pub fn import_buffer(
        &mut self,
        name: &str,
        source: &ImportBuffer,
    ) -> Result<BufferHandle, GraphError> {
        let index = self.import(
            name,
            ResourceDesc::Buffer(source.desc),
            source.key,
            source.revision,
            &source.initialized,
        )?;
        Ok(BufferHandle {
            graph: self.id,
            index,
        })
    }
    pub fn import_image(
        &mut self,
        name: &str,
        source: &ImportImage,
    ) -> Result<ImageHandle, GraphError> {
        let initialized = 0..ResourceDesc::Image(source.desc).units();
        let initial = if source.initialized {
            std::slice::from_ref(&initialized)
        } else {
            &[]
        };
        let index = self.import(
            name,
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
        name: &str,
        desc: ResourceDesc,
        key: ResourceKey,
        revision: u64,
        initial: &[Range<u64>],
    ) -> Result<usize, GraphError> {
        let position = self.imports.binary_search_by_key(&key, |&(key, _)| key);
        if let Ok(position) = position {
            let index = self.imports[position].1;
            let existing = &self.resources[index];
            if existing.desc != desc
                || existing.import_revision != revision
                || &self.initial_ranges[existing.initial.clone()] != initial
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
            .with_resource_name(name));
        }
        let index = self.add_resource(name, desc, Some(key), revision, initial)?;
        self.data_mut()
            .imports
            .insert(position.unwrap_err(), (key, index));
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
            self.data_mut().exported.resize(id + 1, false);
        }
        if !self.exported[id] {
            self.data_mut().exported[id] = true;
            self.data_mut().exports.push(id);
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
                self.resources.get(handle.index).map(|r| self.name(&r.name)),
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
                self.resources.get(handle.index).map(|r| self.name(&r.name)),
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
        self.single_image(id)?;
        if self.image_desc_at(id)?.format != ImageFormat::Rgba8Unorm {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "operation requires RGBA8_UNORM",
                id,
            ));
        }
        Ok(())
    }
    /// Fixed whole-image operations currently encode only one mip/layer.
    pub(super) fn single_image(&self, id: usize) -> Result<(), GraphError> {
        if !self.image_desc_at(id)?.is_single_subresource() {
            return Err(self.resource_error(
                GraphErrorKind::Unsupported,
                "operation requires a single-mip single-layer image",
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

    pub(super) fn compatible_image_copy_pair(
        &self,
        src: usize,
        dst: usize,
    ) -> Result<(), GraphError> {
        self.single_image(src)?;
        self.single_image(dst)?;
        let source = self.image_desc_at(src)?;
        if src == dst
            || source != self.image_desc_at(dst)?
            || source.format == ImageFormat::Depth32Float
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "image copy requires distinct color images with equal descriptions",
                dst,
            ));
        }
        Ok(())
    }

    pub(super) fn resource_error(
        &self,
        kind: GraphErrorKind,
        message: impl Into<std::borrow::Cow<'static, str>>,
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
        error.with_resource(handle, Some(self.name(&resource.name)))
    }
    pub(super) fn access_range(&self, id: usize, range: Range<u64>) -> AccessRange {
        match self.resources[id].desc {
            ResourceDesc::Buffer(_) => AccessRange::Buffer(range),
            ResourceDesc::Image(desc) => AccessRange::Image {
                // Errors spanning several cells identify the first undefined
                // subresource, rather than reporting the flattened unit index.
                mip: (range.start % u64::from(desc.mip_levels)) as u32,
                layer: (range.start / u64::from(desc.mip_levels)) as u32,
                aspect: desc.format.aspect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphCapacity, GraphPool};

    #[test]
    fn packed_bytes_include_shrinking_mips_and_every_layer() {
        let desc = ImageDesc {
            mip_levels: 3,
            array_layers: 2,
            ..ImageDesc::rgba8(4, 2)
        };
        assert_eq!(desc.byte_len(), Some(88));
        assert_eq!(desc.mip_extent(0), Some([4, 2]));
        assert_eq!(desc.mip_extent(1), Some([2, 1]));
        assert_eq!(desc.mip_extent(2), Some([1, 1]));
        assert_eq!(desc.mip_extent(3), None);
        assert!(!desc.is_single_subresource());
        assert_eq!(ImageDesc::rgba8(4, 2).byte_len(), Some(32));
        assert_eq!(ImageDesc::depth32(4, 2).byte_len(), Some(32));
        assert_eq!(ImageDesc::rgba16_float(4, 2).byte_len(), Some(64));
        assert_eq!(
            ImageDesc {
                format: ImageFormat::Rgba16Float,
                ..desc
            }
            .byte_len(),
            Some(176)
        );
        assert_eq!(ImageDesc::rgba16_float(u32::MAX, u32::MAX).byte_len(), None);
        assert!(ImageDesc::depth32(4, 2).is_single_subresource());
    }

    #[test]
    fn invalid_descriptors_do_not_consume_the_prepared_resource_slot() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: 1,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let base = ImageDesc::rgba8(4, 2);
        for desc in [
            ImageDesc::rgba8(0, 2),
            ImageDesc::rgba8(4, 0),
            ImageDesc {
                mip_levels: 0,
                ..base
            },
            ImageDesc {
                mip_levels: 4,
                ..base
            },
            ImageDesc {
                array_layers: 0,
                ..base
            },
            ImageDesc::rgba8(u32::MAX, u32::MAX),
            ImageDesc {
                array_layers: u32::MAX,
                ..ImageDesc::rgba8(65_536, 65_536)
            },
        ] {
            assert_eq!(desc.byte_len(), None);
            assert_eq!(
                graph.image("invalid", desc).unwrap_err().kind(),
                GraphErrorKind::InvalidInput
            );
        }
        let image = graph.image("valid", base).unwrap();
        assert_eq!(graph.image_desc(image).unwrap(), base);
    }

    #[test]
    fn views_preserve_identity_and_reject_foreign_or_recycled_graph_handles() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let image = graph
            .image(
                "array",
                ImageDesc {
                    mip_levels: 3,
                    array_layers: 2,
                    ..ImageDesc::rgba8(4, 2)
                },
            )
            .unwrap();
        let view = graph.image_view(image, 2, 1).unwrap();
        assert_eq!(view.image(), image);
        assert_eq!(view.mip_level(), 2);
        assert_eq!(view.array_layer(), 1);
        assert_eq!(view.aspect(), ImageAspect::Color);
        let depth = graph.image("depth", ImageDesc::depth32(4, 2)).unwrap();
        assert_eq!(
            graph.image_view(depth, 0, 0).unwrap().aspect(),
            ImageAspect::Depth
        );
        for (mip, layer) in [(3, 0), (0, 2), (u32::MAX, u32::MAX)] {
            let error = graph.image_view(image, mip, layer).unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.resource(), Some(image.into()));
            assert_eq!(
                error.range(),
                Some(&AccessRange::Image {
                    mip,
                    layer,
                    aspect: ImageAspect::Color
                })
            );
        }
        let mut other_pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let other = other_pool.begin().unwrap();
        assert_eq!(
            other.image_view(image, 0, 0).unwrap_err().kind(),
            GraphErrorKind::WrongOwner
        );
        drop(graph);
        let graph = pool.begin().unwrap();
        assert_eq!(
            graph.image_view(view.image(), 0, 0).unwrap_err().kind(),
            GraphErrorKind::WrongOwner
        );
    }

    #[test]
    fn initialized_import_covers_the_complete_image_descriptor() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let desc = ImageDesc {
            mip_levels: 3,
            array_layers: 2,
            ..ImageDesc::rgba8(4, 2)
        };
        let mut graph = pool.begin().unwrap();
        let image = graph
            .import_image(
                "complete",
                &ImportImage {
                    key: ResourceKey {
                        owner: 7,
                        index: 0,
                        generation: 1,
                    },
                    revision: 3,
                    desc,
                    initialized: true,
                },
            )
            .unwrap();
        graph.export_image(image).unwrap();
        assert_eq!(graph.compile().unwrap().pass_count(), 0);
    }
}
