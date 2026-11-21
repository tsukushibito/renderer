#[path = "../fixtures/image_transform.rs"]
mod image_transform;
use image_transform::Transform;

use renderer::graph::{
    Access, AccessKind, AccessPurpose, AccessRange, BufferDesc, Graph, GraphCapacity,
    GraphCapacityKind, GraphError, GraphErrorKind, GraphPool, ImageAspect, ImageDesc, ImageHandle,
    PassKind,
};
use renderer::graph::{
    Attachment, LoadOp, RasterAttachments, RasterDraw, RasterParameters, StoreOp,
};

fn offscreen_pipeline(graph: &mut Graph) {
    let vertices = graph
        .buffer("vertices", BufferDesc { size: 3 * 28 })
        .unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 3 * 4 }).unwrap();
    graph
        .upload_buffer("upload vertices", vertices, 0, &[0; 3 * 28])
        .unwrap();
    graph
        .upload_buffer("upload indices", indices, 0, &[0; 3 * 4])
        .unwrap();
    let color = graph.image("mesh color", ImageDesc::rgba8(17, 11)).unwrap();
    let depth = graph
        .image("mesh depth", ImageDesc::depth32(17, 11))
        .unwrap();
    let transformed = graph
        .image("transformed", ImageDesc::rgba8(17, 11))
        .unwrap();
    let final_image = graph.image("final", ImageDesc::rgba8(17, 11)).unwrap();
    graph
        .raster(
            "mesh",
            RasterAttachments {
                color_resolves: &[],
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                    store: StoreOp::Store,
                }],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
            |raster| {
                raster.draw(
                    &renderer::program::ManagedRasterProgram::mesh(),
                    [RasterDraw {
                        bindings: None,
                        vertices,
                        indices,
                        first_index: 0,
                        index_count: 3,
                        instance_count: 1,
                        parameters: RasterParameters::default().into(),
                    }],
                )
            },
        )
        .unwrap();
    image_transform::dispatch(
        graph,
        "image transform",
        color,
        transformed,
        Transform::Invert,
    )
    .unwrap();
    graph
        .copy_image("composite", transformed, final_image)
        .unwrap();
    graph.export_image(final_image).unwrap();
    graph.readback_image("final readback", final_image).unwrap();
    let unused = graph.image("unused image", ImageDesc::rgba8(1, 1)).unwrap();
    graph
        .upload_image("unused upload", unused, &[0; 4])
        .unwrap();
}

#[test]
fn texture_upload_batch_can_retry_after_rejection_in_the_same_pool_slot() {
    let cap = GraphCapacity {
        resources: 2,
        passes: 2,
        accesses: 2,
        name_bytes: 4,
        upload_bytes: 8,
        ..Default::default()
    };
    let mut pool = GraphPool::new(cap, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let desc = ImageDesc::rgba8(1, 1);
    assert!(graph.upload_texture_for_export("x", desc, &[0; 3]).is_err());
    graph.upload_texture_for_export("a", desc, &[1; 4]).unwrap();
    graph.upload_texture_for_export("b", desc, &[2; 4]).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 2);
}

/// A consumer of the public API can build the whole offscreen pipeline without
/// enabling Vulkan, loading a native library, or constructing a window.
#[test]
fn offscreen_pipeline_builds_and_compiles_without_a_backend() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 2).unwrap();
    let mut graph = pool.begin().unwrap();
    offscreen_pipeline(&mut graph);
    let plan = graph.compile().unwrap();
    assert_eq!(plan.pass_count(), 6);
    #[cfg(feature = "graph-diagnostics")]
    assert_eq!(
        plan.culled_pass_names().collect::<Vec<_>>(),
        ["unused upload"]
    );
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        [
            "upload vertices",
            "upload indices",
            "mesh",
            "image transform",
            "composite",
            "final readback"
        ]
    );
    #[cfg(feature = "graph-diagnostics")]
    {
        let mut repeated = pool.begin().unwrap();
        offscreen_pipeline(&mut repeated);
        let repeated = repeated.compile().unwrap();
        assert_eq!(plan.dump(), repeated.dump());
        assert!(plan.dump().contains("purpose=Index"));
        assert!(plan.dump().contains("purpose=StorageRead"));
    }
}

