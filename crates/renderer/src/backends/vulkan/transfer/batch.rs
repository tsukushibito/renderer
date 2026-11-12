//! Ordered copy work retaining resources and shared staging ranges until completion.

use std::collections::HashMap;
use std::error::Error;
use std::ffi::CStr;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ash::vk;

use super::super::buffer::{BufferRole, InitializedRanges, VulkanBuffer};
use super::super::image::{ImageDesc, VulkanImage};
use super::super::memory::MemoryOwner;
use super::staging::{SharedStaging, StagingError, StagingRange, StagingStats};
use super::{
    ExecutionPool, ExecutionPoolStats, ExecutionReservation, RecordedTransfer, TransferError,
    TransferPayload, record_host_read,
};

static NEXT_BATCH: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(in crate::backends::vulkan) enum BatchError {
    Invalid(&'static str),
    Execution(TransferError),
    Staging(StagingError),
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid copy batch: {reason}"),
            Self::Execution(error) => error.fmt(f),
            Self::Staging(error) => error.fmt(f),
        }
    }
}

impl Error for BatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(_) => None,
            Self::Execution(error) => Some(error),
            Self::Staging(error) => Some(error),
        }
    }
}

impl From<TransferError> for BatchError {
    fn from(error: TransferError) -> Self {
        Self::Execution(error)
    }
}

impl From<StagingError> for BatchError {
    fn from(error: StagingError) -> Self {
        Self::Staging(error)
    }
}

macro_rules! resource_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(in crate::backends::vulkan) struct $name {
            batch: u64,
            index: usize,
        }
    };
}

resource_id!(BufferId);
resource_id!(ImageId);
resource_id!(ReadbackId);

/// A bounded execution pool and two independently bounded shared staging buffers.
/// Configuration is explicit: no per-submit growth or implicit GPU wait occurs.
pub(in crate::backends::vulkan) struct TransferPool {
    owner: Arc<MemoryOwner>,
    execution: ExecutionPool,
    upload: SharedStaging,
    readback: SharedStaging,
}

impl TransferPool {
    pub(in crate::backends::vulkan) fn new(
        owner: Arc<MemoryOwner>,
        max_slots: usize,
        upload_capacity: u64,
        readback_capacity: u64,
    ) -> Result<Self, BatchError> {
        Ok(Self {
            execution: ExecutionPool::new(Arc::clone(&owner), max_slots)?,
            upload: SharedStaging::new(Arc::clone(&owner), BufferRole::Upload, upload_capacity)?,
            readback: SharedStaging::new(
                Arc::clone(&owner),
                BufferRole::Readback,
                readback_capacity,
            )?,
            owner,
        })
    }

    pub(in crate::backends::vulkan) fn begin(&self) -> Result<BatchBuilder, BatchError> {
        // Reserve native capacity before accepting any caller-owned resource.
        let reservation = self.execution.reserve()?;
        let id = NEXT_BATCH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| BatchError::Invalid("batch identifier space exhausted"))?;
        Ok(BatchBuilder {
            reservation,
            upload: self.upload.clone(),
            readback: self.readback.clone(),
            work: TransferBatch {
                owner: Arc::clone(&self.owner),
                id,
                buffers: Vec::new(),
                images: Vec::new(),
                uploads: Vec::new(),
                readbacks: Vec::new(),
                commands: Vec::new(),
                hazards: BufferHazards::default(),
                initial_buffer_dependency: false,
                bytes: 0,
                complete: false,
            },
        })
    }

    pub(in crate::backends::vulkan) fn execution_stats(&self) -> ExecutionPoolStats {
        self.execution.stats()
    }

    pub(in crate::backends::vulkan) fn staging_stats(&self) -> (StagingStats, StagingStats) {
        (self.upload.stats(), self.readback.stats())
    }
}

struct BufferState {
    value: Option<VulkanBuffer>,
    initialized: InitializedRanges,
    written: Vec<Range<u64>>,
    prior_contents: bool,
}

#[derive(Clone)]
struct BufferAccess {
    resource: usize,
    range: Range<u64>,
    write: bool,
}

/// Only copy hazards within this batch, not a second RenderGraph. Independent
/// resources are indexed separately; a global dependency retires the whole set.
#[derive(Default)]
struct BufferHazards(HashMap<usize, Vec<BufferAccess>>);

