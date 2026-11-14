//! Native acceptance for the managed interface without a general Graph encoder.

use std::ffi::CStr;
use std::time::Duration;

use super::*;
use crate::backends::vulkan::buffer::{BufferRole, VulkanBuffer};
use crate::backends::vulkan::graph::{RuntimeConfig, VulkanRuntime};
use crate::backends::vulkan::memory::VulkanAllocator;
use crate::backends::vulkan::transfer::{
    RecordedTransfer, TransferError, TransferPayload, record_host_read,
};

struct Output {
    owner: Arc<MemoryOwner>,
    handle: vk::Buffer,
    allocation: vk_mem::Allocation,
}
impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: The fixture payload owns this unaliased output through GPU
        // retirement and retains its allocator. No raw handle escapes it.
        unsafe {
            self.owner
                .raw
                .destroy_buffer(self.handle, &mut self.allocation)
        };
    }
}

struct OutputBinding {
    program: VulkanProgram,
    output: Output,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
}
impl Drop for OutputBinding {
    fn drop(&mut self) {
        // SAFETY: The recording/pending payload is the sole set owner; Drop is
        // reached only for cancellation or proven retirement. Destroy the pool
        // before its separately retained Program/layout and output allocation.
        unsafe {
            self.program
                .0
                .owner
                .device
                .raw()
                .destroy_descriptor_pool(self.pool, None)
        };
    }
}

struct Work {
    binding: VulkanUniformBinding,
    output: OutputBinding,
    readback: VulkanBuffer,
}
impl Work {
    fn new(binding: VulkanUniformBinding) -> Self {
        let program = binding.0.program.clone();
        let owner = Arc::clone(&program.0.owner);
        let info = vk::BufferCreateInfo::default()
            .size(16)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let allocation_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: Fixed nonzero unaliased output, retained allocator and Device.
        let (handle, allocation) =
            unsafe { owner.raw.create_buffer(&info, &allocation_info) }.unwrap();
        let mut output = OutputBinding {
            program,
            output: Output {
                owner: Arc::clone(&owner),
                handle,
                allocation,
            },
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
        };
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 1,
        }];
        // SAFETY: Fresh private set written once before recording; the payload
        // retains the output range and program/layout through every native use.
        unsafe {
            output.pool = owner
                .device
                .raw()
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .unwrap();
            let layouts = [output.program.0.output_set];
            output.set = owner
                .device
                .raw()
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(output.pool)
                        .set_layouts(&layouts),
                )
                .unwrap()[0];
            let buffers = [vk::DescriptorBufferInfo::default()
                .buffer(handle)
                .offset(0)
                .range(16)];
            let writes = [vk::WriteDescriptorSet::default()
                .dst_set(output.set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&buffers)];
            owner.device.raw().update_descriptor_sets(&writes, &[]);
        }
        let readback = VulkanAllocator { owner }
            .create_buffer(BufferRole::Readback, 16)
            .unwrap();
        Self {
            binding,
            output,
            readback,
        }
    }

    fn read(&mut self) -> [u8; 16] {
        let mut result = [0; 16];
        self.readback.read(0, &mut result).unwrap();
        result
    }
}

impl TransferPayload for Work {
    fn validate(&self) -> Result<(), TransferError> {
        self.owner()
            .device
            .ensure_active()
            .map_err(TransferError::DeviceUnavailable)
    }
    fn owner(&self) -> &Arc<MemoryOwner> {
        &self.binding.0.program.0.owner
    }
    fn bytes(&self) -> u64 {
        16
    }
    fn label(&self) -> &'static CStr {
        c"managed.uniform-readback"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        let program = &self.binding.0.program.0;
        let device = &program.owner.device;
        let sets = [self.binding.0.set, self.output.set];
        let barrier = [vk::BufferMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(self.output.output.handle)
            .offset(0)
            .size(16)];
        // SAFETY: One reviewed invocation reads/writes exactly the checked
        // 16-byte ranges. The immutable sets, pipeline, buffers and owner remain
        // retained by this payload. Submit makes flushed host writes available;
        // explicit shader-to-copy and copy-to-host dependencies follow.
        unsafe {
            device.raw().cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::COMPUTE,
                program.pipeline,
            );
            device.raw().cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::COMPUTE,
                program.layout,
                0,
                &sets,
                &[],
            );
            device.raw().cmd_dispatch(command, 1, 1, 1);
            device.raw().cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().buffer_memory_barriers(&barrier),
            );
            device.raw().cmd_copy_buffer(
                command,
                self.output.output.handle,
                self.readback.transfer_handle(),
                &[vk::BufferCopy::default().size(16)],
            );
            record_host_read(device, command);
        }
    }

    unsafe fn establish_contents(&mut self) {
        // SAFETY: Transfer retirement proved successful completion on an active
        // Device. The dispatch and copy wrote all 16 readback bytes.
        unsafe { self.readback.establish_gpu_contents() };
    }
}

fn descriptor() -> UniformBindingDesc {
    UniformBindingDesc {
        group: 0,
        binding: 0,
        offset: 256,
        size: 16,
    }
}

