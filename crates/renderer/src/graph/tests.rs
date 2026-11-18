#![allow(
    clippy::single_range_in_vec_init,
    reason = "Typed fixtures intentionally describe a single initialized byte interval."
)]

//! Builder contract and ownership regressions.

use super::*;
use std::sync::Arc;

// Fixture preparation is separate from the normal-frame allocation harness.
// Every caller retains the pool until its builder and plans have been released.
fn prepared_graph(capacity: GraphCapacity) -> (GraphPool, Graph) {
    let mut pool = GraphPool::new(capacity, 1).unwrap();
    let graph = pool.begin().unwrap();
    (pool, graph)
}

fn batch_draws(draws: Vec<(BufferHandle, BufferHandle, u32, RasterParameters)>) -> Vec<RasterDraw> {
    draws
        .into_iter()
        .map(|(vertices, indices, index_count, parameters)| RasterDraw {
            bindings: None,
            vertices,
            indices,
            index_count,
            parameters: parameters.into(),
        })
        .collect()
}

#[test]
fn sliced_uploads_share_backing_and_initialize_only_selected_bytes() {
    let input: Arc<[u8]> = Arc::from([99, 1, 2, 3, 4, 5, 6, 7, 8, 88]);
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let buffer = graph
        .buffer("sliced destination", BufferDesc { size: 8 })
        .unwrap();
    let first = graph
        .upload_buffer_slice("first chunk", buffer, 0, Arc::clone(&input), 1..5)
        .unwrap();
    let second = graph
        .upload_buffer_slice("second chunk", buffer, 4, Arc::clone(&input), 5..9)
        .unwrap();
    graph.export_buffer(buffer).unwrap();
    let plan = graph.compile().unwrap();
    for (pass, expected_source, expected_target) in [(first, 1..5, 0..4), (second, 5..9, 4..8)] {
        let Operation::UploadBuffer {
            data: UploadSource::Shared { bytes, source },
            ..
        } = &plan.passes[pass.index].operation
        else {
            panic!("live sliced upload must retain its source");
        };
        assert!(Arc::ptr_eq(bytes, &input));
        assert_eq!(*source, expected_source);
        assert_eq!(plan.accesses(pass.index)[0].range, expected_target);
    }
    let (_partial_pool, mut partial) = prepared_graph(GraphCapacity::default());
    let target = partial.buffer("partial", BufferDesc { size: 8 }).unwrap();
    partial
        .upload_buffer_slice("tail only", target, 4, Arc::clone(&input), 1..5)
        .unwrap();
    partial.export_buffer(target).unwrap();
    let error = partial.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
    assert_eq!(error.range(), Some(&AccessRange::Buffer(0..4)));
}

#[test]
fn invalid_upload_slices_preserve_builder_context_without_a_pass() {
    let input: Arc<[u8]> = vec![0; 16].into();
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let buffer = graph.buffer("destination", BufferDesc { size: 8 }).unwrap();
    for source in [Range { start: 8, end: 4 }, 13..17, 4..4, 0..12] {
        let error = graph
            .upload_buffer_slice("invalid chunk", buffer, 0, Arc::clone(&input), source)
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("invalid chunk"));
        assert_eq!(error.resource_name(), Some("destination"));
        assert!(graph.passes.is_empty());
    }
    // Keep the existing copy contract: offsets are aligned, but a source
    // slice may start anywhere and need not contain a multiple of four bytes.
    graph
        .upload_buffer_slice("three bytes", buffer, 0, Arc::clone(&input), 1..4)
        .unwrap();
}

#[test]
fn many_exports_keep_declaration_order_and_indexed_membership() {
    const COUNT: usize = 4_096;
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity {
        resources: COUNT + 1,
        passes: COUNT,
        accesses: COUNT,
        name_bytes: 65_536,
        segments: COUNT + 1,
        ..GraphCapacity::default()
    });
    let bytes = [1; 4];
    let buffers = (0..COUNT)
        .map(|_| {
            let buffer = graph.buffer("asset", BufferDesc { size: 4 }).unwrap();
            graph.upload_buffer("upload", buffer, 0, &bytes).unwrap();
            buffer
        })
        .collect::<Vec<_>>();
    let unused = graph.buffer("unexported", BufferDesc { size: 4 }).unwrap();
    for &buffer in buffers.iter().rev() {
        graph.export_buffer(buffer).unwrap();
        graph.export_buffer(buffer).unwrap();
    }
    let expected = buffers
        .iter()
        .rev()
        .map(|buffer| buffer.index)
        .collect::<Vec<_>>();
    assert_eq!(graph.exports, expected);
    let compiled = graph.compile().unwrap();
    assert_eq!(compiled.exports, expected);
    assert_eq!(compiled.pass_count(), buffers.len());
    assert!(
        buffers
            .iter()
            .all(|buffer| compiled.has_export(buffer.index))
    );
    assert!(!compiled.has_export(unused.index));
    assert!(!compiled.has_export(usize::MAX));
}

