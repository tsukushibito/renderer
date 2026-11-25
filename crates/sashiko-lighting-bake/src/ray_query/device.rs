//! Private, synchronous bake queue. Every submission retires before its borrowed buffers.
use super::Result;
use ash::vk;
use sashiko_renderer::core::vulkan::VulkanExternalDevice;
use std::sync::Arc;
use vk_mem::Alloc;

pub(super) struct Device {
    allocator: vk_mem::Allocator,
    max_buffer_size: u64,
    pub host: VulkanExternalDevice,
    pub acceleration: ash::khr::acceleration_structure::Device,
}
impl Device {
    pub fn new(host: VulkanExternalDevice) -> Result<Arc<Self>> {
        let acceleration =
            ash::khr::acceleration_structure::Device::new(host.instance(), host.device());
        let mut info = vk_mem::AllocatorCreateInfo::new(
            host.instance(),
            host.device(),
            host.physical_device(),
        );
        info.vulkan_api_version = vk::API_VERSION_1_3;
        info.flags = vk_mem::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS;
        // SAFETY: The host retains all three handles past the allocator's destruction. BDA is enabled.
        let allocator = unsafe { vk_mem::Allocator::new(info) }?;
        let mut maintenance = vk::PhysicalDeviceMaintenance4Properties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut maintenance);
        // SAFETY: The physical device belongs to this Vulkan 1.3 instance.
        unsafe {
            host.instance()
                .get_physical_device_properties2(host.physical_device(), &mut properties);
        }
        Ok(Arc::new(Self {
            max_buffer_size: maintenance.max_buffer_size,
            allocator,
            host,
            acceleration,
        }))
    }
    pub fn raw(&self) -> &ash::Device {
        self.host.device()
    }
}
pub(super) struct Buffer {
    dev: Arc<Device>,
    pub handle: vk::Buffer,
    allocation: vk_mem::Allocation,
    pub size: u64,
    mapped: *mut u8,
}
impl Buffer {
    pub fn new(
        dev: &Arc<Device>,
        size: u64,
        usage: vk::BufferUsageFlags,
        host_access: vk_mem::AllocationCreateFlags,
    ) -> Result<Self> {
        if size == 0 || size > dev.max_buffer_size {
            return Err("ray query buffer exceeds device size limit".into());
        }
        let info = vk::BufferCreateInfo::default().size(size).usage(usage);
        let mapped = !host_access.is_empty();
        let alloc = vk_mem::AllocationCreateInfo {
            usage: if mapped {
                vk_mem::MemoryUsage::AutoPreferHost
            } else {
                vk_mem::MemoryUsage::AutoPreferDevice
            },
            flags: if mapped {
                host_access | vk_mem::AllocationCreateFlags::MAPPED
            } else {
                host_access
            },
            ..Default::default()
        };
        // SAFETY: Nonempty buffer belongs to the live allocator and is destroyed exactly once.
        let (handle, allocation) = unsafe {
            dev.allocator
                .create_buffer_with_alignment(&info, &alloc, 16)
        }?;
        let pointer = dev
            .allocator
            .get_allocation_info(&allocation)
            .mapped_data
            .cast();
        let buffer = Self {
            dev: dev.clone(),
            handle,
            allocation,
            size,
            mapped: pointer,
        };
        if mapped && pointer.is_null() {
            return Err("could not map bake buffer".into());
        }
        Ok(buffer)
    }
    pub fn address(&self) -> u64 {
        // SAFETY: Callers allocate all addressed buffers with SHADER_DEVICE_ADDRESS on the BDA allocator.
        unsafe {
            self.dev.raw().get_buffer_device_address(
                &vk::BufferDeviceAddressInfo::default().buffer(self.handle),
            )
        }
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.mapped.is_null() || bytes.len() as u64 > self.size {
            return Err("invalid bake upload size".into());
        }
        // SAFETY: The mapped allocation covers the checked range; all prior device reads have retired.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.mapped, bytes.len());
        }
        self.dev
            .allocator
            .flush_allocation(&self.allocation, 0, bytes.len() as u64)?;
        Ok(())
    }
    pub fn read(&self, len: usize) -> Result<&[u8]> {
        if self.mapped.is_null() || len as u64 > self.size {
            return Err("invalid bake readback size".into());
        }
        self.dev
            .allocator
            .invalidate_allocation(&self.allocation, 0, len as u64)?;
        // SAFETY: Dispatch fence retired; allocation remains mapped and cannot be resubmitted while borrowed.
        Ok(unsafe { std::slice::from_raw_parts(self.mapped, len) })
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: Synchronous queue and unique ownership guarantee no outstanding references.
        unsafe {
            self.dev
                .allocator
                .destroy_buffer(self.handle, &mut self.allocation);
        }
    }
}

