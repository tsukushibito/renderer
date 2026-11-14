//! Actual nearest/clamp sampling with public image, View and sampler owners.

use super::tests::StorageLease;
use super::*;
use crate::backends::vulkan::buffer::{BufferRole, VulkanBuffer as TransferBuffer};
use crate::backends::vulkan::graph::{RuntimeConfig, VulkanRuntime};
use crate::backends::vulkan::memory::VulkanAllocator;
use crate::backends::vulkan::transfer::{
    RecordedTransfer, TransferError, TransferPayload, record_host_read,
};
use crate::program::{ImageDescriptor, ImageFormat, ImageViewDescriptor, SamplerDescriptor};
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use std::time::Duration;

struct ImageLease {
    image: VulkanImage,
}
impl ImageLease {
    fn acquire(image: VulkanImage) -> Result<Self, RuntimeError> {
        image
            .0
            .state
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RuntimeError::capacity("sampled image is already recorded or pending"))?;
        Ok(Self { image })
    }
}
impl Drop for ImageLease {
    fn drop(&mut self) {
        self.image.0.state.busy.store(false, Ordering::Release);
    }
}
struct Work {
    bindings: VulkanBindings,
    image: Option<ImageLease>,
    output: Option<StorageLease>,
    output_index: usize,
    upload: Option<TransferBuffer>,
    readback: TransferBuffer,
}
impl Work {
    fn new(bindings: VulkanBindings, pixels: Option<&[u8]>) -> Result<Self, RuntimeError> {
        let image = bindings
            .0
            .resources
            .iter()
            .find_map(|resource| match resource {
                BoundResource::ImageView(view) => Some(view.0.image.clone()),
                _ => None,
            })
            .unwrap();
        let image = ImageLease::acquire(image)?;
        let initialized = image.image.0.state.initialized.load(Ordering::Acquire);
        if initialized == pixels.is_some() {
            return Err(RuntimeError::invalid(
                "supply initial pixels exactly once, then reuse immutable sampled contents",
            ));
        }
        // This fixture's byte input is exactly the selected 2x2 image in its
        // actual texel format. General Graph upload remains subsequent work.
        let desc = image.image.desc();
        let upload_size = match desc.format {
            ImageFormat::Rgba8Unorm => 16,
            ImageFormat::Rgba16Float => 32,
        };
        if desc.width != 2
            || desc.height != 2
            || pixels.is_some_and(|bytes| bytes.len() != upload_size)
        {
            return Err(RuntimeError::invalid(
                "sampling fixture requires exact format-sized 2x2 pixels",
            ));
        }
        let output_index = bindings
            .layout()
            .iter()
            .position(|slot| slot.kind == BindingType::StorageWrite)
            .unwrap();
        let output = StorageLease::acquire(bindings.0.buffer(output_index).0.clone())?;
        let output_size = bindings.layout()[output_index].min_size;
        let allocator = VulkanAllocator {
            owner: Arc::clone(&bindings.0.program.0.owner),
        };
        let upload = pixels.map(|pixels| {
            let mut upload = allocator
                .create_buffer(BufferRole::Upload, pixels.len() as u64)
                .unwrap();
            upload.write(0, pixels).unwrap();
            upload
        });
        let readback = allocator
            .create_buffer(BufferRole::Readback, output_size)
            .unwrap();
        Ok(Self {
            bindings,
            image: Some(image),
            output: Some(output),
            output_index,
            upload,
            readback,
        })
    }
    fn read(&mut self) -> Vec<u8> {
        let mut bytes = vec![0; self.readback.size() as usize];
        self.readback.read(0, &mut bytes).unwrap();
        bytes
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
        self.upload.as_ref().map_or(0, TransferBuffer::size) + self.readback.size()
    }
    fn label(&self) -> &'static CStr {
        c"managed.sampled-readback"
    }
    unsafe fn record(&self, command: vk::CommandBuffer) {
        let program = &self.bindings.0.program.0;
        let device = &program.owner.device;
        let image = &self.image.as_ref().unwrap().image;
        let output = self.output.as_ref().unwrap();
        let range = self.bindings.0.buffer(self.output_index).1;
        let image_range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        // SAFETY: This payload retains the complete binding chain and exclusive
        // leases for image initialization/sampling and Storage writes. Reviewed
        // shader coordinates are finite and sample with normalized nearest/clamp.
        // Copies use the actual 2x2 upload format and the reviewed shader's
        // 16/64-byte output range. No raw owner escapes.
        unsafe {
            if let Some(upload) = &self.upload {
                let initialize = [vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::COPY)
                    .src_access_mask(vk::AccessFlags2::NONE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                    .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.0.handle)
                    .subresource_range(image_range)];
                // Preserve the existing RTX3060 first-image transition COPY
                // execution workaround; no source memory access or host wait.
                device.raw().cmd_pipeline_barrier2(
                    command,
                    &vk::DependencyInfo::default().image_memory_barriers(&initialize),
                );
                device.raw().cmd_copy_buffer_to_image(
                    command,
                    upload.transfer_handle(),
                    image.0.handle,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .layer_count(1),
                        )
                        .image_extent(vk::Extent3D {
                            width: 2,
                            height: 2,
                            depth: 1,
                        })],
                );
                let sampled = [vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::COPY)
                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.0.handle)
                    .subresource_range(image_range)];
                device.raw().cmd_pipeline_barrier2(
                    command,
                    &vk::DependencyInfo::default().image_memory_barriers(&sampled),
                );
            }
            if output.reused {
                let reused = [vk::BufferMemoryBarrier2::default()
                    .src_stage_mask(
                        vk::PipelineStageFlags2::COPY | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    )
                    .src_access_mask(
                        vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                    )
                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(output.buffer.0.handle)
                    .offset(0)
                    .size(output.buffer.len())];
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
            let copied = [vk::BufferMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(output.buffer.0.handle)
                .offset(range.offset)
                .size(self.readback.size())];
            device.raw().cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().buffer_memory_barriers(&copied),
            );
            device.raw().cmd_copy_buffer(
                command,
                output.buffer.0.handle,
                self.readback.transfer_handle(),
                &[vk::BufferCopy::default()
                    .src_offset(range.offset)
                    .size(self.readback.size())],
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
        if self.upload.is_some() {
            self.image
                .as_ref()
                .unwrap()
                .image
                .0
                .state
                .initialized
                .store(true, Ordering::Release);
        }
    }
    unsafe fn establish_contents(&mut self) {
        // SAFETY: Actual successful Fence wait on active Device proved both
        // image transfer and shader/copy commands completed, including all bytes.
        unsafe { self.readback.establish_gpu_contents() };
        self.image.take();
        self.output.take();
    }
}
fn image_desc(format: ImageFormat) -> ImageDescriptor {
    ImageDescriptor {
        width: 2,
        height: 2,
        format,
    }
}
fn bind(
    runtime: &VulkanRuntime,
    program: &VulkanProgram,
    view: &VulkanImageView,
    sampler: &VulkanSampler,
    output: &VulkanBuffer,
) -> Result<VulkanBindings, RuntimeError> {
    let output_size = program
        .layout()
        .iter()
        .find(|slot| slot.kind == BindingType::StorageWrite)
        .unwrap()
        .min_size;
    runtime.create_bindings(
        program,
        &[
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: output,
                    offset: 256,
                    size: output_size,
                },
            },
            Binding {
                group: 0,
                binding: 1,
                resource: BindingResource::Sampler(sampler),
            },
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::ImageView(view),
            },
        ],
    )
}

