//! Builder contract and ownership regressions.

use super::*;
use std::sync::Arc;

fn batch_draws(
    draws: Vec<(BufferHandle, BufferHandle, u32, RasterParameters)>,
) -> impl ExactSizeIterator<
    Item = Result<(BufferHandle, BufferHandle, u32, ValidatedRasterParameters), GraphError>,
> {
    draws
        .into_iter()
        .map(|(vertices, indices, count, parameters)| {
            ValidatedRasterParameters::new(parameters)
                .map(|parameters| (vertices, indices, count, parameters))
        })
}

#[test]
fn sliced_uploads_share_backing_and_initialize_only_selected_bytes() {
    let input: Arc<[u8]> = Arc::from([99, 1, 2, 3, 4, 5, 6, 7, 8, 88]);
    let mut graph = Graph::new();
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
        let Operation::UploadBuffer { bytes, source, .. } = &plan.passes[pass.index].operation
        else {
            panic!("live sliced upload must retain its source");
        };
        assert!(Arc::ptr_eq(bytes, &input));
        assert_eq!(*source, expected_source);
        assert_eq!(plan.passes[pass.index].accesses[0].range, expected_target);
    }
    let mut partial = Graph::new();
    let target = partial.buffer("partial", BufferDesc { size: 8 }).unwrap();
    partial
        .upload_buffer_slice("tail only", target, 4, input, 1..5)
        .unwrap();
    partial.export_buffer(target).unwrap();
    let error = partial.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
    assert_eq!(error.range(), Some(&AccessRange::Buffer(0..4)));
}

#[test]
fn invalid_upload_slices_preserve_builder_context_without_a_pass() {
    let input: Arc<[u8]> = vec![0; 16].into();
    let mut graph = Graph::new();
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
        .upload_buffer_slice("three bytes", buffer, 0, input, 1..4)
        .unwrap();
}