impl BufferHazards {
    fn before(&mut self, accesses: &[BufferAccess]) -> bool {
        let needed = accesses.iter().any(|next| {
            self.0.get(&next.resource).is_some_and(|earlier| {
                earlier.iter().any(|prior| {
                    (prior.write || next.write)
                        && prior.range.start < next.range.end
                        && next.range.start < prior.range.end
                })
            })
        });
        if needed {
            self.0.clear();
        }
        for access in accesses {
            self.0
                .entry(access.resource)
                .or_default()
                .push(access.clone());
        }
        needed
    }
}

struct ImageState {
    value: Option<VulkanImage>,
    layout: vk::ImageLayout,
    initialized: bool,
    touched: bool,
}

enum Copy {
    UploadBuffer {
        range: usize,
        target: usize,
        offset: u64,
        barrier_before: bool,
    },
    Buffer {
        source: usize,
        source_offset: u64,
        target: usize,
        target_offset: u64,
        size: u64,
        barrier_before: bool,
    },
    ReadBuffer {
        source: usize,
        offset: u64,
        range: usize,
        barrier_before: bool,
    },
    UploadImage {
        range: usize,
        target: usize,
        old_layout: vk::ImageLayout,
    },
    ReadImage {
        source: usize,
        range: usize,
        old_layout: vk::ImageLayout,
    },
}

/// Builder ownership excludes all target CPU/native access while planning copies.
pub(in crate::backends::vulkan) struct BatchBuilder {
    // Native reservation drops before owned resource payload on cancellation.
    reservation: ExecutionReservation,
    upload: SharedStaging,
    readback: SharedStaging,
    work: TransferBatch,
}

impl BatchBuilder {
    pub(in crate::backends::vulkan) fn add_buffer(
        &mut self,
        buffer: VulkanBuffer,
    ) -> Result<BufferId, BatchError> {
        if !Arc::ptr_eq(buffer.owner(), &self.work.owner)
            || buffer.role() != BufferRole::DeviceTransfer
        {
            return Err(BatchError::Invalid(
                "target buffer must be DeviceTransfer on the same owner",
            ));
        }
        let id = BufferId {
            batch: self.work.id,
            index: self.work.buffers.len(),
        };
        let initialized = buffer.initialized_ranges();
        self.work.buffers.push(BufferState {
            prior_contents: !initialized.is_empty(),
            initialized,
            value: Some(buffer),
            written: Vec::new(),
        });
        Ok(id)
    }

    pub(in crate::backends::vulkan) fn add_image(
        &mut self,
        image: VulkanImage,
    ) -> Result<ImageId, BatchError> {
        if !Arc::ptr_eq(image.owner(), &self.work.owner) {
            return Err(BatchError::Invalid(
                "target image must belong to the same owner",
            ));
        }
        let desc = image.desc();
        if desc != ImageDesc::transfer(desc.extent.width, desc.extent.height) {
            return Err(BatchError::Invalid(
                "copy images must be transfer-only RGBA8_UNORM",
            ));
        }
        let layout = image.layout();
        if !supported_layout(layout) {
            return Err(BatchError::Invalid("unsupported completed image layout"));
        }
        let id = ImageId {
            batch: self.work.id,
            index: self.work.images.len(),
        };
        self.work.images.push(ImageState {
            value: Some(image),
            layout,
            initialized: layout != vk::ImageLayout::UNDEFINED,
            touched: false,
        });
        Ok(id)
    }

    pub(in crate::backends::vulkan) fn upload_buffer(
        &mut self,
        target: BufferId,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), BatchError> {
        let target = check_id(
            self.work.id,
            target.batch,
            target.index,
            self.work.buffers.len(),
        )?;
        let size = bytes.len() as u64;
        let range = copy_range(self.work.buffer(target).size(), offset, size)?;
        let mut staging = self.upload.allocate(size)?;
        staging.write(bytes)?;
        let index = self.work.uploads.len();
        self.work.uploads.push(staging);
        let barrier_before = self.work.buffer_barrier(&[BufferAccess {
            resource: target,
            range: range.clone(),
            write: true,
        }]);
        self.work.commands.push(Copy::UploadBuffer {
            range: index,
            target,
            offset,
            barrier_before,
        });
        self.work.buffers[target].initialized.insert(range.clone());
        self.work.buffers[target].written.push(range);
        self.work.bytes = self.work.bytes.saturating_add(size);
        Ok(())
    }

