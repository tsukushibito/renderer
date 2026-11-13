//! Content versions remain stable across read-only queue-state changes.

use super::*;
use crate::graph::{Attachment, LoadOp, RasterAttachments, RasterDraw, StoreOp};
use crate::graph::{BufferDesc, Graph, GraphErrorKind, ImageDesc};

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn readonly_scopes_preserve_content_revision_and_order_later_writes() {
    let mut state = Snapshot {
        revision: 7,
        stage: vk::PipelineStageFlags2::COPY,
        access: vk::AccessFlags2::TRANSFER_WRITE,
        writer_stage: vk::PipelineStageFlags2::COPY,
        writer_access: vk::AccessFlags2::TRANSFER_WRITE,
        ..Snapshot::default()
    };
    state.record_access(usage(AccessPurpose::CopySource), false, 8);
    state.record_access(usage(AccessPurpose::StorageRead), false, 9);
    state.record_access(usage(AccessPurpose::Sampled), true, 10);
    assert_eq!(state.revision, 7, "reads/layout changes preserve contents");
    let mut write = usage(AccessPurpose::CopyDestination);
    write.writes = true;
    let (stages, access) = source_scope(&state, write, true);
    assert!(stages.contains(vk::PipelineStageFlags2::FRAGMENT_SHADER));
    assert!(stages.contains(vk::PipelineStageFlags2::COPY));
    assert!(access.contains(vk::AccessFlags2::SHADER_SAMPLED_READ));
    assert!(access.contains(vk::AccessFlags2::TRANSFER_WRITE));
    state.record_access(write, true, 11);
    assert_eq!(state.revision, 11);
}

fn real_gpu(test: impl FnOnce(&mut VulkanRuntime)) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 4,
            resource_capacity: 16,
            staging_bytes: 4096,
            max_shader_bindings: 8,
        })
        .expect("real Vulkan runtime is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let diagnostics = Arc::clone(&runtime.diagnostics);
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        test(&mut runtime);
        drop(runtime);
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "Core/SyncVal through shutdown: {report:#?}"
        );
    });
}

fn completed(submission: &mut Submission) -> SubmissionResult {
    submission
        .wait(WAIT)
        .unwrap()
        .expect("accepted GPU work must complete")
}

#[test]
#[ignore = "explicit real-GPU readonly import rebasing and writer staleness; requires Core/SyncVal"]
fn vulkan_graph_precompiled_readers_and_writer_rebase_unchanged_contents() {
    real_gpu(|runtime| {
        let pixels = [10, 20, 30, 255].repeat(4);
        let bytes = (0..16).collect::<Vec<u8>>();
        let mut initial = Graph::new();
        let image = initial
            .image("shared image", ImageDesc::rgba8(2, 2))
            .unwrap();
        let buffer = initial
            .buffer("shared bytes", BufferDesc { size: 16 })
            .unwrap();
        initial
            .upload_image("initial image", image, pixels.clone())
            .unwrap();
        initial
            .upload_buffer("initial bytes", buffer, 0, bytes.clone())
            .unwrap();
        initial.export_image(image).unwrap();
        initial.export_buffer(buffer).unwrap();
        let mut first = runtime.submit(&initial.compile().unwrap()).unwrap();
        let source_image = first.export_image(image).unwrap();
        let source_buffer = first.export_buffer(buffer).unwrap();
        completed(&mut first);

        // Compile both readers and a writer before any of them changes queue
        // scopes/layout. Their logical input contents are the same version.
        let mut compute = Graph::new();
        let compute_image = compute
            .import_image("compute input", &source_image)
            .unwrap();
        let compute_buffer = compute
            .import_buffer("compute bytes", &source_buffer)
            .unwrap();
        let inverted = compute.image("inverted", ImageDesc::rgba8(2, 2)).unwrap();
        compute
            .compute("invert", compute_image, inverted, ComputeTransform::Invert)
            .unwrap();
        let compute_pixels = compute.readback_image("inverted result", inverted).unwrap();
        let compute_bytes = compute
            .readback_buffer("first bytes", compute_buffer, 0, 16)
            .unwrap();
        let compute = compute.compile().unwrap();

        let mut sampled = Graph::new();
        let sampled_image = sampled.import_image("sample input", &source_image).unwrap();
        let sampled_buffer = sampled
            .import_buffer("sample bytes", &source_buffer)
            .unwrap();
        let output = sampled
            .image("sample output", ImageDesc::rgba8(2, 2))
            .unwrap();
        sampled.composite("sample", sampled_image, output).unwrap();
        let sampled_pixels = sampled.readback_image("sample result", output).unwrap();
        let sampled_bytes = sampled
            .readback_buffer("second bytes", sampled_buffer, 0, 16)
            .unwrap();
        let sampled = sampled.compile().unwrap();

        let mut writer = Graph::new();
        let write_image = writer.import_image("write image", &source_image).unwrap();
        let write_buffer = writer.import_buffer("write bytes", &source_buffer).unwrap();
        writer
            .upload_image("replace image", write_image, vec![91; 16])
            .unwrap();
        writer
            .upload_buffer("partial replacement", write_buffer, 4, vec![73; 8])
            .unwrap();
        let written_image = writer.readback_image("written image", write_image).unwrap();
        let written_buffer = writer
            .readback_buffer("written bytes", write_buffer, 0, 16)
            .unwrap();
        let writer = writer.compile().unwrap();

        let mut a = runtime.submit(&compute).unwrap();
        let mut b = runtime.submit(&sampled).unwrap();
        assert_eq!(
            runtime.refresh_image(source_image.key()).unwrap(),
            source_image
        );
        assert_eq!(
            runtime.refresh_buffer(source_buffer.key()).unwrap(),
            source_buffer
        );
        {
            let latest = runtime.entry(source_image.key()).unwrap().physical().lock();
            assert_eq!(latest.scheduled.revision, source_image.revision);
            assert_eq!(
                latest.scheduled.layout,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            );
            assert!(latest.scheduled.serial > source_image.revision);
        }
        // This writer must order against the latest readers, even though its
        // compiled imports predate them and its content version remains valid.
        let mut c = runtime.submit(&writer).unwrap();
        assert_eq!(
            runtime.submit(&sampled).err().unwrap().kind(),
            RuntimeErrorKind::StaleResource
        );
        assert_eq!(
            runtime.submit(&writer).err().unwrap().kind(),
            RuntimeErrorKind::StaleResource
        );
        let mut a_result = completed(&mut a);
        let mut b_result = completed(&mut b);
        let mut c_result = completed(&mut c);
        assert_eq!(
            a_result.readback(compute_pixels).unwrap(),
            [245, 235, 225, 255].repeat(4)
        );
        assert_eq!(a_result.readback(compute_bytes).unwrap(), bytes);
        assert_eq!(b_result.readback(sampled_pixels).unwrap(), pixels);
        assert_eq!(b_result.readback(sampled_bytes).unwrap(), bytes);
        assert_eq!(c_result.readback(written_image).unwrap(), [91; 16]);
        let mut replaced = bytes;
        replaced[4..12].fill(73);
        assert_eq!(c_result.readback(written_buffer).unwrap(), replaced);
        runtime.delete(source_image.key()).unwrap();
        runtime.delete(source_buffer.key()).unwrap();
    });
}

