//! Owned, single-queue upload -> device resource -> readback execution.
//!
//! These private paths copy whole buffers or tightly packed RGBA8 images.
//! Recording and pending owners retain every resource; callers regain them after a
//! successful Fence wait. Drop may wait for up to its Vulkan timeout budget.

use std::error::Error;
use std::ffi::CStr;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ash::vk;

use super::buffer::{BufferRole, VulkanBuffer};
use super::device::{DeviceState, VulkanDevice};
use super::image::VulkanImage;
use super::instance::LOG_TARGET;
use super::memory::MemoryOwner;

mod lifecycle;
use lifecycle::Retained;

mod image;

pub(super) mod batch;
pub(super) mod staging;

mod pool;
use pool::NativeLease;

/// Bounded native execution resources, independent of payload resource storage.
pub(super) struct ExecutionPool {
    state: Arc<pool::PoolState>,
}

/// Reserves capacity before a batch consumes caller-owned resource inputs.
pub(super) struct ExecutionReservation {
    native: NativeLease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ExecutionPoolStats {
    pub(super) capacity: usize,
    pub(super) allocated: usize,
    pub(super) leased: usize,
    pub(super) idle: usize,
    pub(super) native_creates: usize,
    pub(super) reuses: usize,
    pub(super) resets: usize,
    pub(super) capacity_exhaustions: usize,
}

const DROP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) enum TransferError {
    InvalidBuffers(&'static str),
    InvalidImage(&'static str),
    InvalidPool(&'static str),
    CapacityExhausted {
        capacity: usize,
    },
    DeviceUnavailable(DeviceState),
    AlreadyFinished,
    Vulkan {
        operation: &'static str,
        device: String,
        result: vk::Result,
    },
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBuffers(reason) => write!(f, "invalid buffer transfer: {reason}"),
            Self::InvalidImage(reason) => write!(f, "invalid image transfer: {reason}"),
            Self::InvalidPool(reason) => write!(f, "invalid execution pool: {reason}"),
            Self::CapacityExhausted { capacity } => {
                write!(f, "execution pool has no free slot (capacity {capacity})")
            }
            Self::DeviceUnavailable(state) => {
                write!(f, "transfer Device is {state:?}; normal use is stopped")
            }
            Self::AlreadyFinished => write!(f, "transfer has already been finished"),
            Self::Vulkan {
                operation,
                device,
                result,
            } => write!(f, "{operation} on {device} failed: {result:?}"),
        }
    }
}

impl Error for TransferError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Vulkan { result, .. } => Some(result),
            _ => None,
        }
    }
}

fn native_error(
    device: &VulkanDevice,
    operation: &'static str,
    result: vk::Result,
) -> TransferError {
    device.observe_result(result);
    TransferError::Vulkan {
        operation,
        device: device.name().to_owned(),
        result,
    }
}

fn timeout_ns(timeout: Duration) -> u64 {
    // Keep even an oversized Duration finite rather than turning it into the
    // Vulkan infinite-timeout sentinel.
    timeout.as_nanos().min((u64::MAX - 1) as u128) as u64
}

/// Shared by recording and optional Graph diagnostics, so the reported scope
/// cannot drift from the actual host-read dependency.
pub(super) fn host_read_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::HOST)
        .dst_access_mask(vk::AccessFlags2::HOST_READ)
}

/// # Safety
/// `command` must be recording on `device`, after the copy writes whose bytes
/// will be read by the host. Its owner must retain them through completion.
pub(super) unsafe fn record_host_read(device: &VulkanDevice, command: vk::CommandBuffer) {
    let barriers = [host_read_barrier()];
    // SAFETY: The caller retains the command/resources and guarantees this
    // dependency follows their copy writes. A later Fence wait precedes CPU read.
    unsafe {
        device.raw().cmd_pipeline_barrier2(
            command,
            &vk::DependencyInfo::default().memory_barriers(&barriers),
        );
    }
}

pub(super) struct TransferBuffers {
    pub(super) upload: VulkanBuffer,
    pub(super) transfer: VulkanBuffer,
    pub(super) readback: VulkanBuffer,
}

