//! Logical resource identity, descriptions, snapshots and construction boundaries.

use std::ops::Range;

use crate::ImageFormat;

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
/// A graph-local contiguous mip range in one image layer. Views retain the image's identity rather
/// than declaring another resource, so overlapping views share content/hazards.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ImageView {
    pub(crate) image: ImageHandle,
    pub(crate) mip: u32,
    pub(crate) mip_count: u32,
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
    /// Number of consecutive levels interpreted by this View. Transfer and
    /// attachment operations require one level; sampled bindings may span more.
    pub fn mip_level_count(self) -> u32 {
        self.mip_count
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
    /// Logical byte length. [`Graph::buffer`] rejects zero; native alignment,
    /// memory placement and Device size limits are separate runtime concerns.
    pub size: u64,
}

impl ImageFormat {
    pub(crate) const fn aspect(self) -> ImageAspect {
        match self {
            Self::Depth32Float => ImageAspect::Depth,
            _ => ImageAspect::Color,
        }
    }
}

/// Number of raster samples in one image texel.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SampleCount {
    #[default]
    X1,
    X4,
}

impl SampleCount {
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::X1 => 1,
            Self::X4 => 4,
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
    pub sample_count: SampleCount,
}

impl ImageDesc {
    /// Single-sample 2D texture with one mip level.
    pub const fn texture_2d(width: u32, height: u32, format: ImageFormat) -> Self {
        Self {
            width,
            height,
            format,
            mip_levels: 1,
            array_layers: 1,
            sample_count: SampleCount::X1,
        }
    }
    pub const fn rgba8(width: u32, height: u32) -> Self {
        Self::texture_2d(width, height, ImageFormat::Rgba8Unorm)
    }
    /// Four FP32 channels for sampled data.
    pub const fn rgba32_float(width: u32, height: u32) -> Self {
        Self::texture_2d(width, height, ImageFormat::Rgba32Float)
    }
    /// Eight-bit sRGB color for sampling and transfer, with linear alpha.
    pub const fn rgba8_srgb(width: u32, height: u32) -> Self {
        Self::texture_2d(width, height, ImageFormat::Rgba8Srgb)
    }
    pub const fn depth32(width: u32, height: u32) -> Self {
        Self::texture_2d(width, height, ImageFormat::Depth32Float)
    }
    /// Scene-linear floating-point color, packed as four little-endian IEEE 754
    /// binary16 components per texel in RGBA order.
    pub const fn rgba16_float(width: u32, height: u32) -> Self {
        Self::texture_2d(width, height, ImageFormat::Rgba16Float)
    }

    /// Include this many consecutive levels starting at mip zero.
    pub const fn with_mip_levels(self, mip_levels: u32) -> Self {
        Self { mip_levels, ..self }
    }

    pub const fn with_sample_count(self, sample_count: SampleCount) -> Self {
        Self {
            sample_count,
            ..self
        }
    }

    pub fn byte_len(self) -> Option<u64> {
        if self.width == 0
            || self.height == 0
            || self.mip_levels == 0
            || self.mip_levels > self.maximum_mip_levels()
            || self.array_layers == 0
            || (self.sample_count == SampleCount::X4
                && (self.mip_levels != 1
                    || self.array_layers != 1
                    || !self.format.supports_attachment()))
        {
            return None;
        }
        let mut bytes = 0u64;
        for mip in 0..self.mip_levels {
            let [width, height] = self.mip_extent(mip)?;
            let level = self.format.packed_byte_len(width, height)?;
            bytes = bytes.checked_add(level)?;
        }
        bytes
            .checked_mul(u64::from(self.array_layers))?
            .checked_mul(u64::from(self.sample_count.as_u32()))
    }

    pub(crate) fn is_single_subresource(self) -> bool {
        self.mip_levels == 1 && self.array_layers == 1
    }

