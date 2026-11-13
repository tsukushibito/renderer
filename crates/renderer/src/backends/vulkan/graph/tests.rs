use super::*;
use crate::graph::{Attachment, LoadOp, RasterAttachments, RasterDraw, StoreOp};
use crate::graph::{BufferDesc, Graph, ImageDesc};

#[test]
fn prepared_upload_uses_slice_length_without_retaining_source_bytes() {
    let input: Arc<[u8]> = vec![7; 64].into();
    let weak = Arc::downgrade(&input);
    let mut graph = Graph::new();
    let buffer = graph.buffer("slice", BufferDesc { size: 4 }).unwrap();
    let upload = graph
        .upload_buffer_slice("upload", buffer, 0, input, 5..9)
        .unwrap();
    graph.readback_buffer("read", buffer, 0, 4).unwrap();
    let plan = graph.compile().unwrap();
    let operation = PreparedOperation::try_from(&plan.passes[upload.index].operation).unwrap();
    assert!(matches!(
        &operation,
        PreparedOperation::UploadBuffer {
            offset: 0,
            size: 4,
            ..
        }
    ));
    drop(graph);
    drop(plan);
    assert!(
        weak.upgrade().is_none(),
        "native recording operands must not retain CPU upload input"
    );
}

#[test]
fn provenance_follows_index_copy_sources_and_persistent_imports_only() {
    let mut graph = Graph::new();
    let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
    graph
        .upload_buffer("vertices.upload", vertices, 0, vec![0; 84])
        .unwrap();
    let source = graph.buffer("source", BufferDesc { size: 12 }).unwrap();
    let middle = graph.buffer("middle", BufferDesc { size: 12 }).unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    graph
        .upload_buffer("index.upload", source, 0, vec![0; 12])
        .unwrap();
    graph
        .copy_buffer("index.copy1", source, 0, middle, 0, 12)
        .unwrap();
    graph
        .copy_buffer("index.copy2", middle, 0, indices, 0, 12)
        .unwrap();
    let color = graph.image("color", ImageDesc::rgba8(1, 1)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(1, 1)).unwrap();
    graph
        .raster(
            "mesh",
            RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: color,
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                },
                depth: Attachment {
                    image: depth,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                },
            },
        )
        .unwrap();
    graph.readback_image("color.read", color).unwrap();
    let imported = graph
        .import_buffer(
            "persistent",
            &ImportBuffer {
                key: ResourceKey {
                    owner: 1,
                    index: 0,
                    generation: 1,
                },
                revision: 1,
                desc: BufferDesc { size: 12 },
                initialized: std::iter::once(0..12).collect(),
            },
        )
        .unwrap();
    graph
        .readback_buffer("persistent.read", imported, 0, 12)
        .unwrap();
    let (needed, retained) = provenance_requirements(&graph.compile().unwrap());
    assert!(
        !needed[vertices.index],
        "vertex-only input needs no CPU copy"
    );
    for buffer in [source, middle, indices] {
        assert!(
            needed[buffer.index],
            "all index copy sources must remain known"
        );
        assert!(
            !retained[buffer.index],
            "transient data need not survive prepare"
        );
    }
    assert!(
        needed[imported.index] && retained[imported.index],
        "registration persists without another explicit export"
    );
}

#[test]
fn writer_visibility_survives_different_read_stages_and_import_snapshots() {
    let previous = Snapshot {
        stage: vk::PipelineStageFlags2::COPY | vk::PipelineStageFlags2::COMPUTE_SHADER,
        access: vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::SHADER_STORAGE_READ,
        writer_stage: vk::PipelineStageFlags2::COPY,
        writer_access: vk::AccessFlags2::TRANSFER_WRITE,
        ..Snapshot::default()
    };
    let snapshot = previous.clone();
    for purpose in [
        AccessPurpose::Vertex,
        AccessPurpose::Index,
        AccessPurpose::Sampled,
    ] {
        let (stage, access) = source_scope(&snapshot, usage(purpose), false);
        assert!(stage.contains(vk::PipelineStageFlags2::COPY));
        assert!(access.contains(vk::AccessFlags2::TRANSFER_WRITE));
    }
    let mut writer = usage(AccessPurpose::CopyDestination);
    writer.writes = true;
    let (stage, access) = source_scope(&previous, writer, false);
    assert!(
        stage.contains(vk::PipelineStageFlags2::COPY | vk::PipelineStageFlags2::COMPUTE_SHADER)
    );
    assert!(
        access.contains(vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::TRANSFER_WRITE)
    );
}

