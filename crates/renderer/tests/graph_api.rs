use renderer::graph::{AttachmentOps, BufferDesc, ComputeTransform, Graph, ImageDesc};

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
        .upload_buffer("upload vertices", vertices, 0, vec![0; 3 * 28])
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
            vertices,
            indices,
            3,
            color,
            depth,
            AttachmentOps::CLEAR_STORE,
            AttachmentOps::CLEAR_STORE,
            [0.0, 0.0, 0.0, 1.0],
            1.0,
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
        .upload_image("unused upload", unused, vec![0; 4])
        .unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(plan.pass_count(), 6);
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
    assert_eq!(plan.dump(), graph.compile().unwrap().dump());
}

#[test]
fn public_handles_preserve_graph_ownership() {
    let mut producer = Graph::new();
    let image = producer.image("local", ImageDesc::rgba8(1, 1)).unwrap();
    let mut unrelated = Graph::new();
    assert!(unrelated.readback_image("foreign read", image).is_err());
    assert!(unrelated.export_image(image).is_err());
}
