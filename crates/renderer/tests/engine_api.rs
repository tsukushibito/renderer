use renderer::engine::{
    Draw, Engine, FrameOutputOwnership, IDENTITY, Material, MeshData, Vertex, View,
};

#[test]
fn owned_frame_inputs_need_no_gpu_or_window() {
    let mut engine = Engine::new(2, 2).unwrap();
    let mesh = engine
        .create_mesh(
            MeshData::new(
                &[Vertex {
                    position: [0.0, 0.0, 0.5],
                    color: [1.0; 4],
                }],
                &[0, 0, 0],
            )
            .unwrap(),
        )
        .unwrap();
    let material = engine.create_material(Material::default()).unwrap();
    let view = View {
        width: 32,
        height: 24,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    };
    let draw = Draw {
        mesh,
        material,
        world_from_model: IDENTITY,
    };
    let captured = engine.snapshot(&view, &[draw]).unwrap();
    engine.delete_mesh(mesh).unwrap();
    engine.delete_material(material).unwrap();
    assert_eq!(captured.draw_count(), 1);
    assert_eq!(*captured.view(), view);
    let frame = captured
        .build_graph(true, |graph, color| {
            let desc = graph.image_desc(color)?;
            let transformed = graph.image("external custom output", desc)?;
            graph.compute(
                "external custom transform",
                color,
                transformed,
                renderer::graph::ComputeTransform::SwapRedBlue,
            )?;
            let unused = graph.image("unused custom output", desc)?;
            graph.compute(
                "unused custom branch",
                color,
                unused,
                renderer::graph::ComputeTransform::Invert,
            )?;
            Ok(transformed)
        })
        .unwrap();
    let plan = frame.graph.compile().unwrap();
    assert!(
        plan.ordered_pass_names()
            .contains(&"external custom transform")
    );
    assert!(!plan.ordered_pass_names().contains(&"unused custom branch"));
    #[cfg(feature = "graph-diagnostics")]
    assert!(plan.culled_pass_names().contains(&"unused custom branch"));
    assert!(frame.readback.is_some());
    assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
    assert!(engine.snapshot(&view, &[draw]).is_err());
    assert_eq!(engine.snapshot(&view, &[]).unwrap().draw_count(), 0);
}

#[test]
fn identity_extension_exports_its_input_without_a_copy() {
    let engine = Engine::new(1, 1).unwrap();
    let view = View {
        width: 17,
        height: 11,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    };
    let snapshot = engine.snapshot(&view, &[]).unwrap();
    let mut returned = None;
    let frame = snapshot
        .build_graph(false, |_, color| {
            returned = Some(color);
            Ok(color)
        })
        .unwrap();
    assert_eq!(frame.output, returned.unwrap());
    assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
    let plan = frame.graph.compile().unwrap();
    assert_eq!(plan.ordered_pass_names(), ["view clear"]);
}