#[test]
fn public_handles_preserve_graph_ownership() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 2).unwrap();
    let mut producer = pool.begin().unwrap();
    let image = producer.image("local", ImageDesc::rgba8(1, 1)).unwrap();
    let mut unrelated = pool.begin().unwrap();
    assert!(unrelated.readback_image("foreign read", image).is_err());
    assert!(unrelated.export_image(image).is_err());
}

#[test]
fn reusable_image_extension_discovers_each_input_description() {
    fn transform(graph: &mut Graph, input: ImageHandle) -> Result<ImageHandle, GraphError> {
        let output = graph.image("compatible output", graph.image_desc(input)?)?;
        image_transform::dispatch(graph, "transform", input, output, Transform::Invert)?;
        Ok(output)
    }
    let mut pool = GraphPool::new(GraphCapacity::default(), 2).unwrap();
    let pixels = [0; 11 * 7 * 4];
    for desc in [ImageDesc::rgba8(3, 2), ImageDesc::rgba8(11, 7)] {
        let mut graph = pool.begin().unwrap();
        let input = graph.image("input", desc).unwrap();
        // Metadata is available before contents are initialized.
        assert_eq!(graph.image_desc(input).unwrap(), desc);
        graph
            .upload_image(
                "pixels",
                input,
                &pixels[..desc.byte_len().unwrap() as usize],
            )
            .unwrap();
        let output = transform(&mut graph, input).unwrap();
        assert_eq!(graph.image_desc(output).unwrap(), desc);
        graph.export_image(output).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(3, 2)).unwrap();
        assert_eq!(graph.image_desc(depth).unwrap(), ImageDesc::depth32(3, 2));
        let _plan = graph.compile().unwrap();
        let mut unrelated = pool.begin().unwrap();
        unrelated.image("unrelated local name", desc).unwrap();
        let error = unrelated.image_desc(input).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
        assert_eq!(error.resource(), Some(input.into()));
        assert_eq!(error.resource_name(), None);
    }
}

#[test]
fn reusable_buffer_extension_discovers_size_and_rejects_recycled_handles() {
    fn copy(graph: &mut Graph, input: renderer::graph::BufferHandle) -> Result<(), GraphError> {
        let desc = graph.buffer_desc(input)?;
        let output = graph.buffer("compatible buffer", desc)?;
        graph.copy_buffer("copy full input", input, 0, output, 0, desc.size)?;
        graph.export_buffer(output)
    }

    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let desc = BufferDesc { size: 28 };
    let input = graph.buffer("input", desc).unwrap();
    // Description lookup is valid before initialization, just as for images.
    assert_eq!(graph.buffer_desc(input).unwrap(), desc);
    graph
        .upload_buffer("input bytes", input, 0, &[0; 28])
        .unwrap();
    copy(&mut graph, input).unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(plan.pass_count(), 2);
    drop(plan);

    let mut recycled = pool.begin().unwrap();
    recycled.buffer("new occupant", desc).unwrap();
    let error = recycled.buffer_desc(input).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    assert_eq!(error.resource(), Some(input.into()));
    assert_eq!(error.resource_name(), None);
}

#[test]
fn builder_errors_identify_the_attempt_without_creating_a_pass() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let buffer = graph.buffer("bytes", BufferDesc { size: 16 }).unwrap();
    let error = graph
        .readback_buffer("past end", buffer, 12, 8)
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.pass(), None);
    assert_eq!(error.pass_name(), Some("past end"));
    assert_eq!(error.resource(), Some(buffer.into()));
    assert_eq!(error.resource_name(), Some("bytes"));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(12..20)));

    let image = graph.image("rgba", ImageDesc::rgba8(2, 2)).unwrap();
    let range = AccessRange::Image {
        mip: 1,
        layer: 0,
        aspect: ImageAspect::Color,
    };
    let error = graph
        .declare_analysis_pass(
            "unsupported mip",
            PassKind::Compute,
            &[Access {
                resource: image.into(),
                range: range.clone(),
                kind: AccessKind::Write,
                purpose: AccessPurpose::StorageWrite,
            }],
            false,
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::Unsupported);
    assert_eq!(error.pass(), None);
    assert_eq!(error.pass_name(), Some("unsupported mip"));
    assert_eq!(error.resource(), Some(image.into()));
    assert_eq!(error.range(), Some(&range));
    assert_eq!(graph.compile().unwrap().pass_count(), 0);
}