    pub(in crate::backends::vulkan) fn copy_buffer(
        &mut self,
        source: BufferId,
        source_offset: u64,
        target: BufferId,
        target_offset: u64,
        size: u64,
    ) -> Result<(), BatchError> {
        let source = check_id(
            self.work.id,
            source.batch,
            source.index,
            self.work.buffers.len(),
        )?;
        let target = check_id(
            self.work.id,
            target.batch,
            target.index,
            self.work.buffers.len(),
        )?;
        let source_range = copy_range(self.work.buffer(source).size(), source_offset, size)?;
        let target_range = copy_range(self.work.buffer(target).size(), target_offset, size)?;
        if source == target
            && source_range.start < target_range.end
            && target_range.start < source_range.end
        {
            return Err(BatchError::Invalid("same-buffer copy regions overlap"));
        }
        if !self.work.buffers[source]
            .initialized
            .contains(&source_range)
        {
            return Err(BatchError::Invalid("copy source range is uninitialized"));
        }
        let barrier_before = self.work.buffer_barrier(&[
            BufferAccess {
                resource: source,
                range: source_range,
                write: false,
            },
            BufferAccess {
                resource: target,
                range: target_range.clone(),
                write: true,
            },
        ]);
        self.work.commands.push(Copy::Buffer {
            source,
            source_offset,
            target,
            target_offset,
            size,
            barrier_before,
        });
        self.work.buffers[target]
            .initialized
            .insert(target_range.clone());
        self.work.buffers[target].written.push(target_range);
        self.work.bytes = self.work.bytes.saturating_add(size);
        Ok(())
    }

    pub(in crate::backends::vulkan) fn readback_buffer(
        &mut self,
        source: BufferId,
        offset: u64,
        size: u64,
    ) -> Result<ReadbackId, BatchError> {
        let source = check_id(
            self.work.id,
            source.batch,
            source.index,
            self.work.buffers.len(),
        )?;
        let range = copy_range(self.work.buffer(source).size(), offset, size)?;
        if !self.work.buffers[source].initialized.contains(&range) {
            return Err(BatchError::Invalid(
                "readback source range is uninitialized",
            ));
        }
        let staging = self.readback.allocate(size)?;
        let id = ReadbackId {
            batch: self.work.id,
            index: self.work.readbacks.len(),
        };
        self.work.readbacks.push(Some(staging));
        let barrier_before = self.work.buffer_barrier(&[BufferAccess {
            resource: source,
            range,
            write: false,
        }]);
        self.work.commands.push(Copy::ReadBuffer {
            source,
            offset,
            range: id.index,
            barrier_before,
        });
        self.work.bytes = self.work.bytes.saturating_add(size);
        Ok(id)
    }

    pub(in crate::backends::vulkan) fn upload_image(
        &mut self,
        target: ImageId,
        bytes: &[u8],
    ) -> Result<(), BatchError> {
        let target = check_id(
            self.work.id,
            target.batch,
            target.index,
            self.work.images.len(),
        )?;
        let size = image_bytes(self.work.image(target))?;
        if bytes.len() as u64 != size {
            return Err(BatchError::Invalid(
                "image upload must be a whole tightly packed RGBA8 image",
            ));
        }
        let mut staging = self.upload.allocate(size)?;
        staging.write(bytes)?;
        let index = self.work.uploads.len();
        self.work.uploads.push(staging);
        self.work.commands.push(Copy::UploadImage {
            range: index,
            target,
            old_layout: self.work.images[target].layout,
        });
        let state = &mut self.work.images[target];
        state.layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
        state.initialized = true;
        state.touched = true;
        self.work.bytes = self.work.bytes.saturating_add(size);
        Ok(())
    }

