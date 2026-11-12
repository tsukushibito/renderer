use renderer::engine::{Draw, Engine, IDENTITY, Material, MeshData, Vertex, View};

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
    let dump = plan.dump();
    assert!(
        dump.lines()
            .any(|line| line.contains("external custom transform") && line.contains("live"))
    );
    assert!(
        dump.lines()
            .any(|line| line.contains("unused custom branch") && line.contains("culled"))
    );
    assert!(frame.readback.is_some());
    assert!(engine.snapshot(&view, &[draw]).is_err());
    assert_eq!(engine.snapshot(&view, &[]).unwrap().draw_count(), 0);
}