    /// Returns the width and height at a valid mip level.
    pub fn mip_extent(self, mip: u32) -> Option<[u32; 2]> {
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
    key: ResourceKey,
    revision: u64,
    desc: BufferDesc,
    initialized: super::ImportRanges,
}
impl ImportBuffer {
    /// Issue an immutable snapshot at a runtime creation/change boundary.
    /// Descriptor and canonical content bounds are checked here once; Graph
    /// construction consumes the frozen guarantee rather than rescanning it.
    #[cfg(any(test, sashiko_gpu_backend))]
    pub(crate) fn new(
        key: ResourceKey,
        revision: u64,
        desc: BufferDesc,
        initialized: super::ImportRanges,
    ) -> Result<Self, GraphError> {
        if desc.size == 0 {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "runtime buffer snapshot size must be nonzero",
            ));
        }
        validate_snapshot_ranges(&initialized, desc.size)?;
        Ok(Self {
            key,
            revision,
            desc,
            initialized,
        })
    }
    pub fn key(&self) -> ResourceKey {
        self.key
    }
    pub fn desc(&self) -> BufferDesc {
        self.desc
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn initialized(&self) -> &super::ImportRanges {
        &self.initialized
    }
}

/// Runtime image contents, independent of the latest scheduled layout/access.
/// Read-only uses preserve this snapshot; writes or invalidation make it stale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportImage {
    key: ResourceKey,
    revision: u64,
    desc: ImageDesc,
    initialized: bool,
    partial_mips: u32,
}
impl ImportImage {
    /// Metadata fixtures only; real runtimes derive coverage from their state.
    #[cfg(test)]
    pub(crate) fn test_initialized(key: ResourceKey, revision: u64, desc: ImageDesc) -> Self {
        let complete = 0..ResourceDesc::Image(desc).units();
        Self::new(key, revision, desc, std::slice::from_ref(&complete)).unwrap()
    }
    /// Derive the complete-image content guarantee from canonical runtime
    /// ranges; a caller cannot supply an unverified `initialized` boolean.
    #[cfg(any(test, sashiko_gpu_backend))]
    pub(crate) fn new(
        key: ResourceKey,
        revision: u64,
        desc: ImageDesc,
        initialized: &[Range<u64>],
    ) -> Result<Self, GraphError> {
        if desc.byte_len().is_none() {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "runtime image snapshot description must be valid",
            ));
        }
        let complete = validate_snapshot_ranges(initialized, ResourceDesc::Image(desc).units())?;
        // Native 2D images have at most 32 mip levels. The bitset is retained in
        // the token itself; importing a mip chain never leases a CPU range slot.
        let partial_mips = if complete || desc.array_layers != 1 {
            0
        } else {
            initialized
                .iter()
                .fold(0, |mask, r| mask | mip_mask(r.start as u32..r.end as u32))
        };
        Ok(Self {
            key,
            revision,
            desc,
            initialized: complete,
            partial_mips,
        })
    }
    pub fn key(&self) -> ResourceKey {
        self.key
    }
    pub fn desc(&self) -> ImageDesc {
        self.desc
    }
    /// Scheduled content revision, scoped to this registration key.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Whether this mip of a single-layer image is fully initialized.
    pub fn is_mip_initialized(&self, mip_level: u32) -> bool {
        self.desc.array_layers == 1
            && mip_level < self.desc.mip_levels
            && (self.initialized || self.partial_mips & (1 << mip_level) != 0)
    }
    /// Whether every mip in a nonempty contiguous range has scheduled contents.
    pub fn contains_mips(&self, levels: Range<u32>) -> bool {
        self.desc.array_layers == 1
            && !levels.is_empty()
            && levels.end <= self.desc.mip_levels
            && (self.initialized || {
                let mask = mip_mask(levels);
                self.partial_mips & mask == mask
            })
    }
    pub(crate) fn initialized(&self) -> bool {
        self.initialized
    }
}

