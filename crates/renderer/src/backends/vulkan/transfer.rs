//! Owned, single-queue upload -> device buffer -> readback execution.
//!
//! This private path copies whole, equally sized buffers. Recording and pending
//! owners retain every referenced resource; callers regain buffers only after a
//! successful Fence wait. Drop may wait for up to its Vulkan timeout budget.

use std::error::Error;
use std::ffi::CStr;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ash::vk;

use super::buffer::{BufferRole, VulkanBuffer};
use super::device::{DeviceState, VulkanDevice};
use super::instance::LOG_TARGET;
use super::memory::MemoryOwner;

mod lifecycle;
use lifecycle::Retained;

const DROP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) enum TransferError {
    InvalidBuffers(&'static str),
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

pub(super) struct TransferBuffers {
    pub(super) upload: VulkanBuffer,
    pub(super) transfer: VulkanBuffer,
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

/// Native children are destroyed before the buffers they referenced. This Drop
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
        {
            let _queue = device
                .lock_queue()
                .map_err(TransferError::DeviceUnavailable)?;
            native.name(device.queue(), c"renderer.graphics-compute.queue")?;
        }
        Ok(native)
    }

    fn name<T: vk::Handle>(&self, handle: T, name: &CStr) -> Result<(), TransferError> {
        let device = &self.owner.device;
        // SAFETY: Every named object belongs to this live Device; command/pool/
        // fence are uniquely owned, and naming the Queue takes its shared lock.
        unsafe {
            device.debug_utils().set_debug_utils_object_name(
                &vk::DebugUtilsObjectNameInfoEXT::default()
                    .object_handle(handle)
                    .object_name(name),
            )
        }
        .map_err(|result| native_error(device, "vkSetDebugUtilsObjectNameEXT", result))
    }

    fn record(&self, buffers: &TransferBuffers) -> Result<(), TransferError> {
        let device = &self.owner.device;
        // SAFETY: Newly allocated, uniquely owned primary command buffer.
        unsafe {
            device.raw().begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|result| native_error(device, "vkBeginCommandBuffer", result))?;
        let debug = device.debug_utils();
        let copy = [vk::BufferCopy::default().size(buffers.upload.size())];
        let between = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
        let to_host = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ)];
        // SAFETY: All buffers are validated, unaliased, bound to this Device and
        // exclusively retained by the enclosing recording owner. No handle is
        // exposed to the caller. Source writes/flush precede submit, which makes
        // host writes available; a HOST->COPY barrier is unnecessary. The COPY
        // dependency makes the first copy's writes visible to the second; the
        // HOST dependency plus Fence wait and invalidate establish CPU readback.
        unsafe {
            debug.cmd_begin_debug_utils_label(
                self.command,
                &vk::DebugUtilsLabelEXT::default()
                    .label_name(c"renderer upload -> device -> readback")
                    .color([0.2, 0.7, 1.0, 1.0]),
            );
            device.raw().cmd_copy_buffer(
                self.command,
                buffers.upload.transfer_handle(),
                buffers.transfer.transfer_handle(),
                &copy,
            );
            device.raw().cmd_pipeline_barrier2(
                self.command,
                &vk::DependencyInfo::default().memory_barriers(&between),
            );
            device.raw().cmd_copy_buffer(
                self.command,
                buffers.transfer.transfer_handle(),
                buffers.readback.transfer_handle(),
                &copy,
            );
            device.raw().cmd_pipeline_barrier2(
                self.command,
                &vk::DependencyInfo::default().memory_barriers(&to_host),
            );
            debug.cmd_end_debug_utils_label(self.command);
        }
        // SAFETY: The unique command buffer has balanced command labels and no
        // render pass; all referenced resources remain in the recording owner.
        unsafe { device.raw().end_command_buffer(self.command) }
            .map_err(|result| native_error(device, "vkEndCommandBuffer", result))
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

struct TransferResources {
    native: NativeTransfer,
    buffers: TransferBuffers,
}

/// Dropping this unsubmitted owner cancels the recording and releases resources.
pub(super) struct RecordedTransfer {
    resources: TransferResources,
}

impl RecordedTransfer {
    pub(super) fn new(buffers: TransferBuffers) -> Result<Self, TransferError> {
        let result = (|| {
            buffers.validate()?;
            buffers
                .upload
                .owner()
                .device
                .ensure_active()
                .map_err(TransferError::DeviceUnavailable)?;
            let native = NativeTransfer::new(Arc::clone(buffers.upload.owner()))?;
            let recorded = Self {
                resources: TransferResources { native, buffers },
            };
            recorded
                .resources
                .native
                .record(&recorded.resources.buffers)?;
            Ok(recorded)
        })();
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan transfer recording failed");
        }
        result
    }

    pub(super) fn submit(self) -> Result<PendingTransfer, TransferError> {
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
            let submits = [vk::SubmitInfo2::default().command_buffer_infos(&commands)];
            let fence = native.fence;
            pending.retained.begin_submit();
            // SAFETY: The Device-owned lock serializes every Queue host call.
            // This owner already retains pool/commands/Fence/buffers before the
            // call, including any unwind or uncertain submission failure.
            let result = unsafe { device.raw().queue_submit2(device.queue(), &submits, fence) };
            if let Err(result) = result {
                device.observe_result(result);
            }
            pending.retained.submitted(result)
        };
        if let Err(result) = submitted {
            let error = native_error(device, "vkQueueSubmit2", result);
            // PendingTransfer remains responsible for safe retirement on error.
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan transfer submission failed");
            return Err(error);
        }
        let native = &pending.retained.value().unwrap().native;
        tracing::info!(target: LOG_TARGET, device = device.name(), fence = ?native.fence,
            bytes = pending.retained.value().unwrap().buffers.upload.size(),
            "Vulkan buffer transfer submitted");
        Ok(pending)
    }
}

pub(super) struct PendingTransfer {
    retained: Retained<TransferResources>,
}

impl PendingTransfer {
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
    pub(super) fn wait(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<TransferBuffers>, TransferError> {
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
                    mut buffers,
                } = self.retained.take_retired().unwrap();
                // SAFETY: Successful Fence wait proves both whole-buffer copies
                // completed. The active Device check excludes lost/retired data.
                unsafe {
                    buffers.transfer.establish_gpu_contents();
                    buffers.readback.establish_gpu_contents();
                }
                tracing::info!(target: LOG_TARGET, device = native.owner.device.name(), fence = ?native.fence,
                    bytes = buffers.readback.size(), "Vulkan buffer transfer completed");
                drop(native);
                Ok(Some(buffers))
            }
        }
    }
}

impl Drop for PendingTransfer {
    fn drop(&mut self) {
        if !self.retained.is_pending() {
            return;
        }
        let result = self.wait_native(DROP_TIMEOUT);
        if self.retained.is_pending() {
            // Stop before logging (which may unwind). Retained's Drop preserves
            // this whole native/buffer/Allocator/Device chain on unresolved work.
            let device = &self.retained.value().unwrap().native.owner.device;
            device.stop();
            tracing::error!(target: LOG_TARGET, device = device.name(), wait_result = ?result,
                timeout_ns = timeout_ns(DROP_TIMEOUT), "GPU completion unproven during drop; retaining resources and stopping Device");
        }
    }
}

#[cfg(test)]
mod tests;