#[test]
#[ignore = "explicit real-GPU managed uniform ABI, immutable versions and ownership; run strict and shipping separately"]
fn vulkan_managed_uniform_creation_and_versions() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let runtime = VulkanRuntime::new(RuntimeConfig::default()).unwrap();
        let monitor = runtime.validation_monitor();
        assert_ne!(
            runtime.device_info().device_type,
            "CPU",
            "hardware GPU required"
        );
        #[cfg(feature = "development-validation")]
        {
            let report = monitor.report();
            assert_eq!(report.core_validation, Some(true), "{report:#?}");
            assert_eq!(report.synchronization_validation, Some(true), "{report:#?}");
        }
        #[cfg(not(feature = "development-validation"))]
        assert_eq!(
            monitor.report().requested,
            crate::backends::vulkan::ValidationMode::Disabled
        );

        let managed = ManagedProgram::uniform_readback();
        let program = runtime.create_program(&managed).unwrap();
        let [expected, newer_expected] = crate::program::UNIFORM_TEST_DATA;
        let mut bytes = vec![0; 272];
        bytes[256..].copy_from_slice(&expected);
        let buffer = runtime.create_uniform_buffer(&bytes).unwrap();
        assert_eq!(buffer.len(), 272);
        bytes[256..].fill(0); // Caller mutation cannot alter published contents.
        let binding = runtime
            .create_uniform_binding(&program, &buffer, descriptor())
            .unwrap();
        assert_eq!(binding.desc(), descriptor());
        assert_eq!(program.uniform_layout(), managed.uniform_layout());
        for desc in [
            UniformBindingDesc {
                group: 1,
                ..descriptor()
            },
            UniformBindingDesc {
                binding: 1,
                ..descriptor()
            },
            UniformBindingDesc {
                offset: 1,
                ..descriptor()
            },
            UniformBindingDesc {
                size: 15,
                ..descriptor()
            },
            UniformBindingDesc {
                size: 17,
                ..descriptor()
            },
        ] {
            assert!(
                matches!(runtime.create_uniform_binding(&program, &buffer, desc),
                Err(error) if error.kind() == super::super::error::RuntimeErrorKind::InvalidInput)
            );
        }
        assert!(matches!(runtime.create_uniform_buffer(&[]),
            Err(error) if error.kind() == super::super::error::RuntimeErrorKind::InvalidInput));
        // Reject another allocator/Device even when its layout and bytes match.
        let foreign = VulkanRuntime::new(RuntimeConfig::default()).unwrap();
        let foreign_monitor = foreign.validation_monitor();
        let foreign_buffer = foreign.create_uniform_buffer(&[0; 272]).unwrap();
        assert!(
            matches!(runtime.create_uniform_binding(&program, &foreign_buffer, descriptor()),
            Err(error) if error.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
        );
        let foreign_program = foreign.create_program(&managed).unwrap();
        assert!(
            matches!(runtime.create_uniform_binding(&foreign_program, &buffer, descriptor()),
            Err(error) if error.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
        );
        drop((foreign_program, foreign_buffer, foreign));
        assert!(
            foreign_monitor.report().is_clean(),
            "{:#?}",
            foreign_monitor.report()
        );

        // Cancel a real recorded command before submit; its owned descriptors
        // and buffers must be releasable without changing other bindings.
        let canceled = RecordedTransfer::new(Work::new(binding.clone())).unwrap();
        drop(canceled);
        let weak_program = Arc::downgrade(&program.0);
        let weak_buffer = Arc::downgrade(&buffer.0);
        let weak_binding = Arc::downgrade(&binding.0);
        let recorded = RecordedTransfer::new(Work::new(binding)).unwrap();
        let newer_program = runtime.create_program(&managed).unwrap();
        bytes[256..].copy_from_slice(&newer_expected);
        let newer_buffer = runtime.create_uniform_buffer(&bytes).unwrap();
        let newer_binding = runtime
            .create_uniform_binding(&newer_program, &newer_buffer, descriptor())
            .unwrap();
        let newer_recorded = RecordedTransfer::new(Work::new(newer_binding)).unwrap();
        drop((program, buffer, newer_program, newer_buffer));
        assert!(weak_program.upgrade().is_some());
        assert!(weak_buffer.upgrade().is_some());
        assert!(weak_binding.upgrade().is_some());
        let mut first = recorded.submit().unwrap();
        let mut second = newer_recorded.submit().unwrap();
        drop(runtime); // Pending work, not the facade, owns all native resources.
        let mut second_result = second.wait(Duration::from_secs(10)).unwrap().unwrap();
        let mut first_result = first.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(first_result.read(), expected);
        assert_eq!(second_result.read(), newer_expected);
        drop((first_result, second_result, first, second));
        assert!(weak_program.upgrade().is_none());
        assert!(weak_buffer.upgrade().is_none());
        assert!(weak_binding.upgrade().is_none());
        let report = monitor.report();
        assert!(report.is_clean(), "final native teardown: {report:#?}");
    });
}