#[test]
fn compilation_retains_anchored_mesh_bytes_and_captured_uploads_until_recycle() {
    // Shared sources model the private, immutable mesh-version path. Their
    // asset anchors remain alive through every ordinary pool reuse.
    let buffer_bytes: Arc<[u8]> = vec![1; 4].into();
    let dead_buffer_bytes: Arc<[u8]> = vec![3; 4].into();
    let mut image_bytes = [2; 4];
    let (mut graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let buffer = graph.buffer("live buffer", BufferDesc { size: 4 }).unwrap();
    let image = graph.image("live image", ImageDesc::rgba8(1, 1)).unwrap();
    let dead_buffer = graph.buffer("dead buffer", BufferDesc { size: 4 }).unwrap();
    let dead_image = graph.image("dead image", ImageDesc::rgba8(1, 1)).unwrap();
    let buffer_upload = graph
        .upload_buffer_slice(
            "live buffer upload",
            buffer,
            0,
            Arc::clone(&buffer_bytes),
            0..4,
        )
        .unwrap();
    let image_upload = graph
        .upload_image("live image upload", image, &image_bytes)
        .unwrap();
    image_bytes.fill(9);
    let dead_buffer_upload = graph
        .upload_buffer_slice(
            "dead buffer upload",
            dead_buffer,
            0,
            Arc::clone(&dead_buffer_bytes),
            0..4,
        )
        .unwrap();
    let dead_image_upload = graph
        .upload_image("dead image upload", dead_image, &[4; 4])
        .unwrap();
    graph.export_buffer(buffer).unwrap();
    graph.export_image(image).unwrap();

    let plan = graph.compile().unwrap();
    let Operation::UploadBuffer {
        data: UploadSource::Shared { bytes, .. },
        ..
    } = &plan.passes[buffer_upload.index].operation
    else {
        panic!("live buffer upload must remain executable");
    };
    assert!(Arc::ptr_eq(bytes, &buffer_bytes));
    assert_eq!(
        plan.upload_data(&plan.passes[image_upload.index].operation),
        &[2; 4]
    );
    assert_eq!(plan.pass_count(), 2);
    assert!(!plan.order.contains(&dead_buffer_upload.index));
    assert!(!plan.order.contains(&dead_image_upload.index));
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["live buffer upload", "live image upload"]
    );

    let consumer = plan.clone();
    assert_eq!(
        graph_pool.begin().unwrap_err().kind(),
        GraphErrorKind::CapacityExhausted
    );
    drop(plan);
    assert_eq!(
        graph_pool.begin().unwrap_err().kind(),
        GraphErrorKind::CapacityExhausted
    );
    assert_eq!(
        consumer.upload_data(&consumer.passes[image_upload.index].operation),
        &[2; 4]
    );
    drop(consumer);
    // Recycle releases Graph references, while the immutable asset anchors
    // prevent a final payload deallocation in this ordinary reset path.
    let _recycled = graph_pool.begin().unwrap();
    assert_eq!(Arc::strong_count(&buffer_bytes), 1);
    assert_eq!(Arc::strong_count(&dead_buffer_bytes), 1);
}

#[test]
fn typed_handles_reject_foreign_undefined_and_wrong_kind_resources() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let buffer = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
    let image = graph.image("rgba", ImageDesc::rgba8(2, 2)).unwrap();
    let (_other_pool, mut other) = prepared_graph(GraphCapacity::default());
    assert!(other.upload_buffer("foreign", buffer, 0, &[1; 4]).is_err());
    assert!(
        graph
            .upload_buffer(
                "undefined",
                BufferHandle {
                    graph: graph.id,
                    index: 100
                },
                0,
                &[1; 4]
            )
            .is_err()
    );
    assert!(
        graph
            .upload_buffer(
                "wrong kind",
                BufferHandle {
                    graph: graph.id,
                    index: image.index
                },
                0,
                &[1; 4]
            )
            .is_err()
    );
    assert!(other.export_image(image).is_err());
}