// A validated single-layer 2D image has at most 32 mips. Each nonempty run
// fits one word even when it includes mip 31.
fn mip_mask(range: Range<u32>) -> u32 {
    (u32::MAX >> (32 - (range.end - range.start))) << range.start
}

pub(super) fn mip_ranges(mut mask: u32) -> impl Iterator<Item = Range<u64>> {
    std::iter::from_fn(move || {
        if mask == 0 {
            return None;
        }
        let start = mask.trailing_zeros();
        let end = start + (mask >> start).trailing_ones();
        mask &= !mip_mask(start..end);
        Some(u64::from(start)..u64::from(end))
    })
}

#[cfg(any(test, sashiko_gpu_backend))]
fn validate_snapshot_ranges(initialized: &[Range<u64>], units: u64) -> Result<bool, GraphError> {
    let mut end = 0;
    let mut complete = true;
    for range in initialized {
        if range.start >= range.end || range.end > units || range.start < end {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "runtime snapshot ranges must be ordered, nonoverlapping and within the resource",
            ));
        }
        complete &= range.start == end;
        end = range.end;
    }
    Ok(complete && end == units)
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
    pub read_only_input: bool,
    pub written_in_graph: bool,
    pub export_mips: u32,
}

impl Graph {
    /// Whether an earlier pass may write this image before the next declared pass.
    pub fn image_has_pending_write(&self, image: &ImportImage) -> bool {
        self.imports
            .get(&image.key())
            .is_some_and(|&i| self.resources[i].written_in_graph)
    }

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
            read_only_input: false,
            written_in_graph: false,
            export_mips: 0,
        });
        data.exported.push(false);
        Ok(index)
    }
    /// Declare a nonempty transient buffer with initially undefined contents.
    /// This reserves logical metadata only, not native memory. Use upload/copy
    /// or a reviewed producer before reading/exporting the required bytes.
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
    /// Declare a transient image with initially undefined contents.
    /// Extents, mip/layer counts and packed byte length must be representable.
    /// Logical support does not imply that every backend supports every use;
    /// native capabilities and prepared working-set limits are checked later.
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
    /// Create a texture, upload its tightly packed mip chain, and export it in
    /// this submission. Call repeatedly to batch asset uploads in one Graph.
    /// The bytes are captured during this call and may immediately be reused.
    /// A failed upload leaves the Graph ready for another texture.
    pub fn upload_texture_for_export(
        &mut self,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> Result<ImageHandle, GraphError> {
        let name_end = self.names.len();
        let image = self.image(name, desc)?;
        if let Err(error) = self.upload_image(name, image, bytes) {
            // upload_image has already returned its unpublished pass storage.
            // A fresh transient has no import entry or initial ranges.
            let data = self.data_mut();
            data.resources.pop();
            data.exported.pop();
            data.names.truncate(name_end);
            // The slot may be reused by a different resource on retry.
            return Err(error.without_resource_handle());
        }
        // Successful full-image upload establishes the single-sample contract.
        self.export(image.index);
        Ok(image)
    }

    /// Query a local transient or imported buffer's immutable description.
    /// No contents, initialization or GPU-completion guarantee is implied.
    /// Foreign handles, including an earlier checkout of this pool, are rejected.
    pub fn buffer_desc(&self, buffer: BufferHandle) -> Result<BufferDesc, GraphError> {
        let id = self.buffer_id(buffer)?;
        let ResourceDesc::Buffer(desc) = self.resources[id].desc else {
            unreachable!("buffer_id established the resource kind")
        };
        Ok(desc)
    }
    /// Query a local transient or imported image's immutable description.
    /// No contents, initialization or GPU-completion guarantee is implied.
    /// Foreign handles, including an earlier checkout of this pool, are rejected.
    pub fn image_desc(&self, image: ImageHandle) -> Result<ImageDesc, GraphError> {
        self.image_desc_at(self.image_id(image)?)
    }

    /// Require an image to have the exact description expected by a caller.
    /// A mismatch reports the local image and both descriptions.
    pub fn require_image_desc(
        &self,
        image: ImageHandle,
        expected: ImageDesc,
    ) -> Result<(), GraphError> {
        let id = self.image_id(image)?;
        let actual = self.image_desc_at(id)?;
        if actual != expected {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                format!("expected image description {expected:?}; got {actual:?}"),
                id,
            ));
        }
        Ok(())
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
            mip_count: 1,
            layer,
            aspect,
        })
    }
    /// Select a contiguous range of mip levels for a sampled binding. The
    /// complete range is declared as a read, so an uninitialized level cannot
    /// be hidden behind an initialized base level. Array layers remain separate.
    pub fn sampled_image_view(
        &self,
        image: ImageHandle,
        base_mip_level: u32,
        mip_level_count: u32,
    ) -> Result<ImageView, GraphError> {
        let id = self.image_id(image)?;
        let desc = self.image_desc_at(id)?;
        if mip_level_count == 0
            || base_mip_level
                .checked_add(mip_level_count)
                .is_none_or(|end| end > desc.mip_levels)
            || desc.array_layers != 1
            || desc.sample_count != SampleCount::X1
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "sampled View requires a nonempty mip range in one image layer",
                id,
            ));
        }
        Ok(ImageView {
            image,
            mip: base_mip_level,
            mip_count: mip_level_count,
            layer: 0,
            aspect: desc.format.aspect(),
        })
    }
    /// Connect a runtime-issued buffer snapshot to this graph.
    ///
    /// The snapshot supplies canonical identity, content revision, description
    /// and known initialized ranges. Consistent repeated imports return the
    /// same logical handle; conflicting snapshots of one identity are rejected.
    /// Submission checks that mutable runtime state still matches the snapshot.
    /// Importing does not wait for GPU completion or transfer registration
    /// ownership from the original caller.
    pub fn import_buffer(
        &mut self,
        name: &str,
        source: &ImportBuffer,
    ) -> Result<BufferHandle, GraphError> {
        let index = self.import(
            name,
            ResourceDesc::Buffer(source.desc),
            source.key,
            source.revision(),
            source.initialized(),
        )?;
        Ok(BufferHandle {
            graph: self.id,
            index,
        })
    }
    /// Import a buffer for shared reading. Every alias of this registration in
    /// the graph is protected against writes and re-export. Copy to a separate
    /// buffer when independent writable contents are needed.
    pub fn import_readonly_buffer(
        &mut self,
        name: &str,
        source: &ImportBuffer,
    ) -> Result<BufferHandle, GraphError> {
        let previous_len = self.resources.len();
        let buffer = self.import_buffer(name, source)?;
        self.protect_input(buffer, buffer.index >= previous_len)?;
        Ok(buffer)
    }

    /// Connect a runtime-issued image snapshot to this graph.
    ///
    /// Consistent repeated imports share one logical identity. A single-layer
    /// snapshot preserves the initialized mip levels. Conflicting
    /// revisions/descriptions are rejected. The runtime checks freshness again
    /// at submission. This operation neither waits for GPU completion nor
    /// transfers the caller's runtime registration ownership.
    pub fn import_image(
        &mut self,
        name: &str,
        source: &ImportImage,
    ) -> Result<ImageHandle, GraphError> {
        let whole = 0..ResourceDesc::Image(source.desc).units();
        let mut ranges = [const { 0..0 }; 16];
        let initial = if source.initialized() {
            std::slice::from_ref(&whole)
        } else if source.partial_mips != 0 {
            let mut count = 0;
            for r in mip_ranges(source.partial_mips) {
                ranges[count] = r;
                count += 1;
            }
            &ranges[..count]
        } else {
            &[]
        };
        let index = self.import(
            name,
            ResourceDesc::Image(source.desc),
            source.key,
            source.revision(),
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
        if let Some(&index) = self.imports.get(&key) {
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
        // Only private checked snapshot constructors can reach this path.
        // Their descriptor/ranges stay frozen for every token and clone; a
        // changed runtime revision issues another snapshot and is checked again
        // at submission. Repeated-import consistency above remains dynamic.
        let index = self.add_resource(name, desc, Some(key), revision, initial)?;
        // A new key owns one resource. add_resource enforces the prepared
        // resource limit, and the table reserved that full limit at creation;
        // insertion therefore cannot grow it on a frame path.
        self.data_mut().imports.insert(key, index);
        Ok(index)
    }
    /// Keep this buffer's final contents observable outside the graph.
    ///
    /// Compilation requires every byte to be initialized and retains its
    /// necessary producers. Repeated export is idempotent. Export does not
    /// submit, wait, read back bytes, or transfer an imported registration's
    /// ownership. Read-only snapshot mesh inputs must first be copied into an
    /// independent buffer rather than exported directly.
    pub fn export_buffer(&mut self, buffer: BufferHandle) -> Result<(), GraphError> {
        let id = self.buffer_id(buffer)?;
        if self.resources[id].read_only_input {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "snapshot mesh inputs cannot be exported; copy to an independent buffer",
                id,
            ));
        }
        self.export(id);
        Ok(())
    }
    /// Keep this image's final contents observable outside the graph.
    ///
    /// Compilation requires all mip/layer subresources to have valid contents
    /// and retains the necessary producers. Repeated export is idempotent.
    /// Native registration/result retrieval is a separate runtime operation;
    /// this declaration neither submits nor waits, and an imported image's
    /// registration remains owned by its caller.
    pub fn export_image(&mut self, image: ImageHandle) -> Result<(), GraphError> {
        let id = self.image_id(image)?;
        self.single_sample_image(id)?;
        self.data_mut().resources[id].export_mips = 0;
        self.export(id);
        Ok(())
    }
    /// Publish just these initialized mip levels of a single-layer image. Other
    /// mips remain unreadable until initialized. Repeated declarations accumulate
    /// mip levels; a whole-image export requires all mips.
    pub fn export_image_mips(
        &mut self,
        image: ImageHandle,
        levels: Range<u32>,
    ) -> Result<(), GraphError> {
        let id = self.image_id(image)?;
        self.single_sample_image(id)?;
        let desc = self.image_desc_at(id)?;
        if desc.array_layers != 1 || levels.is_empty() || levels.end > desc.mip_levels {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "invalid single-layer image mip export",
                id,
            ));
        }
        if !self.exported[id] || self.resources[id].export_mips != 0 {
            self.data_mut().resources[id].export_mips |= mip_mask(levels);
        }
        self.export(id);
        Ok(())
    }
    /// Export one image whose registration the common Runtime manages for this
    /// frame. Its submission receipt holds the image until taken or dropped.
    /// An imported target keeps its original registration owner.
    pub fn export_frame_image(&mut self, image: ImageHandle) -> Result<(), GraphError> {
        if self.frame_image.is_some_and(|selected| selected != image) {
            return Err(GraphError::invalid_input(
                "a frame image is already selected",
            ));
        }
        self.export_image(image)?;
        self.data_mut().frame_image = Some(image);
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

    /// Restrict a captured immutable mesh to reads. Only the private upload
    /// connection may initialize its newly-created transient before protection.
    pub(crate) fn protect_input(
        &mut self,
        buffer: BufferHandle,
        created_here: bool,
    ) -> Result<(), GraphError> {
        let id = self.buffer_id(buffer)?;
        if self.resources[id].read_only_input {
            return Ok(());
        }
        if self.exported[id]
            || (!created_here
                && self
                    .accesses
                    .iter()
                    .any(|access| access.resource == id && access.write))
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "snapshot mesh input was already exported or written",
                id,
            ));
        }
        self.data_mut().resources[id].read_only_input = true;
        Ok(())
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
    /// Fixed whole-image operations currently encode only one mip/layer.
    pub(super) fn single_image(&self, id: usize) -> Result<(), GraphError> {
        self.single_sample_image(id)?;
        if !self.image_desc_at(id)?.is_single_subresource() {
            return Err(self.resource_error(
                GraphErrorKind::Unsupported,
                "operation requires a single-mip single-layer image",
                id,
            ));
        }
        Ok(())
    }

    pub(super) fn single_sample_image(&self, id: usize) -> Result<(), GraphError> {
        if self.image_desc_at(id)?.sample_count != SampleCount::X1 {
            return Err(self.resource_error(
                GraphErrorKind::Unsupported,
                "operation requires a single-sample image",
                id,
            ));
        }
        Ok(())
    }
    pub(super) fn compatible_image_copy_pair(
        &self,
        src: usize,
        dst: usize,
    ) -> Result<ImageDesc, GraphError> {
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
        Ok(source)
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

    const SNAPSHOT_KEY: ResourceKey = ResourceKey {
        owner: 3,
        index: 2,
        generation: 1,
    };

    #[test]
    fn unsorted_imports_keep_identity_at_capacity_and_across_pool_reuse() {
        let mut ranges = super::super::ImportRangesPool::new(1, 1).unwrap();
        let initialized = ranges.capture(std::slice::from_ref(&(0..4))).unwrap();
        let sources = [17, 3, 11, 5].map(|index| {
            ImportBuffer::new(
                ResourceKey {
                    index,
                    ..SNAPSHOT_KEY
                },
                1,
                BufferDesc { size: 4 },
                initialized.clone(),
            )
            .unwrap()
        });
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: 3,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut previous = None;
        for _ in 0..3 {
            let mut graph = pool.begin().unwrap();
            let handles =
                [0, 1, 2].map(|index| graph.import_buffer("input", &sources[index]).unwrap());
            if let Some(old) = previous {
                assert_eq!(
                    graph.export_buffer(old).unwrap_err().kind(),
                    GraphErrorKind::WrongOwner
                );
            }
            for index in (0..3).rev() {
                assert_eq!(
                    graph.import_buffer("alias", &sources[index]).unwrap(),
                    handles[index]
                );
            }
            let error = graph.import_buffer("overflow", &sources[3]).unwrap_err();
            assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::Resources);
            let changed =
                ImportBuffer::new(sources[0].key(), 2, sources[0].desc(), initialized.clone())
                    .unwrap();
            assert_eq!(
                graph
                    .import_buffer("stale alias", &changed)
                    .unwrap_err()
                    .kind(),
                GraphErrorKind::InconsistentImport
            );
            assert_eq!(
                graph.import_buffer("still valid", &sources[0]).unwrap(),
                handles[0]
            );
            previous = Some(handles[0]);
        }
    }

    #[test]
    fn snapshots_reject_invalid_descriptions_and_noncanonical_ranges_at_issuance() {
        let mut ranges = super::super::ImportRangesPool::new(2, 1).unwrap();
        assert_eq!(
            ImportBuffer::new(
                SNAPSHOT_KEY,
                1,
                BufferDesc { size: 0 },
                ranges.capture(&[]).unwrap()
            )
            .unwrap_err()
            .kind(),
            GraphErrorKind::InvalidInput
        );
        for invalid in [
            std::slice::from_ref(&(0..17)),
            std::slice::from_ref(&(4..4)),
            &[8..12, 0..4],
            &[0..8, 4..12],
        ] {
            let token = ranges.capture(invalid).unwrap();
            assert_eq!(
                ImportBuffer::new(SNAPSHOT_KEY, 1, BufferDesc { size: 16 }, token)
                    .unwrap_err()
                    .kind(),
                GraphErrorKind::InvalidInput
            );
            assert!(
                ranges.is_idle(),
                "rejected issuance returns its pooled token"
            );
        }
        for desc in [
            ImageDesc::rgba8(0, 1),
            ImageDesc {
                mip_levels: 0,
                ..ImageDesc::rgba8(1, 1)
            },
            ImageDesc {
                array_layers: 0,
                ..ImageDesc::rgba8(1, 1)
            },
            ImageDesc::rgba8(u32::MAX, u32::MAX),
        ] {
            assert_eq!(
                ImportImage::new(SNAPSHOT_KEY, 1, desc, &[])
                    .unwrap_err()
                    .kind(),
                GraphErrorKind::InvalidInput
            );
        }
        assert_eq!(
            ImportImage::new(
                SNAPSHOT_KEY,
                1,
                ImageDesc::rgba8(1, 1),
                std::slice::from_ref(&(0..2))
            )
            .unwrap_err()
            .kind(),
            GraphErrorKind::InvalidInput
        );
    }

    #[test]
    fn image_snapshot_derives_whole_mip_layer_coverage_without_claiming_holes() {
        let desc = ImageDesc {
            mip_levels: 3,
            array_layers: 2,
            ..ImageDesc::rgba8(4, 2)
        };
        for partial in [
            &[][..],
            std::slice::from_ref(&(0..3)),
            &[0..2, 3..6],
            std::slice::from_ref(&(1..6)),
        ] {
            let snapshot = ImportImage::new(SNAPSHOT_KEY, 1, desc, partial).unwrap();
            assert!(!snapshot.initialized());
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            let mut graph = pool.begin().unwrap();
            let image = graph.import_image("incomplete image", &snapshot).unwrap();
            graph.export_image(image).unwrap();
            assert_eq!(
                graph.compile().unwrap_err().kind(),
                GraphErrorKind::UninitializedRead
            );
        }
        let snapshot = ImportImage::new(SNAPSHOT_KEY, 1, desc, &[0..2, 2..6]).unwrap();
        assert!(snapshot.initialized());
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let image = graph.import_image("complete image", &snapshot).unwrap();
        graph.export_image(image).unwrap();
        assert!(graph.compile().is_ok());
    }

    #[test]
    fn frozen_snapshot_clones_survive_range_slot_reuse_and_new_revision_issuance() {
        let mut ranges = super::super::ImportRangesPool::new(1, 1).unwrap();
        let source = ImportBuffer::new(
            SNAPSHOT_KEY,
            7,
            BufferDesc { size: 8 },
            ranges.capture(std::slice::from_ref(&(0..4))).unwrap(),
        )
        .unwrap();
        let retained = source.clone();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let buffer = graph.import_buffer("frozen source", &source).unwrap();
        assert_eq!(
            graph
                .import_buffer("same immutable source", &retained)
                .unwrap(),
            buffer
        );
        drop(source);
        assert!(ranges.capture(&[]).is_err());
        assert_eq!(retained.revision(), 7);
        assert_eq!(
            retained.initialized().as_slice(),
            std::slice::from_ref(&(0..4))
        );
        drop(retained);
        let changed = ImportBuffer::new(
            SNAPSHOT_KEY,
            8,
            BufferDesc { size: 8 },
            ranges.capture(std::slice::from_ref(&(4..8))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            graph
                .import_buffer("changed revision", &changed)
                .unwrap_err()
                .kind(),
            GraphErrorKind::InconsistentImport
        );
        // Graph captured the old contents independently before the pooled token
        // returned. Reusing the token cannot rewrite this immutable plan input.
        graph
            .readback_buffer("old known prefix", buffer, 0, 4)
            .unwrap();
        assert!(graph.compile().is_ok());
    }

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
                &ImportImage::test_initialized(
                    ResourceKey {
                        owner: 7,
                        index: 0,
                        generation: 1,
                    },
                    3,
                    desc,
                ),
            )
            .unwrap();
        graph.export_image(image).unwrap();
        assert_eq!(graph.compile().unwrap().pass_count(), 0);
    }
}