pub(super) struct ImageTransfer {
    pub(super) upload: VulkanBuffer,
    pub(super) image: VulkanImage,
    pub(super) readback: VulkanBuffer,
}

#[derive(Clone, Copy)]
struct BufferSpec {
    role: BufferRole,
    size: u64,
    initialized: bool,
}

fn validate_request(specs: [BufferSpec; 3], same_owner: bool) -> Result<(), TransferError> {
    if !same_owner {
        return Err(TransferError::InvalidBuffers(
            "buffers must belong to the same Allocator and Device",
        ));
    }
    if specs
        .iter()
        .zip([
            BufferRole::Upload,
            BufferRole::DeviceTransfer,
            BufferRole::Readback,
        ])
        .any(|(spec, role)| spec.role != role)
    {
        return Err(TransferError::InvalidBuffers(
            "expected Upload, DeviceTransfer, Readback roles",
        ));
    }
    if specs[0].size == 0 || specs.iter().any(|spec| spec.size != specs[0].size) {
        return Err(TransferError::InvalidBuffers(
            "whole-buffer copies require equal nonzero sizes",
        ));
    }
    if !specs[0].initialized {
        return Err(TransferError::InvalidBuffers(
            "Upload contents are not fully initialized",
        ));
    }
    Ok(())
}

impl TransferBuffers {
    fn validate(&self) -> Result<(), TransferError> {
        let buffers = [&self.upload, &self.transfer, &self.readback];
        validate_request(
            buffers.map(|buffer| BufferSpec {
                role: buffer.role(),
                size: buffer.size(),
                initialized: buffer.fully_initialized(),
            }),
            Arc::ptr_eq(self.upload.owner(), self.transfer.owner())
                && Arc::ptr_eq(self.upload.owner(), self.readback.owner()),
        )
    }
}

/// Private work items share execution and retirement, without a public encoder.
/// Each payload owns all resources its commands reference.
pub(super) trait TransferPayload {
    fn validate(&self) -> Result<(), TransferError>;
    fn owner(&self) -> &Arc<MemoryOwner>;
    fn bytes(&self) -> u64;
    fn label(&self) -> &'static CStr;

    /// # Safety
    /// The validated payload is exclusively retained through recording and any
    /// subsequent GPU execution. `command` is recording on its owner's Device.
    unsafe fn record(&self, command: vk::CommandBuffer);

    /// Publish precomputed scheduled state immediately after native acceptance,
    /// before logging or returning control to a caller. Implementations must
    /// not invoke application code, allocate, or panic. This is not completion.
    fn submitted(&mut self) {}

    /// # Safety
    /// An actual successful Fence wait proved all commands completed, and the
    /// Device remains active. No error or timeout establishes valid contents.
    unsafe fn establish_contents(&mut self);

    /// Optional completion bookkeeping after the native execution lease has
    /// been returned/reset. This does not run for failed waits or Drop cleanup.
    fn retired(&mut self) {}
}

impl TransferPayload for TransferBuffers {
    fn validate(&self) -> Result<(), TransferError> {
        self.validate()
    }

    fn owner(&self) -> &Arc<MemoryOwner> {
        self.upload.owner()
    }

    fn bytes(&self) -> u64 {
        self.upload.size()
    }

    fn label(&self) -> &'static CStr {
        c"renderer upload -> device -> readback"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        let device = &self.owner().device;
        let copy = [vk::BufferCopy::default().size(self.bytes())];
        let between = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
        // SAFETY: The caller retains the validated, unaliased buffers through
        // execution. Source write/flush precedes submit; the COPY dependency
        // makes the first copy's writes visible to the second.
        unsafe {
            device.raw().cmd_copy_buffer(
                command,
                self.upload.transfer_handle(),
                self.transfer.transfer_handle(),
                &copy,
            );
            device.raw().cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().memory_barriers(&between),
            );
            device.raw().cmd_copy_buffer(
                command,
                self.transfer.transfer_handle(),
                self.readback.transfer_handle(),
                &copy,
            );
            record_host_read(device, command);
        }
    }

    unsafe fn establish_contents(&mut self) {
        // SAFETY: The caller proved both full-buffer copies completed on the
        // active Device with the Fence associated with this retained payload.
        unsafe {
            self.transfer.establish_gpu_contents();
            self.readback.establish_gpu_contents();
        }
    }
}