#[test]
fn initial_image_transition_orders_prior_copies_without_source_memory_access() {
    let initial = Snapshot::default();
    for purpose in [
        AccessPurpose::CopyDestination,
        AccessPurpose::StorageWrite,
        AccessPurpose::ColorAttachment,
        AccessPurpose::DepthAttachment,
    ] {
        assert_eq!(
            source_scope(&initial, usage(purpose), true),
            (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::NONE)
        );
    }
    assert_eq!(
        source_scope(&initial, usage(AccessPurpose::CopyDestination), false),
        (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE),
        "new buffer uploads do not need the image scheduling workaround"
    );
    let previous = Snapshot {
        layout: vk::ImageLayout::GENERAL,
        stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
        access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
        writer_stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
        writer_access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
        ..Snapshot::default()
    };
    assert_eq!(
        source_scope(&previous, usage(AccessPurpose::Sampled), true),
        (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE
        ),
        "later transitions preserve their actual writer without extra COPY scope"
    );
}

#[test]
#[ignore = "explicit real GPU upload ownership and future index import test; requires Core/SyncVal"]
fn vulkan_graph_upload_input_retirement_and_future_index_import() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 3,
            resource_capacity: 8,
            staging_bytes: 4096,
            max_shader_bindings: 4,
        })
        .unwrap();
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );

        // Recording owns staging, not the original transient input. This holds
        // while GPU work is still pending, and also for unread completed output.
        let (plan, buffer_read, image_read, buffer_input, image_input) = {
            let buffer_bytes: Arc<[u8]> = Arc::from([17u8; 16]);
            let image_bytes: Arc<[u8]> = Arc::from([29u8; 16]);
            let buffer_input = Arc::downgrade(&buffer_bytes);
            let image_input = Arc::downgrade(&image_bytes);
            let mut graph = Graph::new();
            let buffer = graph.buffer("transient", BufferDesc { size: 16 }).unwrap();
            let image = graph
                .image("transient-image", ImageDesc::rgba8(2, 2))
                .unwrap();
            graph
                .upload_buffer_shared("buffer.upload", buffer, 0, buffer_bytes)
                .unwrap();
            graph
                .upload_image_shared("image.upload", image, image_bytes)
                .unwrap();
            let buffer_read = graph.readback_buffer("buffer.read", buffer, 0, 16).unwrap();
            let image_read = graph.readback_image("image.read", image).unwrap();
            (
                graph.compile().unwrap(),
                buffer_read,
                image_read,
                buffer_input,
                image_input,
            )
        };
        let mut pending = runtime.submit(&plan).unwrap();
        drop(plan);
        assert!(buffer_input.upgrade().is_none());
        assert!(image_input.upgrade().is_none());
        let mut result = pending.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert!(
            result.work.passes.is_empty(),
            "completed output retains no recording plan"
        );
        assert_eq!(result.readback(buffer_read).unwrap(), [17u8; 16]);
        assert_eq!(result.readback(image_read).unwrap(), [29u8; 16]);
        drop((pending, result));

        // The initial-transition ordering also covers earlier copies in other
        // submissions on the same queue. Only C is waited before reading A/B.
        let mut producer = Graph::new();
        let buffer = producer
            .buffer("pending-buffer", BufferDesc { size: 16 })
            .unwrap();
        producer
            .upload_buffer("pending.upload", buffer, 0, vec![17; 16])
            .unwrap();
        producer.export_buffer(buffer).unwrap();
        let mut first = runtime.submit(&producer.compile().unwrap()).unwrap();
        let buffer = first.export_buffer(buffer).unwrap();
        let mut images = Graph::new();
        // A distinct extent requires a fresh native image with UNDEFINED layout.
        let image = images.image("new-image", ImageDesc::rgba8(3, 1)).unwrap();
        images
            .upload_image("new-image.upload", image, vec![29; 12])
            .unwrap();
        images.export_image(image).unwrap();
        let mut second = runtime.submit(&images.compile().unwrap()).unwrap();
        let image = second.export_image(image).unwrap();
        let mut consumer = Graph::new();
        let input_buffer = consumer.import_buffer("pending-buffer", &buffer).unwrap();
        let input_image = consumer.import_image("pending-image", &image).unwrap();
        let buffer_read = consumer
            .readback_buffer("pending-buffer.read", input_buffer, 0, 16)
            .unwrap();
        let image_read = consumer
            .readback_image("pending-image.read", input_image)
            .unwrap();
        let mut third = runtime.submit(&consumer.compile().unwrap()).unwrap();
        let mut result = third.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(result.readback(buffer_read).unwrap(), [17; 16]);
        assert_eq!(result.readback(image_read).unwrap(), [29; 12]);
        second.wait(Duration::from_secs(10)).unwrap().unwrap();
        first.wait(Duration::from_secs(10)).unwrap().unwrap();
        runtime.delete(buffer.key()).unwrap();
        runtime.delete(image.key()).unwrap();
        drop((first, second, third, result));

        // A generic exported buffer can first acquire an index role in a later
        // graph. Updating that import without re-export must preserve provenance.
        let (initial, indices) = {
            let mut graph = Graph::new();
            let indices = graph
                .buffer("future-indices", BufferDesc { size: 12 })
                .unwrap();
            graph
                .upload_buffer("indices.upload", indices, 0, vec![0; 12])
                .unwrap();
            graph.export_buffer(indices).unwrap();
            (graph.compile().unwrap(), indices)
        };
        let mut first = runtime.submit(&initial).unwrap();
        let exported = first.export_buffer(indices).unwrap();
        drop(initial);
        let mut update = Graph::new();
        let indices = update
            .import_buffer("persistent-indices", &exported)
            .unwrap();
        let patch = update.buffer("patch", BufferDesc { size: 4 }).unwrap();
        update
            .upload_buffer("patch.upload", patch, 0, 1u32.to_le_bytes().to_vec())
            .unwrap();
        update
            .copy_buffer("patch.copy", patch, 0, indices, 4, 4)
            .unwrap();
        let mut second = runtime.submit(&update.compile().unwrap()).unwrap();
        second.wait(Duration::from_secs(10)).unwrap().unwrap();
        first.wait(Duration::from_secs(10)).unwrap().unwrap();
        let current = runtime.refresh_buffer(exported.key()).unwrap();
        let mut draw = Graph::new();
        let indices = draw.import_buffer("indices", &current).unwrap();
        let vertices = draw.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        draw.upload_buffer("vertices.upload", vertices, 0, vec![0; 84])
            .unwrap();
        let color = draw.image("color", ImageDesc::rgba8(1, 1)).unwrap();
        let depth = draw.image("depth", ImageDesc::depth32(1, 1)).unwrap();
        draw.raster(
            "degenerate-mesh",
            RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: color,
                    load: LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
                    store: StoreOp::Store,
                },
                depth: Attachment {
                    image: depth,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                },
            },
        )
        .unwrap();
        let read = draw.readback_image("color.read", color).unwrap();
        let mut third = runtime.submit(&draw.compile().unwrap()).unwrap();
        let mut result = third.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(result.readback(read).unwrap(), [255, 0, 0, 255]);
        runtime.delete(exported.key()).unwrap();
        drop((first, second, third, result, runtime));
        assert!(
            diagnostics.snapshot().is_clean(),
            "{:#?}",
            diagnostics.snapshot()
        );
    });
}