    pub(in crate::backends::vulkan) fn readback_image(
        &mut self,
        source: ImageId,
    ) -> Result<ReadbackId, BatchError> {
        let source = check_id(
            self.work.id,
            source.batch,
            source.index,
            self.work.images.len(),
        )?;
        if !self.work.images[source].initialized {
            return Err(BatchError::Invalid("readback image is uninitialized"));
        }
        let size = image_bytes(self.work.image(source))?;
        let staging = self.readback.allocate(size)?;
        let id = ReadbackId {
            batch: self.work.id,
            index: self.work.readbacks.len(),
        };
        self.work.readbacks.push(Some(staging));
        self.work.commands.push(Copy::ReadImage {
            source,
            range: id.index,
            old_layout: self.work.images[source].layout,
        });
        let state = &mut self.work.images[source];
        state.layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
        state.touched = true;
        self.work.bytes = self.work.bytes.saturating_add(size);
        Ok(id)
    }

    pub(in crate::backends::vulkan) fn record(
        mut self,
    ) -> Result<RecordedTransfer<TransferBatch>, BatchError> {
        // Per-command decisions are now fixed. The temporary hazard index is
        // unnecessary while GPU work or CPU-held readback results stay alive.
        self.work.hazards = BufferHazards::default();
        Ok(self.reservation.record(self.work)?)
    }
}

/// On completion Upload ranges are returned immediately. Readback ranges remain
/// owned until read successfully or this result is dropped, even if native slots
/// have already been recycled for another batch.
pub(in crate::backends::vulkan) struct TransferBatch {
    owner: Arc<MemoryOwner>,
    id: u64,
    buffers: Vec<BufferState>,
    images: Vec<ImageState>,
    uploads: Vec<StagingRange>,
    readbacks: Vec<Option<StagingRange>>,
    commands: Vec<Copy>,
    hazards: BufferHazards,
    initial_buffer_dependency: bool,
    bytes: u64,
    complete: bool,
}

impl TransferBatch {
    fn buffer_barrier(&mut self, accesses: &[BufferAccess]) -> bool {
        self.initial_buffer_dependency |= accesses
            .iter()
            .any(|access| self.buffers[access.resource].prior_contents);
        self.hazards.before(accesses)
    }

    fn buffer(&self, index: usize) -> &VulkanBuffer {
        self.buffers[index]
            .value
            .as_ref()
            .expect("retained batch buffer")
    }

    fn image(&self, index: usize) -> &VulkanImage {
        self.images[index]
            .value
            .as_ref()
            .expect("retained batch image")
    }

    pub(in crate::backends::vulkan) fn readback(
        &mut self,
        id: ReadbackId,
        output: &mut [u8],
    ) -> Result<(), BatchError> {
        self.require_complete()?;
        let index = check_id(self.id, id.batch, id.index, self.readbacks.len())?;
        let range = self.readbacks[index]
            .as_mut()
            .ok_or(BatchError::Invalid("readback was already released"))?;
        range.read(output)?;
        self.readbacks[index].take();
        Ok(())
    }

    pub(in crate::backends::vulkan) fn take_buffer(
        &mut self,
        id: BufferId,
    ) -> Result<VulkanBuffer, BatchError> {
        self.require_complete()?;
        let index = check_id(self.id, id.batch, id.index, self.buffers.len())?;
        self.buffers[index]
            .value
            .take()
            .ok_or(BatchError::Invalid("buffer was already taken"))
    }

    pub(in crate::backends::vulkan) fn take_image(
        &mut self,
        id: ImageId,
    ) -> Result<VulkanImage, BatchError> {
        self.require_complete()?;
        let index = check_id(self.id, id.batch, id.index, self.images.len())?;
        self.images[index]
            .value
            .take()
            .ok_or(BatchError::Invalid("image was already taken"))
    }

    fn require_complete(&self) -> Result<(), BatchError> {
        if self.complete {
            Ok(())
        } else {
            Err(BatchError::Invalid("batch has not completed"))
        }
    }
}

impl TransferPayload for TransferBatch {
    fn validate(&self) -> Result<(), TransferError> {
        if self.complete || self.commands.is_empty() {
            return Err(TransferError::InvalidBuffers(
                "batch must contain unfinished copy commands",
            ));
        }
        if self
            .uploads
            .iter()
            .any(|range| !Arc::ptr_eq(range.owner(), &self.owner) || !range.initialized())
            || self
                .readbacks
                .iter()
                .flatten()
                .any(|range| !Arc::ptr_eq(range.owner(), &self.owner))
        {
            return Err(TransferError::InvalidBuffers(
                "staging owner mismatch or uninitialized upload range",
            ));
        }
        self.owner
            .device
            .ensure_active()
            .map_err(TransferError::DeviceUnavailable)
    }

    fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }
    fn bytes(&self) -> u64 {
        self.bytes
    }
    fn label(&self) -> &'static CStr {
        c"renderer.copy-batch"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        let device = &self.owner.device;
        if self.initial_buffer_dependency {
            // SAFETY: Retained buffers may contain prior completed COPY writes.
            // One initial dependency makes those writes available before this
            // batch, without serializing its independent copies with each other.
            unsafe {
                buffer_dependency(device, command);
            }
        }
        for copy in &self.commands {
            match *copy {
                Copy::UploadBuffer {
                    range,
                    target,
                    offset,
                    barrier_before,
                } => {
                    let staging = &self.uploads[range];
                    // SAFETY: Unique target ownership and disjoint staging lease
                    // are retained by this work until the execution Fence retires.
                    unsafe {
                        if barrier_before {
                            buffer_dependency(device, command);
                        }
                        device.raw().cmd_copy_buffer(
                            command,
                            staging.transfer_handle(),
                            self.buffer(target).transfer_handle(),
                            &[vk::BufferCopy::default()
                                .src_offset(staging.offset())
                                .dst_offset(offset)
                                .size(staging.size())],
                        );
                    }
                }
                Copy::Buffer {
                    source,
                    source_offset,
                    target,
                    target_offset,
                    size,
                    barrier_before,
                } => {
                    // SAFETY: Builder verified initialized nonoverlapping bounded
                    // regions; both native resources remain owned by this work.
                    unsafe {
                        if barrier_before {
                            buffer_dependency(device, command);
                        }
                        device.raw().cmd_copy_buffer(
                            command,
                            self.buffer(source).transfer_handle(),
                            self.buffer(target).transfer_handle(),
                            &[vk::BufferCopy::default()
                                .src_offset(source_offset)
                                .dst_offset(target_offset)
                                .size(size)],
                        );
                    }
                }
                Copy::ReadBuffer {
                    source,
                    offset,
                    range,
                    barrier_before,
                } => {
                    let staging = self.readbacks[range]
                        .as_ref()
                        .expect("retained readback range");
                    // SAFETY: Readback destination is an exclusively retained
                    // disjoint range; prior COPY writes become visible here.
                    unsafe {
                        if barrier_before {
                            buffer_dependency(device, command);
                        }
                        device.raw().cmd_copy_buffer(
                            command,
                            self.buffer(source).transfer_handle(),
                            staging.transfer_handle(),
                            &[vk::BufferCopy::default()
                                .src_offset(offset)
                                .dst_offset(staging.offset())
                                .size(staging.size())],
                        );
                    }
                }
                Copy::UploadImage {
                    range,
                    target,
                    old_layout,
                } => {
                    let staging = &self.uploads[range];
                    // SAFETY: Whole initialized RGBA bytes and sole image
                    // subresource are retained, with planned old layout recorded
                    // in command order rather than committed to the image early.
                    unsafe {
                        let image = self.image(target).transfer_handle();
                        image_dependency(
                            device,
                            command,
                            image,
                            old_layout,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        );
                        device.raw().cmd_copy_buffer_to_image(
                            command,
                            staging.transfer_handle(),
                            image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[image_region(self.image(target), staging.offset())],
                        );
                    }
                }
                Copy::ReadImage {
                    source,
                    range,
                    old_layout,
                } => {
                    let staging = self.readbacks[range]
                        .as_ref()
                        .expect("retained readback range");
                    // SAFETY: Whole initialized image and exclusive readback
                    // region remain owned; the transition makes copy writes
                    // available to this read without an intermediate host wait.
                    unsafe {
                        let image = self.image(source).transfer_handle();
                        image_dependency(
                            device,
                            command,
                            image,
                            old_layout,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        );
                        device.raw().cmd_copy_image_to_buffer(
                            command,
                            image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            staging.transfer_handle(),
                            &[image_region(self.image(source), staging.offset())],
                        );
                    }
                }
            }
        }
        if !self.readbacks.is_empty() {
            // SAFETY: This follows every copy writing a retained readback range;
            // caller receives those ranges only after actual successful wait.
            unsafe {
                record_host_read(device, command);
            }
        }
    }

    unsafe fn establish_contents(&mut self) {
        for state in &mut self.buffers {
            for range in &state.written {
                // SAFETY: Actual Fence success on an active Device proves each
                // planned GPU write completed; targets remained uniquely owned.
                unsafe {
                    state
                        .value
                        .as_mut()
                        .unwrap()
                        .establish_gpu_range(range.start, range.end - range.start);
                }
            }
        }
        for state in &mut self.images {
            if state.touched {
                // SAFETY: Whole-image commands and final transition completed;
                // initialization was proved by a prior upload or prior contents.
                unsafe {
                    state
                        .value
                        .as_mut()
                        .unwrap()
                        .establish_completed_layout(state.layout);
                }
            }
        }
        for range in self.readbacks.iter_mut().flatten() {
            // SAFETY: Each retained region was fully GPU-written by this batch.
            unsafe {
                range.establish_contents();
            }
        }
        self.complete = true;
        self.uploads.clear();
    }
}