#[test]
fn many_exports_keep_declaration_order_and_indexed_membership() {
    let mut graph = Graph::new();
    let bytes: Arc<[u8]> = vec![1; 4].into();
    let buffers = (0..4_096)
        .map(|_| {
            let buffer = graph.buffer("asset", BufferDesc { size: 4 }).unwrap();
            graph
                .upload_buffer_shared("upload", buffer, 0, Arc::clone(&bytes))
                .unwrap();
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
fn compilation_shares_live_uploads_and_releases_culled_payloads() {
    let buffer_bytes: Arc<[u8]> = vec![1; 4].into();
    let image_bytes: Arc<[u8]> = vec![2; 4].into();
    let dead_buffer_bytes: Arc<[u8]> = vec![3; 4].into();
    let dead_image_bytes: Arc<[u8]> = vec![4; 4].into();
    let buffer_lifetime = Arc::downgrade(&buffer_bytes);
    let image_lifetime = Arc::downgrade(&image_bytes);
    let dead_buffer_lifetime = Arc::downgrade(&dead_buffer_bytes);
    let dead_image_lifetime = Arc::downgrade(&dead_image_bytes);
    let mut graph = Graph::new();
    let buffer = graph.buffer("live buffer", BufferDesc { size: 4 }).unwrap();
    let image = graph.image("live image", ImageDesc::rgba8(1, 1)).unwrap();
    let dead_buffer = graph.buffer("dead buffer", BufferDesc { size: 4 }).unwrap();
    let dead_image = graph.image("dead image", ImageDesc::rgba8(1, 1)).unwrap();
    let buffer_upload = graph
        .upload_buffer_shared("live buffer upload", buffer, 0, Arc::clone(&buffer_bytes))
        .unwrap();
    let image_upload = graph
        .upload_image_shared("live image upload", image, Arc::clone(&image_bytes))
        .unwrap();
    let dead_buffer_upload = graph
        .upload_buffer_shared(
            "dead buffer upload",
            dead_buffer,
            0,
            Arc::clone(&dead_buffer_bytes),
        )
        .unwrap();
    let dead_image_upload = graph
        .upload_image_shared(
            "dead image upload",
            dead_image,
            Arc::clone(&dead_image_bytes),
        )
        .unwrap();
    graph.export_buffer(buffer).unwrap();
    graph.export_image(image).unwrap();

    let plan = graph.compile().unwrap();
    let Operation::UploadBuffer { bytes, .. } = &plan.passes[buffer_upload.index].operation else {
        panic!("live buffer upload must remain executable");
    };
    assert!(Arc::ptr_eq(bytes, &buffer_bytes));
    let Operation::UploadImage { bytes, .. } = &plan.passes[image_upload.index].operation else {
        panic!("live image upload must remain executable");
    };
    assert!(Arc::ptr_eq(bytes, &image_bytes));
    assert!(matches!(
        plan.passes[dead_buffer_upload.index].operation,
        Operation::Culled
    ));
    assert!(matches!(
        plan.passes[dead_image_upload.index].operation,
        Operation::Culled
    ));

    drop((
        buffer_bytes,
        image_bytes,
        dead_buffer_bytes,
        dead_image_bytes,
    ));
    drop(graph);
    assert!(buffer_lifetime.upgrade().is_some());
    assert!(image_lifetime.upgrade().is_some());
    assert!(dead_buffer_lifetime.upgrade().is_none());
    assert!(dead_image_lifetime.upgrade().is_none());
    drop(plan);
    assert!(buffer_lifetime.upgrade().is_none());
    assert!(image_lifetime.upgrade().is_none());
}

#[test]
fn typed_handles_reject_foreign_undefined_and_wrong_kind_resources() {
    let mut graph = Graph::new();
    let buffer = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
    let image = graph.image("rgba", ImageDesc::rgba8(2, 2)).unwrap();
    let mut other = Graph::new();
    assert!(
        other
            .upload_buffer("foreign", buffer, 0, vec![1; 4])
            .is_err()
    );
    assert!(
        graph
            .upload_buffer(
                "undefined",
                BufferHandle {
                    graph: graph.id,
                    index: 100
                },
                0,
                vec![1; 4]
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
                vec![1; 4]
            )
            .is_err()
    );
    assert!(other.export_image(image).is_err());
}

#[test]
fn zero_extents_overflows_and_invalid_byte_ranges_are_rejected() {
    let mut graph = Graph::new();
    assert!(graph.buffer("empty", BufferDesc { size: 0 }).is_err());
    assert!(graph.image("zero", ImageDesc::rgba8(0, 2)).is_err());
    assert!(
        graph
            .image("overflow", ImageDesc::rgba8(u32::MAX, u32::MAX))
            .is_err()
    );
    let b = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
    assert!(graph.upload_buffer("empty", b, 0, vec![]).is_err());
    assert!(graph.upload_buffer("alignment", b, 1, vec![0; 4]).is_err());
    assert!(graph.upload_buffer("past end", b, 16, vec![0]).is_err());
    assert!(
        graph
            .copy_buffer("overflow", b, u64::MAX - 3, b, 0, 8)
            .is_err()
    );
    assert!(graph.copy_buffer("overlap", b, 0, b, 4, 8).is_err());
}

#[test]
fn repeated_imports_share_one_identity_and_reject_changed_snapshots() {
    let mut graph = Graph::new();
    let import = ImportBuffer {
        key: ResourceKey {
            owner: 4,
            index: 2,
            generation: 3,
        },
        revision: 9,
        desc: BufferDesc { size: 16 },
        initialized: std::iter::once(0..8).collect(),
    };
    let first = graph.import_buffer("first", &import).unwrap();
    let again = graph.import_buffer("again", &import).unwrap();
    assert_eq!(first, again);
    assert_eq!(graph.resources.len(), 1);
    let mut changed = import.clone();
    changed.revision += 1;
    let error = graph.import_buffer("stale alias", &changed).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InconsistentImport);
    assert_eq!(error.resource(), Some(first.into()));
    assert_eq!(error.resource_name(), Some("first"));
    let image = ImportImage {
        key: import.key,
        revision: 9,
        desc: ImageDesc::rgba8(2, 2),
        initialized: true,
    };
    assert!(graph.import_image("wrong kind alias", &image).is_err());
}

#[test]
fn image_metadata_preserves_imports_and_rejects_invalid_local_handles() {
    let mut graph = Graph::new();
    let source = ImportImage {
        key: ResourceKey {
            owner: 7,
            index: 4,
            generation: 2,
        },
        revision: 3,
        desc: ImageDesc::depth32(9, 5),
        initialized: false,
    };
    let image = graph.import_image("imported depth", &source).unwrap();
    assert_eq!(graph.image_desc(image).unwrap(), source.desc);
    assert_eq!(graph.import_image("alias", &source).unwrap(), image);
    let mut changed = source.clone();
    changed.desc = ImageDesc::rgba8(9, 5);
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
    let mut graph = Graph::new();
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
                    vec![Access {
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
            vec![Access {
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
    let mut graph = Graph::new();
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
        .declare_pass("conflicts", PassKind::Copy, accesses.clone(), false)
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass_name(), Some("conflicts"));
    assert_eq!(error.resource(), Some(first_access.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(4..12)));
    assert_eq!(graph.passes.len(), before);

    accesses.push(access(earlier_resource, 16..20, false));
    let error = graph
        .declare_pass("invalid range first", PassKind::Copy, accesses, false)
        .unwrap_err();
    assert_eq!(error.resource(), Some(earlier_resource.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(16..20)));
    assert!(error.message.contains("byte range"));
    assert_eq!(graph.passes.len(), before);
}

#[test]
fn declaration_allows_many_overlapping_readers_then_rejects_a_writer() {
    let mut graph = Graph::new();
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
        .declare_pass("many readers", PassKind::Copy, accesses.clone(), false)
        .unwrap();
    accesses.push(Access {
        resource: buffer.into(),
        range: AccessRange::Buffer(32..64),
        kind: AccessKind::Write,
        purpose: AccessPurpose::CopyDestination,
    });
    let error = graph
        .declare_pass("reader and writer", PassKind::Copy, accesses, false)
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.resource(), Some(buffer.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(0..64)));
}

#[test]
fn fixed_operations_check_binding_formats_extents_and_aliasing() {
    let mut graph = Graph::new();
    let a = graph.image("a", ImageDesc::rgba8(2, 2)).unwrap();
    let b = graph.image("b", ImageDesc::rgba8(3, 2)).unwrap();
    let depth = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
    assert!(graph.upload_image("short", a, vec![0; 15]).is_err());
    assert!(
        graph
            .upload_image("depth bytes", depth, vec![0; 16])
            .is_err()
    );
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
                vec![Access {
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
        .upload_buffer("upload vertices", vertices, 0, vec![0; 84])
        .unwrap();
    graph
        .upload_buffer("upload indices", indices, 0, vec![0; 12])
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
        let mut graph = Graph::new();
        let (vertices, indices, color, depth) = mesh(&mut graph);
        let result = graph.raster(
            "attachment input",
            RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: color,
                    load: color_load,
                    store: StoreOp::Store,
                },
                depth: Attachment {
                    image: depth,
                    load: depth_load,
                    store: StoreOp::Store,
                },
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
    let mut graph = Graph::new();
    let (v, i, c, d) = mesh(&mut graph);
    graph
        .raster(
            "undefined load",
            RasterDraw {
                vertices: v,
                indices: i,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: c,
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                },
                depth: Attachment {
                    image: d,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                },
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

    let mut graph = Graph::new();
    let (v, i, c, d) = mesh(&mut graph);
    graph
        .raster(
            "discarded",
            RasterDraw {
                vertices: v,
                indices: i,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: c,
                    load: LoadOp::Clear([0.0; 4]),
                    store: StoreOp::Discard,
                },
                depth: Attachment {
                    image: d,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                },
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
    let mut graph = Graph::new();
    let (v, i, c, d) = mesh(&mut graph);
    graph
        .raster(
            "partial",
            RasterDraw {
                vertices: v,
                indices: i,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: c,
                    load: LoadOp::Discard,
                    store: StoreOp::Store,
                },
                depth: Attachment {
                    image: d,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                },
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
    let mut graph = Graph::new();
    let (vertices, indices, color, depth) = mesh(&mut graph);
    let missing = graph
        .buffer("later vertices", BufferDesc { size: 84 })
        .unwrap();
    graph
        .raster_batch(
            "opaque batch",
            batch_draws(vec![
                (vertices, indices, 3, RasterParameters::default()),
                (missing, indices, 3, RasterParameters::default()),
            ]),
            color,
            depth,
            [0.0; 4],
            1.0,
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
    let mut graph = Graph::new();
    let (vertices, _, color, depth) = mesh(&mut graph);
    graph
        .raster_batch(
            "opaque batch",
            batch_draws(vec![
                (vertices, vertices, 3, RasterParameters::default());
                3
            ]),
            color,
            depth,
            [0.0; 4],
            1.0,
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
    let mut graph = Graph::new();
    let (vertices, indices, color, depth) = mesh(&mut graph);
    let before = graph.passes.len();
    let error = graph
        .raster_batch(
            "invalid opaque input",
            batch_draws(vec![
                (vertices, indices, 3, RasterParameters::default()),
                (vertices, indices, 6, RasterParameters::default()),
            ]),
            color,
            depth,
            [0.0; 4],
            1.0,
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass_name(), Some("invalid opaque input"));
    assert_eq!(error.pass(), None);
    assert_eq!(error.resource(), Some(indices.into()));
    assert_eq!(error.resource_name(), Some("indices"));
    assert_eq!(graph.passes.len(), before);

    let mut other = Graph::new();
    let foreign = other
        .buffer("foreign vertices", BufferDesc { size: 84 })
        .unwrap();
    let error = graph
        .raster_batch(
            "foreign opaque input",
            batch_draws(vec![(foreign, indices, 3, RasterParameters::default())]),
            color,
            depth,
            [0.0; 4],
            1.0,
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    assert_eq!(error.pass_name(), Some("foreign opaque input"));
    assert_eq!(error.resource(), Some(foreign.into()));
    assert_eq!(error.resource_name(), None);
    assert_eq!(graph.passes.len(), before);

    let error = graph
        .raster_batch(
            "unresolved opaque input",
            [
                Ok((
                    vertices,
                    indices,
                    3,
                    ValidatedRasterParameters::new(RasterParameters::default()).unwrap(),
                )),
                Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "missing resolved mesh buffers",
                )),
            ]
            .into_iter(),
            color,
            depth,
            [0.0; 4],
            1.0,
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass_name(), Some("unresolved opaque input"));
    assert_eq!(error.pass(), None);
    assert_eq!(graph.passes.len(), before);
}

#[test]
fn public_raster_parameters_are_checked_before_creating_a_pass() {
    let mut graph = Graph::new();
    let (vertices, indices, color, depth) = mesh(&mut graph);
    let before = graph.passes.len();
    let mut transform = RasterParameters::default();
    transform.transform[0][0] = f32::INFINITY;
    let mut tint = RasterParameters::default();
    tint.tint[3] = f32::NAN;
    for parameters in [transform, tint] {
        let error = graph
            .raster(
                "invalid public parameters",
                RasterDraw {
                    vertices,
                    indices,
                    index_count: 3,
                    parameters,
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
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("invalid public parameters"));
        assert_eq!(error.pass(), None);
        assert_eq!(graph.passes.len(), before);
    }
}

#[test]
fn import_identity_survives_interleaved_transients_and_failed_aliases() {
    let mut graph = Graph::new();
    let sources = (0..64)
        .map(|index| ImportBuffer {
            key: ResourceKey {
                owner: 7,
                index,
                generation: 1,
            },
            revision: u64::from(index) + 1,
            desc: BufferDesc { size: 12 },
            initialized: std::iter::once(0..12).collect(),
        })
        .collect::<Vec<_>>();
    let mut handles = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        graph.image("transient", ImageDesc::rgba8(2, 2)).unwrap();
        handles.push(
            graph
                .import_buffer(format!("import {index}"), source)
                .unwrap(),
        );
    }
    for (source, &handle) in sources.iter().zip(&handles).rev() {
        assert_eq!(graph.import_buffer("alias", source).unwrap(), handle);
    }
    let first = &sources[0];
    let image_alias = ImportImage {
        key: first.key,
        revision: first.revision,
        desc: ImageDesc::rgba8(1, 3),
        initialized: true,
    };
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