#[test]
fn zero_extents_overflows_and_invalid_byte_ranges_are_rejected() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    assert!(graph.buffer("empty", BufferDesc { size: 0 }).is_err());
    assert!(graph.image("zero", ImageDesc::rgba8(0, 2)).is_err());
    assert!(
        graph
            .image("overflow", ImageDesc::rgba8(u32::MAX, u32::MAX))
            .is_err()
    );
    let b = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
    assert!(graph.upload_buffer("empty", b, 0, &[]).is_err());
    assert!(graph.upload_buffer("alignment", b, 1, &[0; 4]).is_err());
    assert!(graph.upload_buffer("past end", b, 16, &[0]).is_err());
    assert!(
        graph
            .copy_buffer("overflow", b, u64::MAX - 3, b, 0, 8)
            .is_err()
    );
    assert!(graph.copy_buffer("overlap", b, 0, b, 4, 8).is_err());
}

#[test]
fn repeated_imports_share_one_identity_and_reject_changed_snapshots() {
    let mut range_pool = ImportRangesPool::new(1, 1).unwrap();
    let initial = 0..8;
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let import = ImportBuffer::new(
        ResourceKey {
            owner: 4,
            index: 2,
            generation: 3,
        },
        9,
        BufferDesc { size: 16 },
        range_pool.capture(std::slice::from_ref(&initial)).unwrap(),
    )
    .unwrap();
    let first = graph.import_buffer("first", &import).unwrap();
    let again = graph.import_buffer("again", &import).unwrap();
    assert_eq!(first, again);
    assert_eq!(graph.resources.len(), 1);
    let changed = ImportBuffer::new(
        import.key(),
        import.revision() + 1,
        import.desc(),
        import.initialized().clone(),
    )
    .unwrap();
    let error = graph.import_buffer("stale alias", &changed).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
    assert_eq!(error.resource(), Some(first.into()));
    assert_eq!(error.resource_name(), Some("first"));
    let image = ImportImage::test_initialized(import.key(), 9, ImageDesc::rgba8(2, 2));
    assert!(graph.import_image("wrong kind alias", &image).is_err());
}

#[test]
fn image_metadata_preserves_imports_and_rejects_invalid_local_handles() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let source = ImportImage::new(
        ResourceKey {
            owner: 7,
            index: 4,
            generation: 2,
        },
        3,
        ImageDesc::depth32(9, 5),
        &[],
    )
    .unwrap();
    let image = graph.import_image("imported depth", &source).unwrap();
    assert_eq!(graph.image_desc(image).unwrap(), source.desc());
    assert_eq!(graph.import_image("alias", &source).unwrap(), image);
    let changed =
        ImportImage::new(source.key(), source.revision(), ImageDesc::rgba8(9, 5), &[]).unwrap();
    let error = graph
        .import_image("different description", &changed)
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
    assert_eq!(error.resource(), Some(image.into()));

    let buffer = graph.buffer("buffer", BufferDesc { size: 4 }).unwrap();
    for invalid in [
        ImageHandle {
            graph: graph.id,
            index: buffer.index,
        },
        ImageHandle {
            graph: graph.id,
            index: usize::MAX,
        },
    ] {
        let error = graph.image_desc(invalid).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidHandle);
        assert_eq!(error.resource(), Some(invalid.into()));
    }
}

#[test]
fn only_supported_whole_image_subresources_are_accepted() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let image = graph.image("rgba", ImageDesc::rgba8(2, 2)).unwrap();
    for range in [
        AccessRange::Image {
            mip: 1,
            layer: 0,
            aspect: ImageAspect::Color,
        },
        AccessRange::Image {
            mip: 0,
            layer: 1,
            aspect: ImageAspect::Color,
        },
        AccessRange::Image {
            mip: 0,
            layer: 0,
            aspect: ImageAspect::Depth,
        },
        AccessRange::Buffer(0..1),
    ] {
        assert!(
            graph
                .declare_pass(
                    "unsupported",
                    PassKind::Compute,
                    &[Access {
                        resource: image.into(),
                        range,
                        kind: AccessKind::Write,
                        purpose: AccessPurpose::StorageWrite
                    }],
                    false
                )
                .is_err()
        );
    }
    graph
        .declare_pass(
            "whole",
            PassKind::Compute,
            &[Access {
                resource: image.into(),
                range: AccessRange::Image {
                    mip: 0,
                    layer: 0,
                    aspect: ImageAspect::Color,
                },
                kind: AccessKind::Write,
                purpose: AccessPurpose::StorageWrite,
            }],
            true,
        )
        .unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 1);
}