/// Native children are destroyed before the resources they referenced. This Drop
/// is reachable only before submit, after completion, or after Device loss.
struct NativeTransfer {
    owner: Arc<MemoryOwner>,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
}

impl NativeTransfer {
    fn new(owner: Arc<MemoryOwner>) -> Result<Self, TransferError> {
        let mut native = Self {
            owner,
            pool: vk::CommandPool::null(),
            command: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
        };
        let device = &native.owner.device;
        // SAFETY: This independent pool belongs to the selected Device/family,
        // and this owner exclusively synchronizes all of its host operations.
        native.pool = unsafe {
            device.raw().create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(device.queue_family())
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|result| native_error(device, "vkCreateCommandPool", result))?;
        // SAFETY: Live, uniquely owned pool; ash manages the output storage.
        native.command = unsafe {
            device.raw().allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(native.pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|result| native_error(device, "vkAllocateCommandBuffers", result))?[0];
        // SAFETY: Independent unsignaled Fence, immediately retained by owner.
        native.fence = unsafe {
            device
                .raw()
                .create_fence(&vk::FenceCreateInfo::default(), None)
        }
        .map_err(|result| native_error(device, "vkCreateFence", result))?;
        native.name(native.pool, c"renderer.transfer.pool")?;
        native.name(native.command, c"renderer.transfer.commands")?;
        native.name(native.fence, c"renderer.transfer.completion")?;
        Ok(native)
    }

    fn name<T: vk::Handle>(&self, handle: T, name: &CStr) -> Result<(), TransferError> {
        let device = &self.owner.device;
        // SAFETY: Every named object belongs to this live Device; command/pool/
        // fence are uniquely owned. The Device names its Queue only once.
        unsafe {
            device.debug_utils().set_debug_utils_object_name(
                &vk::DebugUtilsObjectNameInfoEXT::default()
                    .object_handle(handle)
                    .object_name(name),
            )
        }
        .map_err(|result| native_error(device, "vkSetDebugUtilsObjectNameEXT", result))
    }

    fn record(&self, payload: &impl TransferPayload) -> Result<(), TransferError> {
        let device = &self.owner.device;
        // SAFETY: Initially allocated or reset, uniquely owned primary command buffer.
        unsafe {
            device.raw().begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|result| native_error(device, "vkBeginCommandBuffer", result))?;
        let debug = device.debug_utils();
        // SAFETY: The validated payload owns every referenced resource on this
        // Device and is retained by the enclosing recording owner. Source
        // write/flush precedes submit. The payload records dependencies for its
        // actual consumers, including a HOST dependency only for readback.
        unsafe {
            debug.cmd_begin_debug_utils_label(
                self.command,
                &vk::DebugUtilsLabelEXT::default()
                    .label_name(payload.label())
                    .color([0.2, 0.7, 1.0, 1.0]),
            );
            payload.record(self.command);
            debug.cmd_end_debug_utils_label(self.command);
        }
        // SAFETY: The unique command buffer has balanced command labels and no
        // render pass; all referenced resources remain in the recording owner.
        unsafe { device.raw().end_command_buffer(self.command) }
            .map_err(|result| native_error(device, "vkEndCommandBuffer", result))
    }

    /// Called only after this slot is unsubmitted or its Fence actually retired.
    fn reset(&self) -> Result<(), TransferError> {
        let device = &self.owner.device;
        // SAFETY: The enclosing lease is uniquely owned and no command from
        // this pool is pending. Reset before payload resources can be released.
        unsafe {
            device
                .raw()
                .reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
        }
        .map_err(|result| native_error(device, "vkResetCommandPool", result))?;
        // SAFETY: The same retirement proof excludes a pending Fence operation.
        // An unsubmitted unsignaled Fence can also be reset before reuse.
        unsafe { device.raw().reset_fences(&[self.fence]) }
            .map_err(|result| native_error(device, "vkResetFences", result))
    }
}

impl Drop for NativeTransfer {
    fn drop(&mut self) {
        // SAFETY: The enclosing retained owner only permits destruction before
        // submission, after proven completion, or under lost-device retirement.
        // Unresolved pending owners leak the whole resource chain instead.
        unsafe {
            if self.fence != vk::Fence::null() {
                self.owner.device.raw().destroy_fence(self.fence, None);
            }
            if self.pool != vk::CommandPool::null() {
                self.owner
                    .device
                    .raw()
                    .destroy_command_pool(self.pool, None);
            }
        }
    }
}

struct TransferResources<T> {
    native: NativeLease,
    payload: T,
}

/// Dropping this unsubmitted owner cancels the recording and releases resources.
pub(super) struct RecordedTransfer<T: TransferPayload = TransferBuffers> {
    resources: TransferResources<T>,
}

impl<T: TransferPayload> RecordedTransfer<T> {
    /// Reference path for acceptance and performance comparisons. Normal callers
    /// reserve reusable execution resources through `ExecutionPool`.
    #[cfg(test)]
    pub(super) fn new(payload: T) -> Result<Self, TransferError> {
        let result = (|| {
            payload.validate()?;
            payload
                .owner()
                .device
                .ensure_active()
                .map_err(TransferError::DeviceUnavailable)?;
            let native = NativeLease::standalone(NativeTransfer::new(Arc::clone(payload.owner()))?);
            Self::with_native(payload, native)
        })();
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan transfer recording failed");
        }
        result
    }

