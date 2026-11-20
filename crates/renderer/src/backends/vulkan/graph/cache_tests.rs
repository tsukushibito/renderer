//! Cache policy and real-GPU descriptor churn, pressure and failure regressions.

use super::tests::prepare_scenario;
use super::*;
use crate::graph::{BufferDesc, Graph, ImageDesc};
use crate::vulkan::ValidationMode;

const WAIT: Duration = Duration::from_secs(10);

fn real_gpu(
    capacity: usize,
    slots: usize,
    validation: ValidationMode,
    test: impl FnOnce(&mut VulkanRuntime),
) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: slots,
                resource_capacity: capacity,
                staging_bytes: 4096,
                max_shader_bindings: 1,
                staging_ranges: 256,
            },
            validation,
        )
        .expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        if validation == ValidationMode::Required {
            crate::backends::vulkan::assert_real_validation(&runtime.diagnostics);
        }
        let monitor = runtime.validation_monitor();
        test(&mut runtime);
        drop(runtime);
        let report = monitor.report();
        assert_eq!(report.requested, validation);
        assert!(
            report.is_clean(),
            "validation through shutdown: {report:#?}"
        );
    });
}

fn image_plan(size: u32, value: u8) -> (CompiledGraph, ImageHandle, PassHandle) {
    let mut graph = Graph::test_new();
    let image = graph
        .image("cached image", ImageDesc::rgba8(size, size))
        .unwrap();
    graph
        .upload_image("upload", image, &vec![value; (size * size * 4) as usize])
        .unwrap();
    let read = graph.readback_image("read", image).unwrap();
    graph.export_image(image).unwrap();
    (graph.compile().unwrap(), image, read)
}

fn check_result(submission: &mut Submission, read: PassHandle, size: u32, value: u8) {
    let mut result = submission
        .wait(WAIT)
        .unwrap()
        .expect("GPU image operation must complete");
    assert_eq!(
        result.test_readback(read).unwrap(),
        vec![value; (size * size * 4) as usize]
    );
}

fn complete_image(runtime: &mut VulkanRuntime, size: u32, value: u8) -> ResourceKey {
    let (plan, image, read) = image_plan(size, value);
    let mut submission = runtime.submit(&plan).unwrap();
    let key = submission.export_image(image).unwrap().key();
    runtime.delete(key).unwrap();
    check_result(&mut submission, read, size, value);
    key
}

fn prepare_images(runtime: &mut VulkanRuntime, sizes: &[u32]) {
    let classes = sizes
        .iter()
        .map(|&size| ResourceClass::Image {
            desc: ImageDesc::rgba8(size, size),
            count: 1,
        })
        .collect::<Vec<_>>();
    prepare_scenario(runtime, 1, 2, &classes);
}