#[test]
fn declaration_conflicts_keep_first_pair_context_and_validate_inputs_first() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let earlier_resource = graph
        .buffer("earlier resource", BufferDesc { size: 16 })
        .unwrap();
    let first_access = graph
        .buffer("first access", BufferDesc { size: 16 })
        .unwrap();
    let access = |buffer: BufferHandle, range, write| Access {
        resource: buffer.into(),
        range: AccessRange::Buffer(range),
        kind: if write {
            AccessKind::Write
        } else {
            AccessKind::Read
        },
        purpose: if write {
            AccessPurpose::CopyDestination
        } else {
            AccessPurpose::CopySource
        },
    };
    let mut accesses = vec![
        access(first_access, 4..12, false),
        access(earlier_resource, 0..4, true),
        access(first_access, 8..16, true),
        access(earlier_resource, 0..4, true),
    ];
    let before = graph.passes.len();
    let error = graph
        .declare_pass("conflicts", PassKind::Copy, &accesses, false)
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass_name(), Some("conflicts"));
    assert_eq!(error.resource(), Some(first_access.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(4..12)));
    assert_eq!(graph.passes.len(), before);

    accesses.push(access(earlier_resource, 16..20, false));
    let error = graph
        .declare_pass("invalid range first", PassKind::Copy, &accesses, false)
        .unwrap_err();
    assert_eq!(error.resource(), Some(earlier_resource.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(16..20)));
    assert!(error.message.contains("byte range"));
    assert_eq!(graph.passes.len(), before);
}

#[test]
fn declaration_allows_many_overlapping_readers_then_rejects_a_writer() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity {
        // Include the already-published reader pass and the attempted pass so
        // the latter reaches its access-conflict check before rollback.
        accesses: 8_192 + 8_193,
        ..GraphCapacity::default()
    });
    let buffer = graph.buffer("source", BufferDesc { size: 64 }).unwrap();
    let mut accesses = vec![
        Access {
            resource: buffer.into(),
            range: AccessRange::Buffer(0..64),
            kind: AccessKind::Read,
            purpose: AccessPurpose::CopySource,
        };
        8_192
    ];
    graph
        .declare_pass("many readers", PassKind::Copy, &accesses, false)
        .unwrap();
    accesses.push(Access {
        resource: buffer.into(),
        range: AccessRange::Buffer(32..64),
        kind: AccessKind::Write,
        purpose: AccessPurpose::CopyDestination,
    });
    let error = graph
        .declare_pass("reader and writer", PassKind::Copy, &accesses, false)
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.resource(), Some(buffer.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(0..64)));
}

#[test]
fn fixed_operations_check_binding_formats_extents_and_aliasing() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let a = graph.image("a", ImageDesc::rgba8(2, 2)).unwrap();
    let b = graph.image("b", ImageDesc::rgba8(3, 2)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
    assert!(graph.upload_image("short", a, &[0; 15]).is_err());
    assert!(graph.upload_image("depth bytes", depth, &[0; 16]).is_err());
    assert!(
        graph
            .compute("alias", a, a, ComputeTransform::Invert)
            .is_err()
    );
    assert!(
        graph
            .compute("extent", a, b, ComputeTransform::Invert)
            .is_err()
    );
    assert!(graph.composite("depth", depth, a).is_err());
    assert!(
        graph
            .declare_pass(
                "bad binding",
                PassKind::Compute,
                &[Access {
                    resource: a.into(),
                    range: AccessRange::Image {
                        mip: 0,
                        layer: 0,
                        aspect: ImageAspect::Color
                    },
                    kind: AccessKind::Write,
                    purpose: AccessPurpose::Sampled
                }],
                true
            )
            .is_err()
    );
}

