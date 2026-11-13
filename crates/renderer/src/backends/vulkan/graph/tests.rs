use super::*;
use crate::graph::{BufferDesc, Graph, ImageDesc};

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
        let mut reader = runtime.submit(&unchanged.compile().unwrap()).unwrap();
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
            #[cfg(feature = "graph-diagnostics")]
            let (mut submission, lowering) = runtime.submit_with_diagnostics(&plan).unwrap();
            #[cfg(not(feature = "graph-diagnostics"))]
            let mut submission = runtime.submit(&plan).unwrap();
            let key = submission.export_buffer(source).unwrap().key();
            #[cfg(feature = "graph-diagnostics")]
            {
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
            }
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