    fn with_native(payload: T, native: NativeLease) -> Result<Self, TransferError> {
        let recorded = Self {
            resources: TransferResources { native, payload },
        };
        recorded
            .resources
            .native
            .record(&recorded.resources.payload)?;
        Ok(recorded)
    }

    pub(super) fn submit(self) -> Result<PendingTransfer<T>, TransferError> {
        // SAFETY: No external semaphore operations are requested.
        unsafe { self.submit_with_sync(&[], &[]) }
    }

    /// Submit with native semaphore operations owned by the caller's payload.
    ///
    /// # Safety
    /// Every semaphore must belong to this Device and satisfy Vulkan's pending
    /// signal/wait and stage-scope rules. Its owner must remain alive through
    /// every submitted use, including uncertain submission failure. The payload
    /// (or another retained owner) must provide that lifetime independently of
    /// these borrowed submit-info arrays.
    pub(super) unsafe fn submit_with_sync(
        self,
        waits: &[vk::SemaphoreSubmitInfo<'_>],
        signals: &[vk::SemaphoreSubmitInfo<'_>],
    ) -> Result<PendingTransfer<T>, TransferError> {
        let mut pending = PendingTransfer {
            retained: Retained::new(self.resources),
        };
        let owner = Arc::clone(&pending.retained.value().unwrap().native.owner);
        let device = &owner.device;
        let submitted = {
            let _queue = device
                .lock_queue()
                .map_err(TransferError::DeviceUnavailable)?;
            let native = &pending.retained.value().unwrap().native;
            let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(native.command)];
            let submits = [vk::SubmitInfo2::default()
                .command_buffer_infos(&commands)
                .wait_semaphore_infos(waits)
                .signal_semaphore_infos(signals)];
            let fence = native.fence;
            pending.retained.begin_submit();
            // SAFETY: The Device-owned lock serializes every Queue host call.
            // This owner already retains pool/commands/Fence/payload before the
            // call, including any unwind or uncertain submission failure.
            let result = unsafe { device.raw().queue_submit2(device.queue(), &submits, fence) };
            if let Err(result) = result {
                device.observe_submit_result(result);
            }
            let result = pending.retained.submitted(result);
            if result.is_ok() {
                pending.retained.value_mut().unwrap().payload.submitted();
            }
            result
        };
        if let Err(result) = submitted {
            let error = native_error(device, "vkQueueSubmit2", result);
            // PendingTransfer remains responsible for safe retirement on error.
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan transfer submission failed");
            return Err(error);
        }
        let native = &pending.retained.value().unwrap().native;
        tracing::debug!(target: LOG_TARGET, device = device.name(), fence = ?native.fence, command = ?native.command,
            bytes = pending.retained.value().unwrap().payload.bytes(),
            path = ?pending.retained.value().unwrap().payload.label(),
            "Vulkan transfer submitted");
        Ok(pending)
    }
}

pub(super) struct PendingTransfer<T: TransferPayload = TransferBuffers> {
    retained: Retained<TransferResources<T>>,
}

impl<T: TransferPayload> PendingTransfer<T> {
    pub(super) fn is_pending(&self) -> bool {
        self.retained.is_pending()
    }