#[test]
#[ignore = "explicit real-GPU working-set failure recovery in strict and shipping builds"]
fn vulkan_graph_working_set_failure_preserves_committed_resources() {
    real_gpu(3, 1, ValidationMode::default(), |runtime| {
        let class = |size| ResourceClass::Buffer {
            desc: BufferDesc { size },
            count: 1,
        };
        prepare_scenario(runtime, 1, 4, &[class(4), class(8)]);
        let capacity = runtime.frame_capacity.unwrap();
        let key = {
            let imported = runtime
                .create_buffer("registered", BufferDesc { size: 12 })
                .unwrap();
            let mut graph = Graph::test_new();
            let buffer = graph.import_buffer("registered", &imported).unwrap();
            graph
                .upload_buffer("first", buffer, 0, &[1, 2, 3, 4])
                .unwrap();
            graph
                .upload_buffer("second", buffer, 8, &[5, 6, 7, 8])
                .unwrap();
            let first = graph.readback_buffer("first result", buffer, 0, 4).unwrap();
            let second = graph
                .readback_buffer("second result", buffer, 8, 4)
                .unwrap();
            let plan = graph.compile().unwrap();
            let mut submission = runtime.submit(&plan).unwrap();
            let mut result = submission.wait(WAIT).unwrap().unwrap();
            assert_eq!(result.test_readback(first).unwrap(), [1, 2, 3, 4]);
            assert_eq!(result.test_readback(second).unwrap(), [5, 6, 7, 8]);
            imported.key()
        };
        let before = runtime.stats();
        let baseline = runtime.memory_stats().unwrap();
        let invalid = runtime.prepare_working_set(capacity, &[class(16), class(0)]);
        assert_eq!(invalid.unwrap_err().kind(), RuntimeErrorKind::InvalidInput);

        let insufficient = FrameCapacity {
            initialized_ranges_per_resource: 1,
            ..capacity
        };
        let error = runtime
            .prepare_working_set(insufficient, &[class(16)])
            .unwrap_err();
        let exhausted = error.capacity().unwrap();
        assert_eq!(exhausted.resource, RuntimeCapacityKind::GraphMetadata);
        assert_eq!(exhausted.recovery, CapacityRecovery::Reconfigure);

        let mut overlap = None;
        let error = runtime
            .prepare_working_set_with(
                capacity,
                &[class(16), class(20)],
                None,
                |plan, owner, name, snapshot| {
                    if overlap.is_some() {
                        return Err(RuntimeError::native(
                            "injected allocation failure",
                            owner.device.name(),
                            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
                        ));
                    }
                    let staged = plan.create(owner, name, snapshot)?;
                    overlap = Some(owner.raw.calculate_statistics().unwrap().total.statistics);
                    Ok(staged)
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Native);
        assert!(error.to_string().contains("injected allocation failure"));
        let overlap = overlap.expect("first replacement was allocated before the failure");
        assert_eq!(overlap.allocationCount, baseline.vma_allocation_count + 1);
        assert!(overlap.allocationBytes > baseline.vma_allocation_bytes);
        let after = runtime.memory_stats().unwrap();
        assert_eq!(after.vma_allocation_count, baseline.vma_allocation_count);
        assert_eq!(after.vma_allocation_bytes, baseline.vma_allocation_bytes);
        println!(
            "working set VMA allocations before/overlap/after: {} ({}) / {} ({}) / {} ({})",
            baseline.vma_allocation_count,
            baseline.vma_allocation_bytes,
            overlap.allocationCount,
            overlap.allocationBytes,
            after.vma_allocation_count,
            after.vma_allocation_bytes,
        );
        let after = runtime.stats();
        assert_eq!(after.resources_created, before.resources_created);
        assert_eq!(after.resources_reused, before.resources_reused);
        assert_eq!(after.registered_resources, before.registered_resources);

        let read_class = |runtime: &mut VulkanRuntime, size: usize| {
            let mut graph = Graph::test_new();
            let buffer = graph
                .buffer("prepared class", BufferDesc { size: size as u64 })
                .unwrap();
            let bytes = [19; 20];
            graph
                .upload_buffer("upload", buffer, 0, &bytes[..size])
                .unwrap();
            let read = graph
                .readback_buffer("read", buffer, 0, size as u64)
                .unwrap();
            let plan = graph.compile().unwrap();
            let mut submission = runtime.submit(&plan).unwrap();
            let mut result = submission.wait(WAIT).unwrap().unwrap();
            assert_eq!(result.test_readback(read).unwrap(), bytes[..size]);
        };
        read_class(runtime, 4);
        read_class(runtime, 8);
        {
            let imported = runtime.refresh_buffer(key).unwrap();
            let mut graph = Graph::test_new();
            let buffer = graph
                .import_buffer("retained registration", &imported)
                .unwrap();
            let first = graph.readback_buffer("first", buffer, 0, 4).unwrap();
            let second = graph.readback_buffer("second", buffer, 8, 4).unwrap();
            let plan = graph.compile().unwrap();
            let mut submission = runtime.submit(&plan).unwrap();
            let mut result = submission.wait(WAIT).unwrap().unwrap();
            assert_eq!(result.test_readback(first).unwrap(), [1, 2, 3, 4]);
            assert_eq!(result.test_readback(second).unwrap(), [5, 6, 7, 8]);
        }
        runtime
            .prepare_working_set(capacity, &[class(16), class(20)])
            .unwrap();
        read_class(runtime, 16);
        read_class(runtime, 20);
        runtime.delete(key).unwrap();
    });
}

#[test]
#[ignore = "explicit real-GPU transaction covering an invalid Raster working-set replacement"]
fn vulkan_graph_working_set_program_failure_rolls_back_resources() {
    use crate::program::{
        ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups, MAX_RASTER_PROGRAMS,
        MESH_RASTER_DESCRIPTOR, ManagedProgram, ManagedRasterProgram, RasterProgramDescriptor,
        ShaderBinaries,
    };

    static DESCRIPTORS: [RasterProgramDescriptor; MAX_RASTER_PROGRAMS + 1] = [const {
        RasterProgramDescriptor {
            ..MESH_RASTER_DESCRIPTOR
        }
    };
        MAX_RASTER_PROGRAMS + 1];
    static RASTER_METADATA: RasterProgramDescriptor = RasterProgramDescriptor {
        shaders: ShaderBinaries::metadata_only(),
        ..MESH_RASTER_DESCRIPTOR
    };
    static COMPUTE_METADATA: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [1; 3],
        bindings: &[],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Fixed([1; 3]),
            bytes: &[],
        },
        shaders: ShaderBinaries::metadata_only(),
    };

    real_gpu(4, 1, ValidationMode::default(), |runtime| {
        let class = |size| ResourceClass::Image {
            desc: ImageDesc::rgba8(size, size),
            count: 1,
        };
        prepare_scenario(runtime, 1, 2, &[class(4), class(6)]);
        runtime
            .prepare_graph_working_set(
                None,
                WorkingSet {
                    resources: &[class(4), class(6)],
                    raster: &[ManagedRasterProgram::mesh().into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let registered = runtime
            .create_buffer("registered before resize", BufferDesc { size: 4 })
            .unwrap()
            .key();
        let raster = Arc::clone(runtime.programs.as_ref().unwrap());
        let before = runtime.stats();
        let memory = runtime.memory_stats().unwrap();
        // SAFETY: These descriptors carry only immutable CPU Graph metadata.
        let missing_raster =
            unsafe { ManagedRasterProgram::from_trusted_descriptor(&RASTER_METADATA) };
        // SAFETY: The immutable metadata describes a one-workgroup dispatch with no resources.
        let missing_compute = unsafe { ManagedProgram::from_trusted_descriptor(&COMPUTE_METADATA) };
        let compute = Arc::clone(runtime.compute_programs.as_ref().unwrap());
        let missing_raster_request = missing_raster.clone().into();
        for error in [
            runtime
                .prepare_raster_programs(&[missing_raster])
                .unwrap_err(),
            runtime
                .prepare_compute_programs(std::slice::from_ref(&missing_compute))
                .unwrap_err(),
            runtime
                .prepare_graph_working_set(
                    None,
                    WorkingSet {
                        resources: &[class(8)],
                        raster: &[missing_raster_request],
                        ..Default::default()
                    },
                )
                .unwrap_err(),
            runtime
                .prepare_graph_working_set(
                    None,
                    WorkingSet {
                        resources: &[class(8)],
                        compute: &[missing_compute],
                        ..Default::default()
                    },
                )
                .unwrap_err(),
        ] {
            assert_eq!(error.kind(), RuntimeErrorKind::Unsupported);
            assert!(error.to_string().contains("has no SPIR-V"), "{error}");
        }
        assert!(Arc::ptr_eq(&raster, runtime.programs.as_ref().unwrap()));
        assert!(Arc::ptr_eq(
            &compute,
            runtime.compute_programs.as_ref().unwrap()
        ));
        assert_eq!(runtime.stats().resources_created, before.resources_created);
        assert_eq!(
            runtime.memory_stats().unwrap().vma_allocation_count,
            memory.vma_allocation_count
        );
        let invalid = DESCRIPTORS.each_ref().map(|descriptor| {
            // SAFETY: Each descriptor reproduces the complete immutable
            // built-in mesh Program byte-for-byte, differing in identity only.
            unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }.into()
        });
        let error = runtime
            .prepare_graph_working_set(
                None,
                WorkingSet {
                    resources: &[class(8)],
                    raster: &invalid,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(
            error.capacity().unwrap().recovery,
            CapacityRecovery::Reconfigure
        );
        assert!(Arc::ptr_eq(&raster, runtime.programs.as_ref().unwrap()));
        assert_eq!(runtime.stats().resources_created, before.resources_created);
        assert_eq!(
            runtime.memory_stats().unwrap().vma_allocation_count,
            memory.vma_allocation_count
        );
        complete_image(runtime, 4, 23);
        complete_image(runtime, 6, 27);

        let before = runtime.stats();
        runtime
            .prepare_graph_working_set(
                None,
                WorkingSet {
                    resources: &[class(4), class(8)],
                    raster: &[ManagedRasterProgram::mesh().into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let after = runtime.stats();
        assert_eq!(after.resources_reused, before.resources_reused + 1);
        assert_eq!(after.resources_created, before.resources_created + 1);
        assert_eq!(
            runtime.memory_stats().unwrap().vma_allocation_count,
            memory.vma_allocation_count,
            "the old extent must be released when the replacement commits"
        );
        assert_eq!(
            runtime.refresh_buffer(registered).unwrap().key(),
            registered
        );
        runtime.delete(registered).unwrap();
        complete_image(runtime, 4, 23);
        complete_image(runtime, 8, 31);
    });
}

/// Asset replacement is explicit even while other prepared frames are pending.
fn replace_image_asset(runtime: &mut VulkanRuntime, size: u32) {
    let (_, lease) = runtime
        .acquire_inner(
            ResourceDesc::Image(ImageDesc::rgba8(size, size)),
            "replacement asset",
            true,
        )
        .unwrap();
    drop(lease);
}

#[test]
#[ignore = "explicit real-GPU alternating cache descriptors; requires Core/SyncVal"]
fn vulkan_graph_cache_retains_alternating_image_extents() {
    real_gpu(8, 1, ValidationMode::Required, |runtime| {
        prepare_images(runtime, &[2, 3]);
        for frame in 0..8 {
            let size = if frame % 2 == 0 { 2 } else { 3 };
            complete_image(runtime, size, frame + 1);
        }
        assert_eq!(runtime.stats().resources_created, 2);
        assert_eq!(runtime.stats().resources_reused, 8);
        assert_eq!(runtime.entries.len(), 2);
        assert_eq!(runtime.memory_stats().unwrap().cached_logical_bytes, 52);
    });
}

#[test]
#[ignore = "explicit real-GPU cache eviction and generations; requires Core/SyncVal"]
fn vulkan_graph_cache_pressure_preserves_recent_descriptors_and_generations() {
    real_gpu(2, 1, ValidationMode::Required, |runtime| {
        prepare_images(runtime, &[2, 3]);
        let first_a = complete_image(runtime, 2, 11);
        let first_b = complete_image(runtime, 3, 22);
        let second_a = complete_image(runtime, 2, 33);
        assert_eq!(second_a.index, first_a.index);
        assert_eq!(second_a.generation, first_a.generation + 1);
        // Changing the prepared envelope may replace idle native allocations;
        // ordinary submit only reuses already prepared descriptor classes.
        prepare_images(runtime, &[4]);
        let c = complete_image(runtime, 4, 44);
        assert_eq!(c.index, first_b.index, "replace B, not recently reused A");
        assert!(c.generation > first_b.generation);
        let third_a = complete_image(runtime, 2, 55);
        assert_eq!(third_a.index, first_a.index);
        assert_eq!(third_a.generation, second_a.generation + 1);
        assert_eq!(runtime.stats().resources_created, 3);
        assert_eq!(runtime.stats().resources_reused, 5);
        assert_eq!(runtime.entries.len(), 2);

        // A saturated generation cannot be reused even for the same descriptor.
        runtime.entries[c.index as usize].generation = u32::MAX;
        prepare_images(runtime, &[4]);
        let replacement = complete_image(runtime, 4, 66);
        assert_eq!(replacement.index, third_a.index);
        assert!(replacement.generation > third_a.generation);
        runtime.entries[replacement.index as usize].generation = u32::MAX;
        let (plan, _, _) = image_plan(4, 77);
        assert_eq!(
            runtime.submit(&plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(runtime.stats().resources_created, 4);
    });
}

#[test]
#[ignore = "explicit real-GPU registered and pending cache protection; requires Core/SyncVal"]
fn vulkan_graph_cache_never_evicts_registered_or_unobserved_pending_resources() {
    real_gpu(2, 3, ValidationMode::Required, |runtime| {
        prepare_images(runtime, &[2, 3]);
        let (first_plan, first_image, first_read) = image_plan(2, 17);
        let mut first = runtime.submit(&first_plan).unwrap();
        let first_key = first.export_image(first_image).unwrap().key();
        let (second_plan, second_image, second_read) = image_plan(3, 29);
        let mut second = runtime.submit(&second_plan).unwrap();
        let second_key = second.export_image(second_image).unwrap().key();
        runtime.delete(second_key).unwrap();
        let (third_plan, _, _) = image_plan(4, 41);
        assert_eq!(
            runtime.submit(&third_plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted,
            "ordinary submit cannot create an unprepared descriptor class"
        );
        assert_eq!(
            runtime
                .acquire_inner(
                    ResourceDesc::Image(ImageDesc::rgba8(4, 4)),
                    "blocked replacement",
                    true,
                )
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::CapacityExhausted,
            "even explicit replacement cannot evict registered or unobserved pending owners"
        );
        assert_eq!(runtime.stats().resources_created, 2);
        check_result(&mut second, second_read, 3, 29);
        replace_image_asset(runtime, 4);
        let third_key = complete_image(runtime, 4, 41);
        assert_eq!(third_key.index, second_key.index);
        assert!(third_key.generation > second_key.generation);
        check_result(&mut first, first_read, 2, 17);
        replace_image_asset(runtime, 2);
        let same_descriptor = complete_image(runtime, 2, 53);
        assert_ne!(same_descriptor.index, first_key.index);
        assert!(runtime.refresh_image(first_key).is_ok());
        runtime.delete(first_key).unwrap();
    });
}

#[test]
#[ignore = "explicit real-GPU failed cache replacement retains its victim; requires Core/SyncVal"]
fn vulkan_graph_cache_failed_replacement_preserves_existing_allocation() {
    real_gpu(1, 1, ValidationMode::Required, |runtime| {
        prepare_images(runtime, &[2]);
        let original = complete_image(runtime, 2, 7);
        let entry = &runtime.entries[original.index as usize];
        let native_image = entry.physical().image().image;
        let accepted_serial = entry.physical().lock().scheduled.serial;
        // Exercise the native resource boundary directly: Graph names are user
        // input, and a failed constructor must not destroy the selected victim.
        let error = runtime
            .acquire_inner(
                ResourceDesc::Image(ImageDesc::rgba8(3, 3)),
                "invalid\0name",
                true,
            )
            .err()
            .expect("invalid naming input must reject replacement");
        assert_eq!(error.kind(), RuntimeErrorKind::InvalidInput);
        let entry = &runtime.entries[original.index as usize];
        assert_eq!(entry.generation, original.generation);
        assert_eq!(entry.physical().image().image, native_image);
        assert_eq!(entry.physical().lock().scheduled.serial, accepted_serial);
        assert_eq!(runtime.stats().resources_created, 1);
        assert_eq!(runtime.entries.len(), 1);
        let reused = complete_image(runtime, 2, 19);
        assert_eq!(reused.index, original.index);
        assert_eq!(reused.generation, original.generation + 1);
        assert_eq!(runtime.stats().resources_created, 1);
        assert_eq!(runtime.stats().resources_reused, 2);
    });
}

#[test]
#[ignore = "explicit real-GPU idle trim ownership and native memory test; requires Core/SyncVal"]
fn vulkan_graph_trim_releases_idle_working_set_and_preserves_live_owners() {
    real_gpu(8, 3, ValidationMode::Required, |runtime| {
        let baseline = runtime.memory_stats().unwrap();
        prepare_images(runtime, &[4, 8, 3]);
        let first = complete_image(runtime, 4, 11);
        let second = complete_image(runtime, 8, 22);
        let registered = runtime
            .create_buffer("registered", BufferDesc { size: 64 })
            .unwrap();
        let leased = runtime
            .create_buffer("escaped lease", BufferDesc { size: 96 })
            .unwrap();
        let lease = runtime.lease(leased.key()).unwrap();
        runtime.delete(leased.key()).unwrap();

        let (plan, image, read) = image_plan(3, 33);
        let mut pending = runtime.submit(&plan).unwrap();
        let pending_key = pending.export_image(image).unwrap().key();
        runtime.delete(pending_key).unwrap();
        let before = runtime.memory_stats().unwrap();
        assert_eq!(before.cached_logical_bytes, 4 * 4 * 4 + 8 * 8 * 4);
        assert_eq!(before.registered_logical_bytes, 64);
        assert_eq!(before.retained_logical_bytes, 96 + 3 * 3 * 4);
        assert_eq!(
            before.vma_allocation_count,
            baseline.vma_allocation_count + 5
        );

        assert_eq!(runtime.trim_idle_resources(), 2);
        let trimmed = runtime.memory_stats().unwrap();
        assert_eq!(trimmed.cached_logical_bytes, 0);
        assert_eq!(trimmed.registered_logical_bytes, 64);
        assert_eq!(
            trimmed.retained_logical_bytes,
            before.retained_logical_bytes
        );
        assert_eq!(
            trimmed.vma_allocation_count,
            before.vma_allocation_count - 2
        );
        assert!(trimmed.vma_allocation_bytes < before.vma_allocation_bytes);
        assert!(pending.is_pending(), "trim must not poll or wait for work");
        assert!(runtime.refresh_buffer(registered.key()).is_ok());
        assert_eq!(runtime.trim_idle_resources(), 0);
        for key in [first, second, pending_key, leased.key()] {
            assert_eq!(
                runtime.refresh_image(key).err().unwrap().kind(),
                RuntimeErrorKind::StaleResource
            );
        }

        // A rejected constructor must leave the trimmed slot and its old
        // generation untouched. Its next successful creation gets a new key.
        assert_eq!(
            runtime
                .acquire_inner(
                    ResourceDesc::Buffer(BufferDesc { size: 0 }),
                    "invalid size",
                    true,
                )
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::InvalidInput
        );
        let replacement = runtime
            .create_buffer("after trim", BufferDesc { size: 80 })
            .unwrap();
        assert_eq!(replacement.key().index, first.index);
        assert_eq!(replacement.key().generation, first.generation + 1);
        assert_eq!(
            runtime.entries.len(),
            5,
            "fill holes before registry growth"
        );
        assert_eq!(
            runtime.refresh_image(first).unwrap_err().kind(),
            RuntimeErrorKind::StaleResource
        );

        check_result(&mut pending, read, 3, 33);
        assert_eq!(runtime.trim_idle_resources(), 1);
        assert_eq!(runtime.memory_stats().unwrap().retained_logical_bytes, 96);
        drop(lease);
        assert_eq!(runtime.trim_idle_resources(), 1);
        runtime.delete(registered.key()).unwrap();
        runtime.delete(replacement.key()).unwrap();
        assert_eq!(runtime.trim_idle_resources(), 2);
        let final_memory = runtime.memory_stats().unwrap();
        assert_eq!(
            final_memory.vma_allocation_count,
            baseline.vma_allocation_count
        );
        assert_eq!(
            final_memory.vma_allocation_bytes,
            baseline.vma_allocation_bytes
        );
        assert_eq!(final_memory.cached_logical_bytes, 0);
        assert_eq!(final_memory.retained_logical_bytes, 0);
        eprintln!(
            "idle trim memory: baseline={baseline:?}; before={before:?}; trimmed={trimmed:?}; final={final_memory:?}"
        );
    });
}

#[test]
#[ignore = "explicit real-GPU saturated generation trim test; requires Core/SyncVal"]
fn vulkan_graph_trim_releases_saturated_generations_without_reviving_keys() {
    real_gpu(1, 1, ValidationMode::Required, |runtime| {
        let baseline = runtime.memory_stats().unwrap();
        prepare_images(runtime, &[2]);
        let key = complete_image(runtime, 2, 17);
        runtime.entries[key.index as usize].generation = u32::MAX;
        assert_eq!(runtime.trim_idle_resources(), 1);
        assert_eq!(runtime.trim_idle_resources(), 0);
        assert_eq!(
            runtime.memory_stats().unwrap().vma_allocation_count,
            baseline.vma_allocation_count
        );
        let (plan, _, _) = image_plan(2, 29);
        assert_eq!(
            runtime.submit(&plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted,
            "an exhausted slot remains a tombstone after releasing its allocation"
        );
        assert_eq!(
            runtime.refresh_image(key).unwrap_err().kind(),
            RuntimeErrorKind::StaleResource
        );
    });
}