#[test]
#[ignore = "explicit real-GPU managed nearest/clamp sampling, image/View/sampler owners and reuse; run strict/shipping separately"]
fn vulkan_managed_sampled_creation_and_versions() {
    let pixels = crate::program::SAMPLE_TEST_DATA;
    run_sampled_versions(
        ManagedProgram::sampled_readback(),
        ImageFormat::Rgba8Unorm,
        [&pixels[0], &pixels[1]],
        [&pixels[0], &pixels[1]],
    );
}

#[test]
#[ignore = "explicit real-GPU RGBA16F sampling, binary32 output, versions and reuse; run strict/shipping separately"]
fn vulkan_managed_sampled_float_creation_and_versions() {
    let pixels = crate::program::SAMPLE_FLOAT_TEST_DATA;
    let expected = crate::program::SAMPLE_FLOAT_EXPECTED.map(|words| {
        words
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    });
    run_sampled_versions(
        ManagedProgram::sampled_float_readback(),
        ImageFormat::Rgba16Float,
        [&pixels[0], &pixels[1]],
        [&expected[0], &expected[1]],
    );
}

fn run_sampled_versions(
    managed: ManagedProgram,
    format: ImageFormat,
    pixels: [&[u8]; 2],
    expected: [&[u8]; 2],
) {
    let [old, new] = pixels;
    let [old_expected, new_expected] = expected;
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
        let program = runtime.create_program(&managed).unwrap();
        let output_size = program
            .layout()
            .iter()
            .find(|slot| slot.kind == BindingType::StorageWrite)
            .unwrap()
            .min_size;
        let image = runtime.create_managed_image(image_desc(format)).unwrap();
        let view = runtime
            .create_image_view(&image, ImageViewDescriptor::default())
            .unwrap();
        let sampler = runtime
            .create_sampler(SamplerDescriptor::NearestClamp)
            .unwrap();
        let output = runtime
            .create_managed_buffer(BufferDescriptor::Storage {
                size: 256 + output_size,
            })
            .unwrap();
        assert_eq!(image.desc(), image_desc(format));
        assert_eq!(view.desc(), ImageViewDescriptor::default());
        assert_eq!(sampler.desc(), SamplerDescriptor::NearestClamp);
        let bindings = bind(&runtime, &program, &view, &sampler, &output).unwrap();
        assert_eq!(bindings.layout(), managed.bindings());
        if format == ImageFormat::Rgba8Unorm {
            assert!(
                matches!(runtime.create_image_view(&image, ImageViewDescriptor { base_mip_level: 1, ..Default::default() }),
            Err(e) if e.kind() == super::super::error::RuntimeErrorKind::InvalidInput)
            );
            assert!(
                matches!(runtime.create_managed_image(ImageDescriptor { width: 0, ..image_desc(format) }),
            Err(e) if e.kind() == super::super::error::RuntimeErrorKind::InvalidInput)
            );
            let invalid_kind = runtime.create_bindings(
                &program,
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::Sampler(&sampler),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::ImageView(&view),
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: &output,
                            offset: 256,
                            size: 16,
                        },
                    },
                ],
            );
            assert!(
                matches!(invalid_kind, Err(e) if e.kind() == super::super::error::RuntimeErrorKind::InvalidInput)
            );
            let foreign = VulkanRuntime::new(RuntimeConfig::default()).unwrap();
            let foreign_monitor = foreign.validation_monitor();
            assert!(
                matches!(foreign.create_image_view(&image, ImageViewDescriptor::default()), Err(e) if e.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
            );
            let foreign_sampler = foreign
                .create_sampler(SamplerDescriptor::NearestClamp)
                .unwrap();
            assert!(
                matches!(bind(&runtime, &program, &view, &foreign_sampler, &output), Err(e) if e.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
            );
            let foreign_image = foreign.create_managed_image(image_desc(format)).unwrap();
            let foreign_view = foreign
                .create_image_view(&foreign_image, ImageViewDescriptor::default())
                .unwrap();
            assert!(
                matches!(bind(&runtime, &program, &foreign_view, &sampler, &output), Err(e) if e.kind() == super::super::error::RuntimeErrorKind::WrongOwner)
            );
            drop((foreign_sampler, foreign_view, foreign_image, foreign));
            assert!(
                foreign_monitor.report().is_clean(),
                "{:#?}",
                foreign_monitor.report()
            );
        }
        let canceled =
            RecordedTransfer::new(Work::new(bindings.clone(), Some(old)).unwrap()).unwrap();
        assert!(Work::new(bindings.clone(), Some(old)).is_err());
        drop(canceled);
        let weak_image = Arc::downgrade(&image.0);
        let weak_view = Arc::downgrade(&view.0);
        let weak_sampler = Arc::downgrade(&sampler.0);
        let recorded =
            RecordedTransfer::new(Work::new(bindings.clone(), Some(old)).unwrap()).unwrap();
        let newer_program = runtime.create_program(&managed).unwrap();
        let newer_image = runtime.create_managed_image(image_desc(format)).unwrap();
        let newer_view = runtime
            .create_image_view(&newer_image, ImageViewDescriptor::default())
            .unwrap();
        let newer_output = runtime
            .create_managed_buffer(BufferDescriptor::Storage {
                size: 256 + output_size,
            })
            .unwrap();
        let newer_bindings = bind(
            &runtime,
            &newer_program,
            &newer_view,
            &sampler,
            &newer_output,
        )
        .unwrap();
        let newer_recorded =
            RecordedTransfer::new(Work::new(newer_bindings, Some(new)).unwrap()).unwrap();
        drop((
            program,
            image,
            view,
            sampler,
            output,
            newer_program,
            newer_image,
            newer_view,
            newer_output,
        ));
        let mut first = recorded.submit().unwrap();
        let mut second = newer_recorded.submit().unwrap();
        assert!(Work::new(bindings.clone(), None).is_err());
        let mut first_result = first.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(first_result.read(), old_expected);
        let reused = RecordedTransfer::new(Work::new(bindings, None).unwrap()).unwrap();
        let mut third = reused.submit().unwrap();
        drop(runtime);
        let mut third_result = third.wait(Duration::from_secs(10)).unwrap().unwrap();
        let mut second_result = second.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(second_result.read(), new_expected);
        assert_eq!(third_result.read(), old_expected);
        assert!(weak_image.upgrade().is_some());
        assert!(weak_view.upgrade().is_some());
        assert!(weak_sampler.upgrade().is_some());
        drop((
            first_result,
            second_result,
            third_result,
            first,
            second,
            third,
        ));
        assert!(weak_image.upgrade().is_none());
        assert!(weak_view.upgrade().is_none());
        assert!(weak_sampler.upgrade().is_none());
        let report = monitor.report();
        assert!(report.is_clean(), "final native teardown: {report:#?}");
    });
}