    /// Observe completion without an API waiting budget. An unresolved owner is
    /// retained exactly as for a timed wait.
    pub(super) fn poll(&mut self) -> Result<Option<T>, TransferError> {
        self.wait(Duration::ZERO)
    }

    fn wait_native(&mut self, timeout: Duration) -> Result<bool, vk::Result> {
        let native = &self.retained.value().unwrap().native;
        let device = &native.owner.device;
        // SAFETY: This uniquely owned Fence belongs to the pending submit.
        // Its resources are retained throughout the synchronous host wait.
        // Even after another operation observes Device loss, obtain this wait's
        // actual result before retiring the objects covered by this Fence.
        let result = unsafe {
            device
                .raw()
                .wait_for_fences(&[native.fence], true, timeout_ns(timeout))
        };
        if let Err(result) = result {
            device.observe_result(result);
        }
        self.retained.waited(result)
    }

    /// `Ok(None)` means timeout and preserves the pending owner for another wait.
    /// Errors also retain unproven work; Device loss permits teardown, never data.
    pub(super) fn wait(&mut self, timeout: Duration) -> Result<Option<T>, TransferError> {
        if !self.retained.is_pending() {
            return Err(if self.retained.is_lost() {
                TransferError::DeviceUnavailable(DeviceState::Lost)
            } else {
                TransferError::AlreadyFinished
            });
        }
        let result = self.wait_native(timeout);
        let device = &self.retained.value().unwrap().native.owner.device;
        match result {
            Ok(false) => {
                tracing::debug!(target: LOG_TARGET, device = device.name(), timeout_ns = timeout_ns(timeout), "Vulkan transfer remains pending");
                Ok(None)
            }
            Err(result) => {
                let error = native_error(device, "vkWaitForFences", result);
                tracing::error!(target: LOG_TARGET, error = %error, "Vulkan transfer wait failed");
                Err(error)
            }
            Ok(true) => {
                device
                    .ensure_active()
                    .map_err(TransferError::DeviceUnavailable)?;
                let TransferResources {
                    native,
                    mut payload,
                } = self.retained.take_retired().unwrap();
                // SAFETY: Successful Fence wait proves every retained command
                // completed. The active Device check excludes lost/retired data.
                unsafe {
                    payload.establish_contents();
                }
                tracing::debug!(target: LOG_TARGET, device = native.owner.device.name(), fence = ?native.fence,
                    bytes = payload.bytes(), path = ?payload.label(), "Vulkan transfer completed");
                drop(native);
                payload.retired();
                Ok(Some(payload))
            }
        }
    }
}

impl<T: TransferPayload> Drop for PendingTransfer<T> {
    fn drop(&mut self) {
        if !self.retained.is_pending() {
            return;
        }
        let result = self.wait_native(DROP_TIMEOUT);
        if self.retained.is_pending() {
            // Stop before logging (which may unwind). Retained's Drop preserves
            // this whole native/resource/Allocator/Device chain on unresolved work.
            let device = &self.retained.value().unwrap().native.owner.device;
            device.stop();
            tracing::error!(target: LOG_TARGET, device = device.name(), wait_result = ?result,
                timeout_ns = timeout_ns(DROP_TIMEOUT), "GPU completion unproven during drop; retaining resources and stopping Device");
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod benchmarks;