#[test]
#[ignore = "explicit real GPU graph Copy/import/reuse test; requires strict Vulkan validation"]
fn vulkan_graph_copy_async_identity_and_bounded_reuse() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 6,
            staging_bytes: 4096,
            max_shader_bindings: 8,
        })
        .unwrap();
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let buffer_bytes = (0u8..16)
            .map(|byte| byte.wrapping_mul(17))
            .collect::<Vec<_>>();
        let image_bytes = (0..17 * 13 * 4)
            .map(|byte| (byte * 37 + 11) as u8)
            .collect::<Vec<_>>();
        let mut producer = Graph::new();
        let buffer = producer
            .buffer("shared.buffer", BufferDesc { size: 16 })
            .unwrap();
        let image = producer
            .image("shared.image", ImageDesc::rgba8(17, 13))
            .unwrap();
        producer
            .upload_buffer("upload.buffer", buffer, 0, buffer_bytes.clone())
            .unwrap();
        producer
            .upload_image("upload.image", image, image_bytes.clone())
            .unwrap();
        let dead = producer.buffer("dead", BufferDesc { size: 4 }).unwrap();
        producer
            .upload_buffer("unused.upload", dead, 0, vec![0; 4])
            .unwrap();
        producer.export_buffer(buffer).unwrap();
        producer.export_image(image).unwrap();
        let plan = producer.compile().unwrap();
        assert!(!plan.ordered_pass_names().contains(&"unused.upload"));
        #[cfg(feature = "graph-diagnostics")]
        assert_eq!(plan.culled_pass_names(), ["unused.upload"]);
        let mut first = runtime.submit(&plan).unwrap();
        let buffer_export = first.export_buffer(buffer).unwrap();
        let image_export = first.export_image(image).unwrap();
        assert_eq!(
            runtime.stats().resources_created,
            2,
            "dead resource must not allocate"
        );
        let mut consumer = Graph::new();
        let input = consumer
            .import_buffer("canonical.buffer", &buffer_export)
            .unwrap();
        let same_input = consumer.import_buffer("alias", &buffer_export).unwrap();
        assert_eq!(input, same_input);
        let input_image = consumer
            .import_image("canonical.image", &image_export)
            .unwrap();
        let output = consumer
            .buffer("copy.buffer", BufferDesc { size: 16 })
            .unwrap();
        let output_image = consumer
            .image("copy.image", ImageDesc::rgba8(17, 13))
            .unwrap();
        consumer
            .copy_buffer("copy.buffer", input, 0, output, 0, 16)
            .unwrap();
        consumer
            .copy_image("copy.image", input_image, output_image)
            .unwrap();
        let buffer_read = consumer
            .readback_buffer("read.buffer", output, 0, 16)
            .unwrap();
        let image_read = consumer.readback_image("read.image", output_image).unwrap();
        let mut second = runtime.submit(&consumer.compile().unwrap()).unwrap();
        assert_eq!(
            runtime.submit(&plan).err().unwrap().kind(),
            RuntimeErrorKind::CapacityExhausted,
            "bounded execution capacity cannot hide a wait"
        );
        runtime.delete(buffer_export.key()).unwrap();
        assert!(runtime.refresh_buffer(buffer_export.key()).is_err());
        let mut result = second.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(result.readback(buffer_read).unwrap(), buffer_bytes);
        assert_eq!(result.readback(image_read).unwrap(), image_bytes);
        assert!(result.readback(image_read).is_err());
        let accepted = runtime.entries[image_export.key.index as usize]
            .physical()
            .lock()
            .scheduled
            .serial;
        assert_eq!(accepted, 2);
        let completed = runtime.entries[image_export.key.index as usize]
            .physical()
            .lock()
            .completed_serial;
        assert_eq!(completed, 2);
        first.wait(Duration::from_secs(10)).unwrap().unwrap();
        let state = runtime.entries[image_export.key.index as usize]
            .physical()
            .lock();
        assert_eq!(
            state.completed_serial, 2,
            "waiting A after B cannot regress completion"
        );
        assert_eq!(
            state.scheduled.serial, 2,
            "waiting A cannot regress B's accepted state"
        );
        drop(state);
        // B only read the image. Its changed native layout/serial must not
        // invalidate A's unchanged content snapshot.
        let mut unchanged = Graph::new();
        let old = unchanged
            .import_image("unchanged.snapshot", &image_export)
            .unwrap();
        let old_read = unchanged.readback_image("unchanged.read", old).unwrap();
        let (mut reader, _) = runtime
            .submit_with_diagnostics(&unchanged.compile().unwrap())
            .unwrap();
        let mut read_result = reader.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(read_result.readback(old_read).unwrap(), image_bytes);
        assert_eq!(
            runtime.refresh_image(image_export.key()).unwrap(),
            image_export
        );
        runtime.delete(image_export.key()).unwrap();
        drop((first, second, result, reader, read_result));
        let mut repeated = Graph::new();
        let source = repeated
            .buffer("repeat.src", BufferDesc { size: 16 })
            .unwrap();
        let target = repeated
            .buffer("repeat.dst", BufferDesc { size: 16 })
            .unwrap();
        repeated
            .upload_buffer("repeat.upload", source, 0, buffer_bytes.clone())
            .unwrap();
        repeated
            .copy_buffer("repeat.copy", source, 0, target, 0, 16)
            .unwrap();
        repeated.export_buffer(source).unwrap();
        let read = repeated
            .readback_buffer("repeat.read", target, 0, 16)
            .unwrap();
        let plan = repeated.compile().unwrap();
        let mut previous_key: Option<ResourceKey> = None;
        for _ in 0..8 {
            let (mut submission, lowering) = runtime.submit_with_diagnostics(&plan).unwrap();
            let key = submission.export_buffer(source).unwrap().key();
            let physical = runtime.entry(key).unwrap().physical();
            let label = format!("resource {} \"repeat.src\"", source.index);
            let mut lines = lowering
                .lines()
                .skip_while(|line| !line.starts_with(&label));
            let resource_line = lines.next().expect("logical resource is diagnosed");
            assert!(resource_line.contains(&format!(
                "physical_key owner={} index={} generation={}",
                key.owner, key.index, key.generation
            )));
            assert!(
                lines
                    .next()
                    .unwrap()
                    .contains(&format!("native_buffer={:?}", physical.buffer().handle))
            );
            if let Some(previous) = previous_key {
                assert_eq!(previous.index, key.index, "same physical entry is reused");
                assert!(key.generation > previous.generation);
            }
            previous_key = Some(key);
            runtime.delete(key).unwrap();
            let mut result = submission.wait(Duration::from_secs(10)).unwrap().unwrap();
            assert_eq!(runtime.stats().upload_live_ranges, 0);
            assert_eq!(runtime.stats().readback_live_ranges, 1);
            assert_eq!(result.readback(read).unwrap(), buffer_bytes);
        }
        let stats = runtime.stats();
        assert!(stats.resources_created <= 6, "{stats:?}");
        assert!(stats.resources_reused >= 14, "{stats:?}");
        assert_eq!(stats.execution_slots_created, 2);
        assert!(stats.execution_slots_reused >= 8);
        assert_eq!(
            (stats.upload_live_ranges, stats.readback_live_ranges),
            (0, 0)
        );
        drop(runtime);
        assert!(
            diagnostics.snapshot().is_clean(),
            "shutdown diagnostics: {:#?}",
            diagnostics.snapshot()
        );
    });
}