#[test]
fn compile_errors_preserve_buffer_holes_and_image_export_context() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let buffer = graph
        .buffer("partially initialized", BufferDesc { size: 12 })
        .unwrap();
    graph.upload_buffer("prefix", buffer, 0, &[0; 4]).unwrap();
    let reader = graph.readback_buffer("read all", buffer, 0, 12).unwrap();
    let error = graph.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
    assert_eq!(error.pass(), Some(reader));
    assert_eq!(error.pass_name(), Some("read all"));
    assert_eq!(error.resource(), Some(buffer.into()));
    assert_eq!(error.resource_name(), Some("partially initialized"));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(4..12)));
    assert!(error.to_string().contains("partially initialized"));

    for (desc, aspect) in [
        (ImageDesc::rgba8(2, 2), ImageAspect::Color),
        (ImageDesc::depth32(2, 2), ImageAspect::Depth),
    ] {
        let mut graph = pool.begin().unwrap();
        let image = graph.image("undefined export", desc).unwrap();
        graph.export_image(image).unwrap();
        let error = graph.compile().unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
        assert_eq!(error.pass(), None);
        assert_eq!(error.pass_name(), None);
        assert_eq!(error.resource(), Some(image.into()));
        assert_eq!(error.resource_name(), Some("undefined export"));
        assert_eq!(
            error.range(),
            Some(&AccessRange::Image {
                mip: 0,
                layer: 0,
                aspect
            })
        );
    }
}

#[test]
fn cycle_witness_excludes_blocked_tail_and_distinguishes_duplicate_names() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let tail = graph
        .declare_analysis_pass("same name", PassKind::Copy, &[], true)
        .unwrap();
    let a = graph
        .declare_analysis_pass("same name", PassKind::Copy, &[], false)
        .unwrap();
    let b = graph
        .declare_analysis_pass("same name", PassKind::Copy, &[], false)
        .unwrap();
    graph.depends_on(tail, a).unwrap();
    graph.depends_on(a, b).unwrap();
    graph.depends_on(b, a).unwrap();
    let error = graph.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::DependencyCycle);
    let cycle = error.cycle_passes();
    assert_eq!(cycle.len(), 3);
    assert_eq!(cycle.first(), cycle.last());
    assert!(cycle.contains(&a) && cycle.contains(&b));
    assert!(!cycle.contains(&tail));
    assert_eq!(error.pass(), cycle.first().copied());
    assert_eq!(error.pass_name(), Some("same name"));
    assert_eq!(error.resource(), None);
}

#[test]
fn application_errors_have_an_explicit_category() {
    let error = GraphError::custom("extension rejected");
    assert_eq!(error.kind(), GraphErrorKind::Custom);
    assert_eq!(error.to_string(), "extension rejected");
    assert_eq!(error.pass(), None);
    assert_eq!(error.resource(), None);
    assert_eq!(error.range(), None);
    assert!(error.cycle_passes().is_empty());
}

#[test]
fn interleaved_dependencies_preserve_order_and_capacity_failure_is_transactional() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            dependencies: 4,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let tail = graph
        .declare_analysis_pass("tail", PassKind::Copy, &[], true)
        .unwrap();
    let first = graph
        .declare_analysis_pass("first", PassKind::Copy, &[], false)
        .unwrap();
    let middle = graph
        .declare_analysis_pass("middle", PassKind::Copy, &[], false)
        .unwrap();
    let last = graph
        .declare_analysis_pass("last", PassKind::Copy, &[], false)
        .unwrap();
    graph.depends_on(tail, middle).unwrap();
    graph.depends_on(last, first).unwrap();
    graph.depends_on(tail, last).unwrap();
    graph.depends_on(middle, first).unwrap();
    // Duplicate edges need no additional storage, even at capacity.
    graph.depends_on(tail, middle).unwrap();
    let error = graph.depends_on(first, tail).unwrap_err();
    let capacity = error.capacity().unwrap();
    assert_eq!(capacity.kind, GraphCapacityKind::Dependencies);
    assert_eq!((capacity.limit, capacity.required), (4, Some(5)));
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["first", "middle", "last", "tail"]
    );
    // Reusing the same prepared slot must not retain old dependency nodes.
    let graph = pool.begin().unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 0);
}

