//! Per-execution secondary buffers. A scoped job exclusively records one pool.
use super::{MemoryOwner, RuntimeError, StoragePool, VulkanRuntime, storage::reserved};
use ash::vk;
use sashiko_jobs::JobSystem;
use std::{num::NonZeroUsize, sync::Arc};

pub(super) struct Recorder {
    pub jobs: JobSystem,
    pub storage: StoragePool<BatchCommands>,
    pub capacity: usize,
}

pub(super) struct BatchCommands {
    pub lanes: Vec<Lane>,
    pub handles: Vec<vk::CommandBuffer>,
}

pub(super) struct Lane {
    owner: Arc<MemoryOwner>,
    pool: vk::CommandPool,
    pub command: vk::CommandBuffer,
    pub pass: usize,
    pub batch: usize,
    pub descriptors: usize,
    pub error: Option<RuntimeError>,
}

impl VulkanRuntime {
    /// Prepare exclusive secondary command pools for each in-flight Graph.
    /// Graphs with more live raster batches than the budget use inline recording.
    pub fn prepare_raster_recording(
        &mut self,
        jobs: JobSystem,
        max_batches: NonZeroUsize,
    ) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if self
            .secondary
            .as_ref()
            .is_some_and(|old| !old.storage.is_idle())
        {
            use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
            return Err(RuntimeError::capacity_with(
                "retire executions before replacing secondary command pools",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ExecutionSlots,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        let capacity = max_batches.get();
        let storage = StoragePool::new(
            self.config.execution_slots,
            || {
                let mut lanes = reserved(capacity)?;
                let mut handles = reserved(capacity)?;
                for _ in 0..capacity {
                    let lane = Lane::new(Arc::clone(&self.owner))?;
                    handles.push(lane.command);
                    lanes.push(lane);
                }
                Ok(BatchCommands { lanes, handles })
            },
            |_| {},
        )?;
        self.secondary = Some(Recorder {
            jobs,
            storage,
            capacity,
        });
        Ok(())
    }
}

impl Lane {
    fn new(owner: Arc<MemoryOwner>) -> Result<Self, RuntimeError> {
        let device = &owner.device;
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: Each lane exclusively owns this pool on the selected graphics family.
        let pool = unsafe {
            device.raw().create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(device.queue_family())
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| super::native(&owner, "vkCreateCommandPool", e))?;
        let mut lane = Self {
            owner,
            pool,
            command: vk::CommandBuffer::null(),
            pass: 0,
            batch: 0,
            descriptors: 0,
            error: None,
        };
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::ObjectCreate, 1);
        // SAFETY: The newly owned pool is live and exclusive throughout allocation.
        lane.command = unsafe {
            lane.owner.device.raw().allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::SECONDARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| super::native(&lane.owner, "vkAllocateCommandBuffers", e))?[0];
        Ok(lane)
    }

    pub fn record(
        &mut self,
        inheritance: &vk::CommandBufferInheritanceInfo<'_>,
        draw: impl FnOnce(vk::CommandBuffer),
    ) -> Result<(), RuntimeError> {
        let device = self.owner.device.raw();
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::Reset, 1);
        // SAFETY: Only a cancelled or fence-retired lease returns here. This job
        // exclusively owns the pool, and never resets another job's command.
        unsafe { device.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty()) }
            .map_err(|e| super::native(&self.owner, "vkResetCommandPool", e))?;
        // SAFETY: The captured attachments and samples match the primary rendering scope.
        unsafe {
            device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(
                        vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT
                            | vk::CommandBufferUsageFlags::RENDER_PASS_CONTINUE,
                    )
                    .inheritance_info(inheritance),
            )
        }
        .map_err(|e| super::native(&self.owner, "vkBeginCommandBuffer", e))?;
        draw(self.command);
        // SAFETY: The job has finished all commands in this inherited rendering scope.
        unsafe { device.end_command_buffer(self.command) }
            .map_err(|e| super::native(&self.owner, "vkEndCommandBuffer", e))
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(
            crate::measurement::NativeEvent::ObjectDestroy,
            1 + u64::from(self.command != vk::CommandBuffer::null()),
        );
        // SAFETY: Graph leases retain the pool through cancellation or fence retirement.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_command_pool(self.pool, None)
        };
    }
}
