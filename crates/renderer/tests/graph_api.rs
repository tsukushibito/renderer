use renderer::graph::{
    Access, AccessKind, AccessPurpose, AccessRange, BufferDesc, ComputeTransform, Graph,
    GraphError, GraphErrorKind, ImageAspect, ImageDesc, ImageHandle, PassKind,
};
use renderer::graph::{
    Attachment, LoadOp, RasterAttachments, RasterDraw, RasterParameters, StoreOp,
};
use std::sync::Arc;

/// A consumer of the public API can build the whole offscreen pipeline without
/// enabling Vulkan, loading a native library, or constructing a window.
#[test]
fn offscreen_pipeline_builds_and_compiles_without_a_backend() {
    let mut graph = Graph::new();
    let vertices = graph
        .buffer("vertices", BufferDesc { size: 3 * 28 })
        .unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 3 * 4 }).unwrap();
    graph
        .upload_buffer_shared("upload vertices", vertices, 0, Arc::from(vec![0; 3 * 28]))
        .unwrap();
    graph
        .upload_buffer("upload indices", indices, 0, vec![0; 3 * 4])
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
            RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default(),
            },
            RasterAttachments {
                color: Attachment {
                    image: color,
                    load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
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
    graph
        .compute(
            "image transform",
            color,
            transformed,
            ComputeTransform::Invert,
        )
        .unwrap();
    graph
        .composite("composite", transformed, final_image)
        .unwrap();
    graph.export_image(final_image).unwrap();
    graph.readback_image("final readback", final_image).unwrap();
    let unused = graph.image("unused image", ImageDesc::rgba8(1, 1)).unwrap();
    graph
        .upload_image_shared("unused upload", unused, Arc::from([0; 4]))
        .unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(plan.pass_count(), 6);
    #[cfg(feature = "graph-diagnostics")]
    assert_eq!(plan.culled_pass_names(), ["unused upload"]);
    assert_eq!(
        plan.ordered_pass_names(),
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
        assert_eq!(plan.dump(), graph.compile().unwrap().dump());
        assert!(plan.dump().contains("purpose=Index"));
        assert!(plan.dump().contains("purpose=StorageRead"));
    }
}

#[test]
fn public_handles_preserve_graph_ownership() {
    let mut producer = Graph::new();
    let image = producer.image("local", ImageDesc::rgba8(1, 1)).unwrap();
    let mut unrelated = Graph::new();
    assert!(unrelated.readback_image("foreign read", image).is_err());
    assert!(unrelated.export_image(image).is_err());
}

#[test]
fn reusable_image_extension_discovers_each_input_description() {
    fn transform(graph: &mut Graph, input: ImageHandle) -> Result<ImageHandle, GraphError> {
        let output = graph.image("compatible output", graph.image_desc(input)?)?;
        graph.compute("transform", input, output, ComputeTransform::Invert)?;
        Ok(output)
    }
    for desc in [ImageDesc::rgba8(3, 2), ImageDesc::rgba8(11, 7)] {
        let mut graph = Graph::new();
        let input = graph.image("input", desc).unwrap();
        // Metadata is available before contents are initialized.
        assert_eq!(graph.image_desc(input).unwrap(), desc);
        graph
            .upload_image("pixels", input, vec![0; desc.byte_len().unwrap() as usize])
            .unwrap();
        let output = transform(&mut graph, input).unwrap();
        assert_eq!(graph.image_desc(output).unwrap(), desc);
        graph.export_image(output).unwrap();
        graph.compile().unwrap();

        let depth = graph.image("depth", ImageDesc::depth32(3, 2)).unwrap();
        assert_eq!(graph.image_desc(depth).unwrap(), ImageDesc::depth32(3, 2));
        let mut unrelated = Graph::new();
        unrelated.image("unrelated local name", desc).unwrap();
        let error = unrelated.image_desc(input).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
        assert_eq!(error.resource(), Some(input.into()));
        assert_eq!(error.resource_name(), None);
    }
}

#[test]
fn builder_errors_identify_the_attempt_without_creating_a_pass() {
    let mut graph = Graph::new();
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
        .declare_pass(
            "unsupported mip",
            PassKind::Compute,
            vec![Access {
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
    let mut graph = Graph::new();
    let buffer = graph
        .buffer("partially initialized", BufferDesc { size: 12 })
        .unwrap();
    graph
        .upload_buffer("prefix", buffer, 0, vec![0; 4])
        .unwrap();
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
        let mut graph = Graph::new();
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
    let mut graph = Graph::new();
    let tail = graph
        .declare_pass("same name", PassKind::Copy, vec![], true)
        .unwrap();
    let a = graph
        .declare_pass("same name", PassKind::Copy, vec![], false)
        .unwrap();
    let b = graph
        .declare_pass("same name", PassKind::Copy, vec![], false)
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
