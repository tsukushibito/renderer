//! Acceptance uses public buffer/binding creation and a private exclusive executor.

use super::*;
use crate::backends::vulkan::buffer::{BufferRole, VulkanBuffer as TransferBuffer};
use crate::backends::vulkan::graph::{RuntimeConfig, VulkanRuntime};
use crate::backends::vulkan::memory::VulkanAllocator;
use crate::backends::vulkan::transfer::{
    RecordedTransfer, TransferError, TransferPayload, record_host_read,
};
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use std::time::Duration;

struct StorageLease {
    buffer: VulkanBuffer,
    reused: bool,
}
impl StorageLease {
    fn acquire(buffer: VulkanBuffer) -> Result<Self, RuntimeError> {
        buffer
            .0
            .storage
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RuntimeError::capacity("storage output is already recorded or pending"))?;
        let reused = buffer.0.storage.used.load(Ordering::Acquire);
        Ok(Self { buffer, reused })
    }
}
impl Drop for StorageLease {
    fn drop(&mut self) {
        self.buffer.0.storage.busy.store(false, Ordering::Release);
    }
}

struct Work {
    bindings: VulkanBindings,
    output: Option<StorageLease>,
    output_index: usize,
    readback: TransferBuffer,
}
impl Work {
    fn new(bindings: VulkanBindings) -> Result<Self, RuntimeError> {
        let output_index = bindings
            .0
            .program
            .layout()
            .iter()
            .position(|b| b.kind == BufferBindingType::StorageWrite)
            .unwrap();
        let output = StorageLease::acquire(bindings.0.buffers[output_index].clone())?;
        let allocator = VulkanAllocator {
            owner: Arc::clone(&bindings.0.program.0.owner),
        };
        let readback = allocator.create_buffer(BufferRole::Readback, 16).unwrap();
        Ok(Self {
            bindings,
            output: Some(output),
            output_index,
            readback,
        })
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
        &self.bindings.0.program.0.owner
    }
    fn bytes(&self) -> u64 {
        16
    }
    fn label(&self) -> &'static CStr {
        c"managed.storage-readback"
    }
    unsafe fn record(&self, command: vk::CommandBuffer) {
        let program = &self.bindings.0.program.0;
        let device = &program.owner.device;
        let output = self.output.as_ref().unwrap();
        let desc = self.bindings.0.descs[self.output_index];
        let reused = [vk::BufferMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::COPY)
            .src_access_mask(
                vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::TRANSFER_READ,
            )
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(output.buffer.0.handle)
            .offset(0)
            .size(output.buffer.len())];
        let to_copy = [vk::BufferMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(output.buffer.0.handle)
            .offset(desc.offset)
            .size(16)];
        // SAFETY: This executor exclusively leases the Storage output from
        // recording through cancel or actual retirement. The reviewed shader
        // writes exactly 16 bytes within the validated descriptor. Immutable
        // sets/pipeline/buffers remain retained. Prior accepted use is ordered
        // before reuse, then shader writes before copy and copy before host.
        unsafe {
            if output.reused {
                device.raw().cmd_pipeline_barrier2(
                    command,
                    &vk::DependencyInfo::default().buffer_memory_barriers(&reused),
                );
            }
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
                &self.bindings.0.sets,
                &[],
            );
            device.raw().cmd_dispatch(command, 1, 1, 1);
            device.raw().cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().buffer_memory_barriers(&to_copy),
            );
            device.raw().cmd_copy_buffer(
                command,
                output.buffer.0.handle,
                self.readback.transfer_handle(),
                &[vk::BufferCopy::default().src_offset(desc.offset).size(16)],
            );
            record_host_read(device, command);
        }
    }
    fn submitted(&mut self) {
        self.output
            .as_ref()
            .unwrap()
            .buffer
            .0
            .storage
            .used
            .store(true, Ordering::Release);
    }
    unsafe fn establish_contents(&mut self) {
        // SAFETY: Actual successful Fence wait on active Device proved the
        // dispatch/copy completed and initialized every readback byte.
        unsafe { self.readback.establish_gpu_contents() };
        self.output.take();
    }
}

fn desc(group: u32) -> BufferBindingDesc {
    BufferBindingDesc {
        group,
        binding: 0,
        offset: 256,
        size: 16,
    }
}
fn bind(
    runtime: &VulkanRuntime,
    program: &VulkanProgram,
    uniform: &VulkanBuffer,
    storage: &VulkanBuffer,
    uniform_desc: BufferBindingDesc,
    storage_desc: BufferBindingDesc,
) -> Result<VulkanBindings, RuntimeError> {
    // Deliberately reverse caller order to exercise canonical manifest ordering.
    runtime.create_bindings(
        program,
        &[
            BufferBinding {
                buffer: storage,
                desc: storage_desc,
            },
            BufferBinding {
                buffer: uniform,
                desc: uniform_desc,
            },
        ],
    )
}
fn invalid(result: Result<VulkanBindings, RuntimeError>) {
    assert!(
        matches!(result, Err(error) if error.kind() == super::super::error::RuntimeErrorKind::InvalidInput)
    );
}