pub(super) struct Commands {
    queries: vk::QueryPool,
    period_ns: f64,
    valid_bits: u32,
    pub timing: Option<crate::lightmaps::DispatchTiming>,
    dev: Arc<Device>,
    pool: vk::CommandPool,
    pub cmd: vk::CommandBuffer,
    fence: vk::Fence,
}
impl Commands {
    pub fn new(dev: &Arc<Device>) -> Result<Self> {
        let info = vk::CommandPoolCreateInfo::default().queue_family_index(dev.host.queue_family());
        // SAFETY: Pool and fence belong to the retained device. Partial construction is cleaned up by Drop.
        unsafe {
            let pool = dev.raw().create_command_pool(&info, None)?;
            let mut commands = Self {
                dev: dev.clone(),
                pool,
                queries: vk::QueryPool::null(),
                period_ns: 0.,
                valid_bits: 0,
                timing: None,
                cmd: vk::CommandBuffer::null(),
                fence: vk::Fence::null(),
            };
            commands.cmd = dev.raw().allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .command_buffer_count(1)
                    .level(vk::CommandBufferLevel::PRIMARY),
            )?[0];
            commands.fence = dev
                .raw()
                .create_fence(&vk::FenceCreateInfo::default(), None)?;
            Ok(commands)
        }
    }
    pub fn enable_timing(&mut self) -> Result<()> {
        let host = &self.dev.host;
        // SAFETY: The physical device and selected queue family belong to this instance.
        unsafe {
            self.valid_bits = host
                .instance()
                .get_physical_device_queue_family_properties(host.physical_device())
                [host.queue_family() as usize]
                .timestamp_valid_bits;
            self.period_ns = host
                .instance()
                .get_physical_device_properties(host.physical_device())
                .limits
                .timestamp_period as f64;
            if self.valid_bits > 0 {
                self.queries = host.device().create_query_pool(
                    &vk::QueryPoolCreateInfo::default()
                        .query_type(vk::QueryType::TIMESTAMP)
                        .query_count(2),
                    None,
                )?;
            }
        }
        self.timing = Some(Default::default());
        Ok(())
    }
    pub fn run(&mut self, record: impl FnOnce(vk::CommandBuffer)) -> Result<()> {
        let vk = self.dev.raw();
        // SAFETY: Exclusive primary buffer, reset only after its previous submission completed.
        unsafe {
            vk.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())?;
            vk.reset_fences(&[self.fence])?;
            vk.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let timed = self.queries != vk::QueryPool::null();
            let lifetime = timed.then(std::time::Instant::now);
            if timed {
                vk.cmd_reset_query_pool(self.cmd, self.queries, 0, 2);
                vk.cmd_write_timestamp(
                    self.cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    self.queries,
                    0,
                );
            }
            record(self.cmd);
            if timed {
                vk.cmd_write_timestamp(
                    self.cmd,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    self.queries,
                    1,
                );
            }
            vk.end_command_buffer(self.cmd)?;
            let cmds = [self.cmd];
            self.dev.host.with_queue(|q| {
                vk.queue_submit(
                    q,
                    &[vk::SubmitInfo::default().command_buffers(&cmds)],
                    self.fence,
                )
            })??;
            let waiting = self.timing.as_ref().map(|_| std::time::Instant::now());
            if let Err(e) = vk.wait_for_fences(&[self.fence], true, u64::MAX) {
                // A non-device-lost wait error (e.g. host OOM) does not retire a
                // submitted command. Keep its borrowed resources live until the
                // fence signals or the device is lost, then report the original error.
                if e != vk::Result::ERROR_DEVICE_LOST {
                    loop {
                        match vk.get_fence_status(self.fence) {
                            Ok(true) | Err(vk::Result::ERROR_DEVICE_LOST) => break,
                            _ => std::thread::sleep(std::time::Duration::from_millis(1)),
                        }
                    }
                }
                return Err(e.into());
            }
            if let (Some(t), Some(start)) = (&mut self.timing, waiting) {
                t.wait += start.elapsed();
                if let Some(since) = lifetime
                    && since.elapsed().as_secs_f64()
                        < (2f64).powi(self.valid_bits as i32) * self.period_ns * 1e-9
                {
                    let mut ticks = [0u64; 2];
                    vk.get_query_pool_results(
                        self.queries,
                        0,
                        &mut ticks,
                        vk::QueryResultFlags::TYPE_64,
                    )?;
                    let mask = u64::MAX >> (64 - self.valid_bits);
                    t.gpu += std::time::Duration::from_secs_f64(
                        (ticks[1].wrapping_sub(ticks[0]) & mask) as f64 * self.period_ns * 1e-9,
                    );
                } else {
                    t.missing_gpu = true;
                }
            }
        }
        Ok(())
    }
}
impl Drop for Commands {
    fn drop(&mut self) {
        // SAFETY: run retires the queue before borrowed resources go out of scope. Pool owns cmd.
        unsafe {
            self.dev.raw().destroy_query_pool(self.queries, None);
            self.dev.raw().destroy_fence(self.fence, None);
            self.dev.raw().destroy_command_pool(self.pool, None);
        }
    }
}
pub(super) fn barrier(
    dev: &Device,
    cmd: vk::CommandBuffer,
    src: vk::PipelineStageFlags,
    access: vk::AccessFlags,
    dst: vk::PipelineStageFlags,
    next: vk::AccessFlags,
) {
    let memory = vk::MemoryBarrier::default()
        .src_access_mask(access)
        .dst_access_mask(next);
    // SAFETY: cmd is recording on the shared graphics/compute family; stages match their accesses.
    unsafe {
        dev.raw().cmd_pipeline_barrier(
            cmd,
            src,
            dst,
            vk::DependencyFlags::empty(),
            &[memory],
            &[],
            &[],
        );
    }
}