#[test]
#[ignore = "explicit real-GPU clear/discard content revisions; requires Core/SyncVal"]
fn vulkan_graph_clear_and_attachment_discard_invalidate_old_content_snapshots() {
    real_gpu(|runtime| {
        let mut initial = Graph::new();
        let color = initial.image("color", ImageDesc::rgba8(2, 2)).unwrap();
        let depth = initial.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        initial
            .clear_attachments("initial clear", color, depth, [0.0; 4], 1.0)
            .unwrap();
        initial.export_image(color).unwrap();
        initial.export_image(depth).unwrap();
        let mut first = runtime.submit(&initial.compile().unwrap()).unwrap();
        let color = first.export_image(color).unwrap();
        let depth = first.export_image(depth).unwrap();
        completed(&mut first);

        let mut old_reader = Graph::new();
        let old_color = old_reader.import_image("old color", &color).unwrap();
        old_reader.readback_image("old read", old_color).unwrap();
        let old_reader = old_reader.compile().unwrap();
        let mut clear = Graph::new();
        let clear_color = clear.import_image("clear color", &color).unwrap();
        let clear_depth = clear.import_image("clear depth", &depth).unwrap();
        clear
            .clear_attachments("same value clear", clear_color, clear_depth, [0.0; 4], 1.0)
            .unwrap();
        let mut cleared = runtime.submit(&clear.compile().unwrap()).unwrap();
        assert_eq!(
            runtime.submit(&old_reader).err().unwrap().kind(),
            RuntimeErrorKind::StaleResource
        );
        let new_color = runtime.refresh_image(color.key()).unwrap();
        let new_depth = runtime.refresh_image(depth.key()).unwrap();
        assert!(new_color.initialized && new_depth.initialized);
        assert_ne!(
            new_color.revision, color.revision,
            "clear is a write even for identical values"
        );

        let mut discard = Graph::new();
        let color_target = discard.import_image("discard color", &new_color).unwrap();
        let depth_target = discard.import_image("discard depth", &new_depth).unwrap();
        let vertices = discard.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        let indices = discard.buffer("indices", BufferDesc { size: 12 }).unwrap();
        discard
            .upload_buffer("vertices", vertices, 0, vec![0; 84])
            .unwrap();
        discard
            .upload_buffer("indices", indices, 0, vec![0; 12])
            .unwrap();
        let discard_ops = AttachmentOps {
            load: LoadAction::Load,
            store: StoreOp::Discard,
        };
        discard
            .raster(
                "discard",
                RasterDraw {
                    vertices,
                    indices,
                    index_count: 3,
                    parameters: RasterParameters::default(),
                },
                RasterAttachments {
                    color: Attachment {
                        image: color_target,
                        load: match discard_ops.load {
                            LoadAction::Clear => LoadOp::Clear([0.0; 4]),
                            LoadAction::Load => LoadOp::Load,
                            LoadAction::Discard => LoadOp::Discard,
                        },
                        store: discard_ops.store,
                    },
                    depth: Attachment {
                        image: depth_target,
                        load: match discard_ops.load {
                            LoadAction::Clear => LoadOp::Clear(1.0),
                            LoadAction::Load => LoadOp::Load,
                            LoadAction::Discard => LoadOp::Discard,
                        },
                        store: discard_ops.store,
                    },
                },
            )
            .unwrap();
        let mut discarded = runtime.submit(&discard.compile().unwrap()).unwrap();
        let undefined = runtime.refresh_image(color.key()).unwrap();
        assert!(!undefined.initialized);
        assert_ne!(undefined.revision, new_color.revision);
        let mut undefined_reader = Graph::new();
        let image = undefined_reader
            .import_image("undefined color", &undefined)
            .unwrap();
        undefined_reader
            .readback_image("undefined read", image)
            .unwrap();
        assert_eq!(
            undefined_reader.compile().unwrap_err().kind(),
            GraphErrorKind::UninitializedRead
        );
        completed(&mut cleared);
        completed(&mut discarded);
        runtime.delete(color.key()).unwrap();
        runtime.delete(depth.key()).unwrap();
    });
}