#[test]
#[ignore = "explicit real GPU obsolete input retirement; requires Core/SyncVal"]
fn vulkan_graph_full_replacement_releases_old_completed_input() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 2,
            resource_capacity: 2,
            staging_bytes: 4 * 1024 * 1024,
            max_shader_bindings: 4,
        })
        .unwrap();
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );

        let size = 1024 * 1024;
        let (key, old_input) = {
            let input: Arc<[u8]> = vec![17; size].into();
            let weak = Arc::downgrade(&input);
            let mut graph = Graph::new();
            let buffer = graph
                .buffer("persistent", BufferDesc { size: size as u64 })
                .unwrap();
            graph
                .upload_buffer_shared("A.upload", buffer, 0, input)
                .unwrap();
            graph.export_buffer(buffer).unwrap();
            let plan = graph.compile().unwrap();
            let mut pending = runtime.submit(&plan).unwrap();
            let key = pending.export_buffer(buffer).unwrap().key();
            drop(pending.wait(Duration::from_secs(10)).unwrap().unwrap());
            (key, weak)
        };
        assert!(
            old_input.upgrade().is_some(),
            "current contents support future index use"
        );

        let current_input = {
            let input: Arc<[u8]> = vec![29; size].into();
            let weak = Arc::downgrade(&input);
            let mut graph = Graph::new();
            let buffer = graph
                .import_buffer("persistent", &runtime.refresh_buffer(key).unwrap())
                .unwrap();
            graph
                .upload_buffer_shared("B.upload", buffer, 0, input)
                .unwrap();
            let plan = graph.compile().unwrap();
            let pending = runtime.submit(&plan).unwrap();
            drop((graph, plan));
            // Normal Drop retirement must not require a later explicit wait to
            // release the superseded version of the CPU-known contents.
            drop(pending);
            weak
        };
        assert!(
            old_input.upgrade().is_none(),
            "obsolete completed contents must not pin A"
        );
        assert!(current_input.upgrade().is_some());
        {
            let mut graph = Graph::new();
            let buffer = graph
                .import_buffer("current", &runtime.refresh_buffer(key).unwrap())
                .unwrap();
            let read = graph
                .readback_buffer("C.read", buffer, 0, size as u64)
                .unwrap();
            let mut pending = runtime.submit(&graph.compile().unwrap()).unwrap();
            let mut result = pending.wait(Duration::from_secs(10)).unwrap().unwrap();
            assert_eq!(result.readback(read).unwrap(), vec![29; size]);
        }
        runtime.delete(key).unwrap();
        drop(runtime);
        assert!(current_input.upgrade().is_none());
        assert!(
            diagnostics.snapshot().is_clean(),
            "{:#?}",
            diagnostics.snapshot()
        );
    });
}