fn mesh(graph: &mut Graph) -> (BufferHandle, BufferHandle, ImageHandle, ImageHandle) {
    let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    graph
        .upload_buffer("upload vertices", vertices, 0, &[0; 84])
        .unwrap();
    graph
        .upload_buffer("upload indices", indices, 0, &[0; 12])
        .unwrap();
    let color = graph.image("color", ImageDesc::rgba8(2, 2)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
    (vertices, indices, color, depth)
}

#[test]
fn raster_checks_clear_payloads_and_requires_none_for_load_or_discard() {
    for (color_load, depth_load, valid) in [
        (LoadOp::Load, LoadOp::Load, true),
        (LoadOp::Discard, LoadOp::Discard, true),
        (LoadOp::Clear([0.0; 4]), LoadOp::Clear(1.0), true),
        (LoadOp::Clear([f32::NAN; 4]), LoadOp::Load, false),
        (LoadOp::Load, LoadOp::Clear(f32::NAN), false),
        (LoadOp::Discard, LoadOp::Clear(-0.1), false),
        (LoadOp::Load, LoadOp::Clear(1.1), false),
    ] {
        let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
        let (vertices, indices, color, depth) = mesh(&mut graph);
        let result = graph.raster(
            "attachment input",
            &crate::program::ManagedRasterProgram::mesh(),
            [RasterDraw {
                bindings: None,
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default().into(),
            }],
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: color_load,
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: depth_load,
                    store: StoreOp::Store,
                }),
            },
        );
        assert_eq!(result.is_ok(), valid);
        if let Err(error) = result {
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.pass_name(), Some("attachment input"));
        }
    }
}