#[test]
#[ignore = "explicit real-GPU public uniform/storage bindings, versions, exclusion and reuse; run strict and shipping separately"]
fn vulkan_managed_storage_creation_and_versions() {
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
        assert_eq!(program.layout(), managed.bindings());
        let [expected, newer_expected] = crate::program::UNIFORM_TEST_DATA;
        let mut bytes = vec![0; 272];
        bytes[256..].copy_from_slice(&expected);
        let uniform = runtime
            .create_managed_buffer(BufferDescriptor::Uniform { bytes: &bytes })
            .unwrap();
        let storage = runtime
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap();
        assert_eq!(uniform.len(), 272);
        assert_eq!(storage.len(), 272);
        assert_eq!(uniform.usage(), BufferUsage::Uniform);
        assert_eq!(storage.usage(), BufferUsage::Storage);
        bytes[256..].fill(0);
        let bindings = bind(&runtime, &program, &uniform, &storage, desc(0), desc(1)).unwrap();
        assert_eq!(bindings.descs(), &[desc(0), desc(1)]);
        for bad in [
            BufferBindingDesc {
                binding: 1,
                ..desc(1)
            },
            BufferBindingDesc { size: 0, ..desc(1) },
            BufferBindingDesc {
                size: 15,
                ..desc(1)
            },
            BufferBindingDesc {
                size: 17,
                ..desc(1)
            },
            BufferBindingDesc {
                size: u64::MAX,
                ..desc(1)
            },
        ] {
            invalid(bind(&runtime, &program, &uniform, &storage, desc(0), bad));
        }
        let alignment = program
            .0
            .owner
            .device
            .limits()
            .min_storage_buffer_offset_alignment;
        if alignment > 1 {
            invalid(bind(
                &runtime,
                &program,
                &uniform,
                &storage,
                desc(0),
                BufferBindingDesc {
                    offset: 1,
                    ..desc(1)
                },
            ));
        }
        invalid(bind(
            &runtime,
            &program,
            &storage,
            &uniform,
            desc(0),
            desc(1),
        ));
        invalid(runtime.create_bindings(
            &program,
            &[BufferBinding {
                buffer: &uniform,
                desc: desc(0),
            }],
        ));
        invalid(bind(
            &runtime,
            &program,
            &uniform,
            &storage,
            desc(0),
            desc(0),
        ));
        for request in [
            BufferDescriptor::Uniform { bytes: &[] },
            BufferDescriptor::Storage { size: 0 },
            BufferDescriptor::Storage { size: u64::MAX },
        ] {
            assert!(matches!(runtime.create_managed_buffer(request), Err(error)
                if error.kind() == super::super::error::RuntimeErrorKind::InvalidInput));
        }
        let foreign = VulkanRuntime::new(RuntimeConfig::default()).unwrap();
        let foreign_monitor = foreign.validation_monitor();
        let foreign_storage = foreign
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap();
        assert!(
            matches!(bind(&runtime, &program, &uniform, &foreign_storage, desc(0), desc(1)),
            Err(error) if error.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
        );
        let foreign_program = foreign.create_program(&managed).unwrap();
        assert!(
            matches!(bind(&runtime, &foreign_program, &uniform, &storage, desc(0), desc(1)),
            Err(error) if error.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
        );
        drop((foreign_program, foreign_storage, foreign));
        assert!(
            foreign_monitor.report().is_clean(),
            "{:#?}",
            foreign_monitor.report()
        );

        // Ownership alone does not serialize mutable GPU storage. A second
        // preparation is rejected until the first recording cancels/retires.
        let canceled = RecordedTransfer::new(Work::new(bindings.clone()).unwrap()).unwrap();
        assert!(matches!(Work::new(bindings.clone()), Err(error)
            if error.kind() == super::super::error::RuntimeErrorKind::CapacityExhausted));
        drop(canceled);
        let weak_program = Arc::downgrade(&program.0);
        let weak_uniform = Arc::downgrade(&uniform.0);
        let weak_storage = Arc::downgrade(&storage.0);
        let weak_bindings = Arc::downgrade(&bindings.0);
        let recorded = RecordedTransfer::new(Work::new(bindings.clone()).unwrap()).unwrap();
        let newer_program = runtime.create_program(&managed).unwrap();
        bytes[256..].copy_from_slice(&newer_expected);
        let newer_uniform = runtime
            .create_managed_buffer(BufferDescriptor::Uniform { bytes: &bytes })
            .unwrap();
        let newer_storage = runtime
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap();
        let newer_bindings = bind(
            &runtime,
            &newer_program,
            &newer_uniform,
            &newer_storage,
            desc(0),
            desc(1),
        )
        .unwrap();
        let reuse = bind(
            &runtime,
            &newer_program,
            &newer_uniform,
            &storage,
            desc(0),
            desc(1),
        )
        .unwrap();
        assert!(Work::new(reuse.clone()).is_err());
        let newer_recorded = RecordedTransfer::new(Work::new(newer_bindings).unwrap()).unwrap();
        drop((
            program,
            uniform,
            storage,
            newer_program,
            newer_uniform,
            newer_storage,
        ));
        assert!(weak_program.upgrade().is_some());
        assert!(weak_uniform.upgrade().is_some());
        assert!(weak_storage.upgrade().is_some());
        assert!(weak_bindings.upgrade().is_some());
        let mut first = recorded.submit().unwrap();
        let mut second = newer_recorded.submit().unwrap();
        assert!(Work::new(bindings.clone()).is_err());
        drop(bindings);
        let mut first_result = first.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(first_result.read(), expected);
        let reuse_recorded = RecordedTransfer::new(Work::new(reuse).unwrap()).unwrap();
        let mut third = reuse_recorded.submit().unwrap();
        drop(runtime);
        let mut third_result = third.wait(Duration::from_secs(10)).unwrap().unwrap();
        let mut second_result = second.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(second_result.read(), newer_expected);
        assert_eq!(third_result.read(), newer_expected);
        drop((
            first_result,
            second_result,
            third_result,
            first,
            second,
            third,
        ));
        assert!(weak_program.upgrade().is_none());
        assert!(weak_uniform.upgrade().is_none());
        assert!(weak_storage.upgrade().is_none());
        assert!(weak_bindings.upgrade().is_none());
        let report = monitor.report();
        assert!(report.is_clean(), "final native teardown: {report:#?}");
    });
}
