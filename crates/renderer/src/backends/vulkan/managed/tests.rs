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

pub(super) struct StorageLease {
    pub(super) buffer: VulkanBuffer,
    pub(super) reused: bool,
}
impl StorageLease {
    pub(super) fn acquire(buffer: VulkanBuffer) -> Result<Self, RuntimeError> {
        buffer
            .0
            .storage
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RuntimeError::capacity("storage buffer is already recorded or pending"))?;
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
    initializer: Option<Initializer>,
    output: Option<StorageLease>,
    output_index: usize,
    readback: TransferBuffer,
}
struct Initializer {
    bindings: VulkanBindings,
    input: Option<StorageLease>,
    range: BufferRange,
}
impl Work {
    fn new(
        bindings: VulkanBindings,
        initializer: Option<VulkanBindings>,
    ) -> Result<Self, RuntimeError> {
        let output_index = bindings
            .0
            .program
            .layout()
            .iter()
            .position(|b| b.kind == BindingType::StorageWrite)
            .unwrap();
        let output = bindings.0.buffer(output_index).0;
        let initializer = initializer
            .map(|initializer| {
                if !initializer
                    .0
                    .program
                    .0
                    .managed
                    .same_asset(&ManagedProgram::uniform_readback())
                    || !bindings
                        .0
                        .program
                        .0
                        .managed
                        .same_asset(&ManagedProgram::storage_readback())
                    || !Arc::ptr_eq(&initializer.0.program.0.owner, &bindings.0.program.0.owner)
                {
                    return Err(RuntimeError::invalid(
                        "storage input requires the matching uniform initializer",
                    ));
                }
                let input_index = bindings
                    .layout()
                    .iter()
                    .position(|slot| slot.kind == BindingType::StorageRead)
                    .unwrap();
                let written_index = initializer
                    .layout()
                    .iter()
                    .position(|slot| slot.kind == BindingType::StorageWrite)
                    .unwrap();
                let (input, range) = bindings.0.buffer(input_index);
                let (written, written_range) = initializer.0.buffer(written_index);
                // Public descriptors guarantee native ranges. This private boundary
                // proves the trusted producer initializes the exact 16 bytes read,
                // and excludes whole-buffer input/output aliasing before leasing.
                if !Arc::ptr_eq(&input.0, &written.0)
                    || range.offset != written_range.offset
                    || Arc::ptr_eq(&input.0, &output.0)
                {
                    return Err(RuntimeError::invalid(
                        "storage input must match its initializer and differ from output",
                    ));
                }
                let input = StorageLease::acquire(input.clone())?;
                Ok(Initializer {
                    bindings: initializer,
                    input: Some(input),
                    range,
                })
            })
            .transpose()?;
        if initializer.is_none()
            && !bindings
                .0
                .program
                .0
                .managed
                .same_asset(&ManagedProgram::uniform_readback())
        {
            return Err(RuntimeError::invalid(
                "storage read requires an initializer in the same GPU work",
            ));
        }
        let output = StorageLease::acquire(output.clone())?;
        let allocator = VulkanAllocator {
            owner: Arc::clone(&bindings.0.program.0.owner),
        };
        let readback = allocator.create_buffer(BufferRole::Readback, 16).unwrap();
        Ok(Self {
            bindings,
            initializer,
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

unsafe fn record_dispatch(bindings: &VulkanBindings, command: vk::CommandBuffer) {
    let program = &bindings.0.program.0;
    // SAFETY: The caller retains bindings and owns the recording command; its
    // prepared leases/dependencies cover every trusted shader access.
    unsafe {
        program.owner.device.raw().cmd_bind_pipeline(
            command,
            vk::PipelineBindPoint::COMPUTE,
            program.pipeline,
        );
        program.owner.device.raw().cmd_bind_descriptor_sets(
            command,
            vk::PipelineBindPoint::COMPUTE,
            program.layout,
            0,
            &bindings.0.sets,
            &[],
        );
        program.owner.device.raw().cmd_dispatch(command, 1, 1, 1);
    }
}

unsafe fn record_reuse(lease: &StorageLease, command: vk::CommandBuffer) {
    if !lease.reused {
        return;
    }
    let reused = [vk::BufferMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::TRANSFER_READ)
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .buffer(lease.buffer.0.handle)
        .offset(0)
        .size(lease.buffer.len())];
    // SAFETY: The exclusive lease covers prior accepted read/write/copy and the
    // next write. The COMPUTE execution scope also orders reused input's prior
    // consumer-read -> producer-write WAR without requiring a read access bit.
    unsafe {
        lease.buffer.0.owner.device.raw().cmd_pipeline_barrier2(
            command,
            &vk::DependencyInfo::default().buffer_memory_barriers(&reused),
        );
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
        let desc = self.bindings.0.buffer(self.output_index).1;
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
        // SAFETY: This executor exclusively leases both mutable Storage buffers
        // through cancel or actual retirement. Prepared asset/range identity
        // proves initialization of the exact 16 bytes read between dispatches.
        // Immutable sets/pipelines/buffers remain retained. Prior accepted uses
        // precede reuse, producer writes precede reads, and output precedes copy
        // and host access. No wait/readback separates the two dispatches.
        unsafe {
            record_reuse(output, command);
            if let Some(initializer) = &self.initializer {
                let input = initializer.input.as_ref().unwrap();
                record_reuse(input, command);
                record_dispatch(&initializer.bindings, command);
                let to_read = [vk::BufferMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(input.buffer.0.handle)
                    .offset(initializer.range.offset)
                    .size(16)];
                device.raw().cmd_pipeline_barrier2(
                    command,
                    &vk::DependencyInfo::default().buffer_memory_barriers(&to_read),
                );
            }
            record_dispatch(&self.bindings, command);
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
        if let Some(initializer) = &self.initializer {
            initializer
                .input
                .as_ref()
                .unwrap()
                .buffer
                .0
                .storage
                .used
                .store(true, Ordering::Release);
        }
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
        if let Some(initializer) = &mut self.initializer {
            initializer.input.take();
        }
        self.output.take();
    }
}

fn desc(_group: u32) -> BufferRange {
    BufferRange {
        offset: 256,
        size: 16,
    }
}
fn buffer_binding(
    group: u32,
    buffer: &VulkanBuffer,
    range: BufferRange,
) -> Binding<'_, VulkanBuffer, VulkanImageView, VulkanSampler> {
    Binding {
        group,
        binding: 0,
        resource: BindingResource::Buffer {
            buffer,
            offset: range.offset,
            size: range.size,
        },
    }
}
fn bind(
    runtime: &VulkanRuntime,
    program: &VulkanProgram,
    uniform: &VulkanBuffer,
    storage: &VulkanBuffer,
    uniform_desc: BufferRange,
    storage_desc: BufferRange,
) -> Result<VulkanBindings, RuntimeError> {
    runtime.create_bindings(
        program,
        &[
            buffer_binding(1, storage, storage_desc),
            buffer_binding(0, uniform, uniform_desc),
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
    run_storage_versions(false);
}

#[test]
#[ignore = "explicit real-GPU producer write to StorageRead dispatch, versions, exclusion and reuse; run strict and shipping separately"]
fn vulkan_managed_storage_read_creation_and_versions() {
    run_storage_versions(true);
}

fn execution_bindings(
    runtime: &VulkanRuntime,
    writer: &VulkanProgram,
    reader: Option<&VulkanProgram>,
    uniform: &VulkanBuffer,
    input: Option<&VulkanBuffer>,
    output: &VulkanBuffer,
) -> (VulkanBindings, Option<VulkanBindings>) {
    let initialization = bind(
        runtime,
        writer,
        uniform,
        input.unwrap_or(output),
        desc(0),
        desc(1),
    )
    .unwrap();
    match reader {
        Some(reader) => (
            bind(runtime, reader, input.unwrap(), output, desc(0), desc(1)).unwrap(),
            Some(initialization),
        ),
        None => (initialization, None),
    }
}

fn run_storage_versions(storage_read: bool) {
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
        let reader = storage_read.then(|| {
            runtime
                .create_program(&ManagedProgram::storage_readback())
                .unwrap()
        });
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
        let input = storage_read.then(|| {
            runtime
                .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
                .unwrap()
        });
        assert_eq!(uniform.len(), 272);
        assert_eq!(storage.len(), 272);
        assert_eq!(uniform.usage(), BufferUsage::Uniform);
        assert_eq!(storage.usage(), BufferUsage::Storage);
        bytes[256..].fill(0);
        let (bindings, initializer) = execution_bindings(
            &runtime,
            &program,
            reader.as_ref(),
            &uniform,
            input.as_ref(),
            &storage,
        );
        if storage_read {
            let reader = reader.as_ref().unwrap();
            let input = input.as_ref().unwrap();
            invalid(bind(&runtime, reader, &uniform, &storage, desc(0), desc(1)));
            let alias = bind(&runtime, reader, input, input, desc(0), desc(1)).unwrap();
            assert!(matches!(Work::new(alias, initializer.clone()), Err(error)
                if error.kind() == super::super::error::RuntimeErrorKind::InvalidInput));
            let different_range = bind(
                &runtime,
                reader,
                input,
                &storage,
                BufferRange {
                    offset: 0,
                    size: 16,
                },
                desc(1),
            )
            .unwrap();
            assert!(Work::new(different_range, initializer.clone()).is_err());
            let wrong_input =
                bind(&runtime, &program, &uniform, &storage, desc(0), desc(1)).unwrap();
            assert!(Work::new(bindings.clone(), Some(wrong_input)).is_err());
            assert!(Work::new(bindings.clone(), None).is_err());
        } else {
            assert_eq!(bindings.layout(), managed.bindings());
            for bad in [
                BufferRange { size: 0, ..desc(1) },
                BufferRange {
                    size: 15,
                    ..desc(1)
                },
                BufferRange {
                    size: 17,
                    ..desc(1)
                },
                BufferRange {
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
                    BufferRange {
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
            invalid(runtime.create_bindings(&program, &[buffer_binding(0, &uniform, desc(0))]));
            invalid(runtime.create_bindings(
                &program,
                &[
                    buffer_binding(0, &uniform, desc(0)),
                    buffer_binding(0, &storage, desc(1)),
                ],
            ));
            invalid(runtime.create_bindings(
                &program,
                &[
                    buffer_binding(0, &uniform, desc(0)),
                    buffer_binding(2, &storage, desc(1)),
                ],
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
        }

        // Ownership alone does not serialize mutable GPU storage. A second
        // preparation is rejected until the first recording cancels/retires.
        let canceled =
            RecordedTransfer::new(Work::new(bindings.clone(), initializer.clone()).unwrap())
                .unwrap();
        assert!(
            matches!(Work::new(bindings.clone(), initializer.clone()), Err(error)
            if error.kind() == super::super::error::RuntimeErrorKind::CapacityExhausted)
        );
        drop(canceled);
        let weak_program = Arc::downgrade(&program.0);
        let weak_uniform = Arc::downgrade(&uniform.0);
        let weak_storage = Arc::downgrade(&storage.0);
        let weak_bindings = Arc::downgrade(&bindings.0);
        let weak_input = input.as_ref().map(|input| Arc::downgrade(&input.0));
        let weak_reader = reader.as_ref().map(|reader| Arc::downgrade(&reader.0));
        let recorded =
            RecordedTransfer::new(Work::new(bindings.clone(), initializer.clone()).unwrap())
                .unwrap();
        let newer_program = runtime.create_program(&managed).unwrap();
        let newer_reader = storage_read.then(|| {
            runtime
                .create_program(&ManagedProgram::storage_readback())
                .unwrap()
        });
        bytes[256..].copy_from_slice(&newer_expected);
        let newer_uniform = runtime
            .create_managed_buffer(BufferDescriptor::Uniform { bytes: &bytes })
            .unwrap();
        let newer_storage = runtime
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap();
        let newer_input = storage_read.then(|| {
            runtime
                .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
                .unwrap()
        });
        let (newer_bindings, newer_initializer) = execution_bindings(
            &runtime,
            &newer_program,
            newer_reader.as_ref(),
            &newer_uniform,
            newer_input.as_ref(),
            &newer_storage,
        );
        let (reuse, reuse_initializer) = execution_bindings(
            &runtime,
            &newer_program,
            newer_reader.as_ref(),
            &newer_uniform,
            input.as_ref(),
            &storage,
        );
        assert!(Work::new(reuse.clone(), reuse_initializer.clone()).is_err());
        let newer_recorded =
            RecordedTransfer::new(Work::new(newer_bindings, newer_initializer).unwrap()).unwrap();
        drop((
            program,
            uniform,
            storage,
            reader,
            input,
            newer_program,
            newer_uniform,
            newer_storage,
            newer_reader,
            newer_input,
        ));
        assert!(weak_program.upgrade().is_some());
        assert!(weak_uniform.upgrade().is_some());
        assert!(weak_storage.upgrade().is_some());
        assert!(weak_bindings.upgrade().is_some());
        if let Some(weak) = &weak_input {
            assert!(weak.upgrade().is_some());
        }
        if let Some(weak) = &weak_reader {
            assert!(weak.upgrade().is_some());
        }
        let mut first = recorded.submit().unwrap();
        let mut second = newer_recorded.submit().unwrap();
        assert!(Work::new(bindings.clone(), initializer.clone()).is_err());
        drop((bindings, initializer));
        let mut first_result = first.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(first_result.read(), expected);
        let reuse_recorded =
            RecordedTransfer::new(Work::new(reuse, reuse_initializer).unwrap()).unwrap();
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
        if let Some(weak) = &weak_input {
            assert!(weak.upgrade().is_none());
        }
        if let Some(weak) = &weak_reader {
            assert!(weak.upgrade().is_none());
        }
        let report = monitor.report();
        assert!(report.is_clean(), "final native teardown: {report:#?}");
    });
}