#[test]
fn capacity_errors_identify_builder_limits_and_return_slots() {
    let defaults = GraphCapacity::default();
    for kind in [
        GraphCapacityKind::Resources,
        GraphCapacityKind::Passes,
        GraphCapacityKind::Accesses,
        GraphCapacityKind::NameBytes,
        GraphCapacityKind::UploadBytes,
    ] {
        let mut capacity = defaults;
        match kind {
            GraphCapacityKind::Resources => capacity.resources = 0,
            GraphCapacityKind::Passes => capacity.passes = 0,
            GraphCapacityKind::Accesses => capacity.accesses = 0,
            GraphCapacityKind::NameBytes => capacity.name_bytes = 0,
            GraphCapacityKind::UploadBytes => capacity.upload_bytes = 0,
            _ => unreachable!(),
        }
        let mut pool = GraphPool::new(capacity, 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let error = match kind {
            GraphCapacityKind::Resources | GraphCapacityKind::NameBytes => {
                graph.buffer("x", BufferDesc { size: 4 }).unwrap_err()
            }
            GraphCapacityKind::Passes => graph
                .declare_analysis_pass("p", PassKind::Copy, &[], true)
                .unwrap_err(),
            _ => {
                let buffer = graph.buffer("b", BufferDesc { size: 4 }).unwrap();
                graph
                    .upload_buffer("upload", buffer, 0, &[1; 4])
                    .unwrap_err()
            }
        };
        assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
        let details = error.capacity().unwrap();
        assert_eq!(details.kind, kind);
        assert_eq!(details.limit, 0);
        assert!(details.required.is_some_and(|count| count > 0));
        assert!(matches!(error.message, std::borrow::Cow::Borrowed(_)));
        drop(graph);
        assert!(pool.is_idle());
    }
    let mut pool = GraphPool::new(defaults, 1).unwrap();
    let plan = pool.begin().unwrap().compile().unwrap();
    let details = pool.begin().unwrap_err().capacity().unwrap();
    assert_eq!(details.kind, GraphCapacityKind::Slots);
    assert_eq!((details.limit, details.required), (1, Some(2)));
    drop(plan);
    assert!(pool.begin().is_ok());
}

#[test]
fn compile_capacity_errors_preserve_analysis_dimension() {
    for kind in [
        GraphCapacityKind::Edges,
        GraphCapacityKind::Segments,
        GraphCapacityKind::Readers,
    ] {
        let mut capacity = GraphCapacity::default();
        match kind {
            GraphCapacityKind::Edges => capacity.edges = 0,
            GraphCapacityKind::Segments => capacity.segments = 0,
            GraphCapacityKind::Readers => capacity.readers = 0,
            _ => unreachable!(),
        }
        let mut pool = GraphPool::new(capacity, 1).unwrap();
        let mut graph = pool.begin().unwrap();
        if kind == GraphCapacityKind::Edges {
            let a = graph
                .declare_analysis_pass("a", PassKind::Copy, &[], false)
                .unwrap();
            let b = graph
                .declare_analysis_pass("b", PassKind::Copy, &[], true)
                .unwrap();
            graph.depends_on(b, a).unwrap();
        } else {
            let buffer = graph.buffer("b", BufferDesc { size: 4 }).unwrap();
            if kind == GraphCapacityKind::Readers {
                graph.upload_buffer("upload", buffer, 0, &[1; 4]).unwrap();
                graph.readback_buffer("read", buffer, 0, 4).unwrap();
            }
        }
        let error = graph.compile().unwrap_err();
        let details = error.capacity().unwrap();
        assert_eq!(details.kind, kind);
        assert_eq!(details.limit, 0);
        assert_eq!(details.required, None);
        assert!(pool.is_idle());
    }
}