fn check_id(batch: u64, actual: u64, index: usize, count: usize) -> Result<usize, BatchError> {
    if batch != actual || index >= count {
        Err(BatchError::Invalid(
            "resource identifier belongs to another batch or is out of range",
        ))
    } else {
        Ok(index)
    }
}

fn copy_range(capacity: u64, offset: u64, size: u64) -> Result<Range<u64>, BatchError> {
    let end = offset
        .checked_add(size)
        .ok_or(BatchError::Invalid("copy byte range overflows"))?;
    if size == 0 || !offset.is_multiple_of(4) || end > capacity {
        return Err(BatchError::Invalid(
            "copy range must be nonempty, bounded and have a 4-byte aligned offset",
        ));
    }
    Ok(offset..end)
}

fn image_bytes(image: &VulkanImage) -> Result<u64, BatchError> {
    u64::from(image.desc().extent.width)
        .checked_mul(u64::from(image.desc().extent.height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(BatchError::Invalid("packed image byte count overflows"))
}

fn supported_layout(layout: vk::ImageLayout) -> bool {
    matches!(
        layout,
        vk::ImageLayout::UNDEFINED
            | vk::ImageLayout::TRANSFER_SRC_OPTIMAL
            | vk::ImageLayout::TRANSFER_DST_OPTIMAL
    )
}

unsafe fn buffer_dependency(
    device: &super::super::device::VulkanDevice,
    command: vk::CommandBuffer,
) {
    let barriers = [vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)];
    // SAFETY: Caller owns the recording command and all referenced ranges;
    // dependency covers ordered copy hazards, including preceding submissions.
    unsafe {
        device.raw().cmd_pipeline_barrier2(
            command,
            &vk::DependencyInfo::default().memory_barriers(&barriers),
        );
    }
}

fn image_barrier(
    image: vk::Image,
    old_layout: vk::ImageLayout,
    layout: vk::ImageLayout,
) -> vk::ImageMemoryBarrier2<'static> {
    let access = |layout| match layout {
        vk::ImageLayout::TRANSFER_DST_OPTIMAL => vk::AccessFlags2::TRANSFER_WRITE,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => vk::AccessFlags2::TRANSFER_READ,
        _ => vk::AccessFlags2::NONE,
    };
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(if old_layout == vk::ImageLayout::UNDEFINED {
            vk::PipelineStageFlags2::NONE
        } else {
            vk::PipelineStageFlags2::COPY
        })
        .src_access_mask(access(old_layout))
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(access(layout))
        .old_layout(old_layout)
        .new_layout(layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1),
        )
}

unsafe fn image_dependency(
    device: &super::super::device::VulkanDevice,
    command: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
) {
    let barriers = [image_barrier(image, old, new)];
    // SAFETY: Caller retained the sole color subresource and planned old layout
    // follows its preceding command in this owner or a proven completed batch.
    unsafe {
        device.raw().cmd_pipeline_barrier2(
            command,
            &vk::DependencyInfo::default().image_memory_barriers(&barriers),
        );
    }
}

fn image_region(image: &VulkanImage, offset: u64) -> vk::BufferImageCopy {
    let extent = image.desc().extent;
    vk::BufferImageCopy::default()
        .buffer_offset(offset)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .mip_level(0)
                .base_array_layer(0)
                .layer_count(1),
        )
        .image_offset(vk::Offset3D::default())
        .image_extent(vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        })
}

#[cfg(test)]
mod tests;