#[test]
fn attachment_load_needs_contents_and_store_discard_invalidates_them() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let (v, i, c, d) = mesh(&mut graph);
    graph
        .raster(
            "undefined load",
            &crate::program::ManagedRasterProgram::mesh(),
            [RasterDraw {
                bindings: None,
                vertices: v,
                indices: i,
                index_count: 3,
                parameters: RasterParameters::default().into(),
            }],
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(c, 0, 0).unwrap(),
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(d, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    assert!(
        graph
            .compile()
            .unwrap_err()
            .to_string()
            .contains("uninitialized")
    );

    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let (v, i, c, d) = mesh(&mut graph);
    graph
        .raster(
            "discarded",
            &crate::program::ManagedRasterProgram::mesh(),
            [RasterDraw {
                bindings: None,
                vertices: v,
                indices: i,
                index_count: 3,
                parameters: RasterParameters::default().into(),
            }],
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(c, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Discard,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(d, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    graph.readback_image("read discarded", c).unwrap();
    assert!(
        graph
            .compile()
            .unwrap_err()
            .to_string()
            .contains("uninitialized")
    );
}

#[test]
fn partial_raster_after_discard_does_not_claim_full_initialization() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let (v, i, c, d) = mesh(&mut graph);
    graph
        .raster(
            "partial",
            &crate::program::ManagedRasterProgram::mesh(),
            [RasterDraw {
                bindings: None,
                vertices: v,
                indices: i,
                index_count: 3,
                parameters: RasterParameters::default().into(),
            }],
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(c, 0, 0).unwrap(),
                    load: LoadOp::Discard,
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(d, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    graph.export_image(c).unwrap();
    assert!(
        graph
            .compile()
            .unwrap_err()
            .to_string()
            .contains("uninitialized")
    );
}

#[test]
fn batch_requires_initialized_buffers_from_later_draws() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let (vertices, indices, color, depth) = mesh(&mut graph);
    let missing = graph
        .buffer("later vertices", BufferDesc { size: 84 })
        .unwrap();
    graph
        .raster(
            "opaque batch",
            &crate::program::ManagedRasterProgram::mesh(),
            batch_draws(vec![
                (vertices, indices, 3, RasterParameters::default()),
                (missing, indices, 3, RasterParameters::default()),
            ]),
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    graph.export_image(color).unwrap();
    let error = graph.compile().unwrap_err().to_string();
    assert!(
        error.contains("later vertices") && error.contains("uninitialized"),
        "{error}"
    );
}

#[test]
fn batch_same_buffer_can_supply_vertex_and_index_reads() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let (vertices, _, color, depth) = mesh(&mut graph);
    graph
        .raster(
            "opaque batch",
            &crate::program::ManagedRasterProgram::mesh(),
            batch_draws(vec![
                (vertices, vertices, 3, RasterParameters::default());
                3
            ]),
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    graph.export_image(color).unwrap();
    let compiled = graph.compile().unwrap();
    assert_eq!(compiled.pass_count(), 2);
    #[cfg(feature = "graph-diagnostics")]
    {
        let dump = compiled.dump();
        assert!(dump.contains("purpose=Vertex") && dump.contains("purpose=Index"));
    }
}

#[test]
fn batch_rejections_preserve_builder_context_without_partial_declarations() {
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let (vertices, indices, color, depth) = mesh(&mut graph);
    let before = graph.passes.len();
    let error = graph
        .raster(
            "invalid opaque input",
            &crate::program::ManagedRasterProgram::mesh(),
            batch_draws(vec![
                (vertices, indices, 3, RasterParameters::default()),
                (vertices, indices, 6, RasterParameters::default()),
            ]),
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass_name(), Some("invalid opaque input"));
    assert_eq!(error.pass(), None);
    assert_eq!(error.resource(), Some(indices.into()));
    assert_eq!(error.resource_name(), Some("indices"));
    assert_eq!(graph.passes.len(), before);

    let (_other_pool, mut other) = prepared_graph(GraphCapacity::default());
    let foreign = other
        .buffer("foreign vertices", BufferDesc { size: 84 })
        .unwrap();
    let error = graph
        .raster(
            "foreign opaque input",
            &crate::program::ManagedRasterProgram::mesh(),
            batch_draws(vec![(foreign, indices, 3, RasterParameters::default())]),
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    assert_eq!(error.pass_name(), Some("foreign opaque input"));
    assert_eq!(error.resource(), Some(foreign.into()));
    assert_eq!(error.resource_name(), None);
    assert_eq!(graph.passes.len(), before);
}

#[test]
fn import_identity_survives_interleaved_transients_and_failed_aliases() {
    let mut range_pool = ImportRangesPool::new(1, 1).unwrap();
    let initial = 0..12;
    let initialized = range_pool.capture(std::slice::from_ref(&initial)).unwrap();
    let (_graph_pool, mut graph) = prepared_graph(GraphCapacity::default());
    let sources = (0..64)
        .map(|index| {
            ImportBuffer::new(
                ResourceKey {
                    owner: 7,
                    index,
                    generation: 1,
                },
                u64::from(index) + 1,
                BufferDesc { size: 12 },
                initialized.clone(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut handles = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        graph.image("transient", ImageDesc::rgba8(2, 2)).unwrap();
        handles.push(
            graph
                .import_buffer(&format!("import {index}"), source)
                .unwrap(),
        );
    }
    for (source, &handle) in sources.iter().zip(&handles).rev() {
        assert_eq!(graph.import_buffer("alias", source).unwrap(), handle);
    }
    let first = &sources[0];
    let image_alias =
        ImportImage::test_initialized(first.key(), first.revision(), ImageDesc::rgba8(1, 3));
    let error = graph.import_image("wrong kind", &image_alias).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
    assert_eq!(error.resource(), Some(handles[0].into()));
    assert_eq!(error.resource_name(), Some("import 0"));
    assert_eq!(
        graph.import_buffer("still canonical", first).unwrap(),
        handles[0]
    );
    assert_eq!(graph.resources.len(), sources.len() * 2);
}

#[test]
fn import_and_batch_limits_report_their_distinct_capacity_dimensions() {
    let (_pool, mut graph) = prepared_graph(GraphCapacity {
        initial_ranges: 0,
        draws: 0,
        ..GraphCapacity::default()
    });
    let mut ranges = ImportRangesPool::new(1, 1).unwrap();
    let initialized = ranges.capture(&[0..4]).unwrap();
    let source = ImportBuffer::new(
        ResourceKey {
            owner: 1,
            index: 0,
            generation: 1,
        },
        1,
        BufferDesc { size: 4 },
        initialized,
    )
    .unwrap();
    let details = graph
        .import_buffer("input", &source)
        .unwrap_err()
        .capacity()
        .unwrap();
    assert_eq!(details.kind, GraphCapacityKind::InitialRanges);
    assert_eq!((details.limit, details.required), (0, Some(1)));
    assert!(graph.resources.is_empty());
    let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    let color = graph.image("color", ImageDesc::rgba8(1, 1)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(1, 1)).unwrap();
    let error = graph
        .raster(
            "batch",
            &crate::program::ManagedRasterProgram::mesh(),
            batch_draws(vec![(vertices, indices, 3, RasterParameters::default())]),
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap_err();
    let details = error.capacity().unwrap();
    assert_eq!(details.kind, GraphCapacityKind::Draws);
    assert_eq!((details.limit, details.required), (0, Some(1)));
    assert!(graph.passes.is_empty());
    assert!(graph.draws.is_empty());
    assert!(graph.accesses.is_empty());
}