#[test]
#[ignore = "explicit real GPU graph capacity/failure/owner test; requires strict Vulkan validation"]
fn vulkan_graph_failed_acquisition_recovers_and_rejects_foreign_or_stale_imports() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let config = RuntimeConfig {
            execution_slots: 1,
            resource_capacity: 1,
            staging_bytes: 256,
            max_shader_bindings: 2,
        };
        let mut runtime = VulkanRuntime::new(config).unwrap();
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let mut over_capacity = Graph::new();
        let source = over_capacity
            .buffer("source", BufferDesc { size: 16 })
            .unwrap();
        let target = over_capacity
            .buffer("target", BufferDesc { size: 16 })
            .unwrap();
        over_capacity
            .upload_buffer("upload", source, 0, vec![1; 16])
            .unwrap();
        over_capacity
            .copy_buffer("copy", source, 0, target, 0, 16)
            .unwrap();
        over_capacity
            .readback_buffer("read", target, 0, 16)
            .unwrap();
        assert_eq!(
            runtime
                .submit(&over_capacity.compile().unwrap())
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(runtime.stats().registered_resources, 0);
        let mut over_staging = Graph::new();
        let large = over_staging
            .buffer("large", BufferDesc { size: 512 })
            .unwrap();
        over_staging
            .upload_buffer("large.upload", large, 0, vec![1; 512])
            .unwrap();
        over_staging.export_buffer(large).unwrap();
        assert_eq!(
            runtime
                .submit(&over_staging.compile().unwrap())
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(runtime.stats().registered_resources, 0);
        assert_eq!(runtime.stats().upload_live_ranges, 0);
        let mut valid = Graph::new();
        let value = valid.buffer("value", BufferDesc { size: 16 }).unwrap();
        valid
            .upload_buffer("upload", value, 0, vec![7; 16])
            .unwrap();
        let read = valid.readback_buffer("read", value, 0, 16).unwrap();
        valid.export_buffer(value).unwrap();
        let plan = valid.compile().unwrap();
        let mut submission = runtime.submit(&plan).unwrap();
        let first_export = submission.export_buffer(value).unwrap();
        let mut result = submission.wait(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(result.readback(read).unwrap(), [7; 16]);
        assert_eq!(
            runtime.entries.len(),
            1,
            "failed acquisitions do not grow the registry"
        );
        let mut foreign_runtime = VulkanRuntime::new(config).unwrap();
        let foreign_diagnostics = foreign_runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&foreign_diagnostics);
        let mut imported = Graph::new();
        let input = imported.import_buffer("input", &first_export).unwrap();
        imported
            .readback_buffer("input.read", input, 0, 16)
            .unwrap();
        let imported_plan = imported.compile().unwrap();
        assert_eq!(
            foreign_runtime.submit(&imported_plan).err().unwrap().kind(),
            RuntimeErrorKind::WrongOwner
        );
        // Forging contents inside the crate cannot authorize a different runtime
        // initialization snapshot. Public callers cannot construct these fields.
        let mut forged = first_export.clone();
        forged.initialized = std::iter::once(0..8).collect();
        let mut malformed = Graph::new();
        let input = malformed.import_buffer("forged", &forged).unwrap();
        malformed.readback_buffer("read", input, 0, 8).unwrap();
        assert_eq!(
            runtime
                .submit(&malformed.compile().unwrap())
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::StaleResource
        );
        runtime.delete(first_export.key()).unwrap();
        assert_eq!(
            runtime.submit(&imported_plan).err().unwrap().kind(),
            RuntimeErrorKind::StaleResource
        );
        let mut recycled = runtime.submit(&plan).unwrap();
        let second_export = recycled.export_buffer(value).unwrap();
        assert_ne!(first_export.key(), second_export.key());
        assert!(runtime.refresh_buffer(first_export.key()).is_err());
        recycled.wait(Duration::from_secs(10)).unwrap().unwrap();
        runtime.delete(second_export.key()).unwrap();
        drop((result, submission, recycled));
        drop((runtime, foreign_runtime));
        assert!(
            diagnostics.snapshot().is_clean(),
            "{:#?}",
            diagnostics.snapshot()
        );
        assert!(
            foreign_diagnostics.snapshot().is_clean(),
            "{:#?}",
            foreign_diagnostics.snapshot()
        );
    });
}
