//! Real engine-input contracts, including pending versions and external passes.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::{RuntimeConfig, VulkanRuntime};
use crate::{
    EngineRenderError, FrameId, VulkanEngine,
    engine::{Draw, IDENTITY, Material, MeshData, Vertex, View},
    graph::{ComputeTransform, Graph, GraphError, ImageDesc, PassHandle},
};

fn mesh(color: [f32; 4]) -> MeshData {
    MeshData::new(
        &[[-0.4, -0.4, 0.25], [0.4, -0.4, 0.25], [0.0, 0.4, 0.25]]
            .map(|position| Vertex { position, color }),
        &[0, 1, 2],
    )
    .unwrap()
}

fn with_engine(test: impl FnOnce(&mut VulkanEngine)) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 4,
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert_ne!(
            runtime.owner.device.device_type(),
            ash::vk::PhysicalDeviceType::CPU
        );
        let diagnostics = Arc::clone(&runtime.diagnostics);
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        let mut engine = VulkanEngine::new(runtime, 4, 4).unwrap();
        test(&mut engine);
        assert!(
            engine
                .runtime_mut()
                .drain_frames(Duration::from_secs(10))
                .unwrap()
        );
        drop(engine);
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "Core/SyncVal through engine shutdown: {report:#?}"
        );
    });
}

fn collect(engine: &mut VulkanEngine, expected: &[(FrameId, PassHandle, usize, [u8; 4])]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut completed = Vec::new();
    while completed.len() != expected.len() {
        if let Some(mut completion) = engine.runtime_mut().poll_frame().unwrap() {
            let &(_, pass, pixel, color) = expected
                .iter()
                .find(|(id, ..)| *id == completion.id())
                .unwrap();
            let bytes = completion.result.readback(pass).unwrap();
            assert_eq!(&bytes[pixel * 4..pixel * 4 + 4], &color);
            assert_eq!(&bytes[..4], &[0, 0, 0, 255]);
            completed.push(completion.id());
        } else {
            assert!(Instant::now() < deadline, "engine GPU completion timed out");
            std::thread::yield_now();
        }
    }
}

#[test]
#[ignore = "explicit real Vulkan engine input/lifetime test; requires Core+SyncVal"]
fn vulkan_engine_owned_inputs_transform_and_survive_pending_updates() {
    with_engine(|engine| {
        let mesh_handle = engine.create_mesh(mesh([1.0; 4])).unwrap();
        let material = engine
            .create_material(Material {
                tint: [1.0, 0.0, 0.0, 1.0],
            })
            .unwrap();
        let mut view = View {
            width: 32,
            height: 32,
            clip_from_world: IDENTITY,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        };
        let mut draw = Draw {
            mesh: mesh_handle,
            material,
            world_from_model: IDENTITY,
        };
        let first = engine
            .render_frame(&view, &[draw], true, |_, color| Ok(color))
            .unwrap();
        engine.runtime_mut().delete(first.output.key()).unwrap();
        let mut expected = vec![(
            first.submission.id(),
            first.readback.unwrap(),
            16 * 32 + 16,
            [255, 0, 0, 255],
        )];
        engine
            .update_material(
                material,
                Material {
                    tint: [0.0, 1.0, 0.0, 1.0],
                },
            )
            .unwrap();
        draw.world_from_model[3][0] = -0.75;
        let shifted = engine
            .render_frame(&view, &[draw], true, |_, color| Ok(color))
            .unwrap();
        engine.runtime_mut().delete(shifted.output.key()).unwrap();
        expected.push((
            shifted.submission.id(),
            shifted.readback.unwrap(),
            16 * 32 + 4,
            [0, 255, 0, 255],
        ));
        view.clip_from_world[3][0] = 0.75;
        engine
            .update_material(
                material,
                Material {
                    tint: [0.0, 0.0, 1.0, 1.0],
                },
            )
            .unwrap();
        let camera = engine
            .render_frame(&view, &[draw], true, |_, color| Ok(color))
            .unwrap();
        engine.runtime_mut().delete(camera.output.key()).unwrap();
        expected.push((
            camera.submission.id(),
            camera.readback.unwrap(),
            16 * 32 + 16,
            [0, 0, 255, 255],
        ));
        // Replace vertex content while all three accepted frames are owned by
        // the runtime. The old mesh buffers remain retained by their work.
        engine
            .update_mesh(mesh_handle, mesh([1.0, 0.0, 1.0, 1.0]))
            .unwrap();
        engine
            .update_material(material, Material::default())
            .unwrap();
        let replaced = engine
            .render_frame(&view, &[draw], true, |_, color| Ok(color))
            .unwrap();
        engine.runtime_mut().delete(replaced.output.key()).unwrap();
        expected.push((
            replaced.submission.id(),
            replaced.readback.unwrap(),
            16 * 32 + 16,
            [255, 0, 255, 255],
        ));
        engine.delete_mesh(mesh_handle).unwrap();
        engine.delete_material(material).unwrap();
        assert_eq!(engine.runtime().pending_frames(), 4);
        collect(engine, &expected);
        assert!(engine.snapshot(&view, &[draw]).is_err());
    });
}

#[test]
#[ignore = "explicit real Vulkan engine Graph extension/cancellation test; requires Core+SyncVal"]
fn vulkan_engine_custom_empty_view_and_prepared_frame_failures() {
    with_engine(|engine| {
        let view = View {
            width: 8,
            height: 8,
            clip_from_world: IDENTITY,
            clear_color: [0.0, 0.0, 1.0, 1.0],
        };
        let frame = engine
            .render_frame(&view, &[], true, |graph, color| {
                let target = graph.image("external custom invert", ImageDesc::rgba8(8, 8))?;
                graph.compute(
                    "external inversion",
                    color,
                    target,
                    ComputeTransform::Invert,
                )?;
                Ok(target)
            })
            .unwrap();
        engine.runtime_mut().delete(frame.output.key()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(mut complete) = engine.runtime_mut().poll_frame().unwrap() {
                let bytes = complete.result.readback(frame.readback.unwrap()).unwrap();
                assert_eq!(bytes, [255, 255, 0, 255].repeat(64));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let before = engine.runtime().stats().registered_resources;
        let rejected = engine.build_frame(&view, &[], false, |_, _| {
            Err(GraphError::custom("extension rejected"))
        });
        assert!(matches!(rejected, Err(EngineRenderError::Graph(_))));
        let mut prepared = engine
            .build_frame(&view, &[], false, |_, color| Ok(color))
            .unwrap();
        prepared.graph = Graph::new();
        let foreign = prepared.graph.compile().unwrap();
        assert!(matches!(
            engine.submit_frame(prepared, &foreign),
            Err(EngineRenderError::Input(_))
        ));
        assert_eq!(engine.runtime().pending_frames(), 0);
        assert_eq!(engine.runtime().stats().registered_resources, before);

        let black_view = View {
            clear_color: [0.0, 0.0, 0.0, 1.0],
            ..view
        };
        let handle = engine.create_mesh(mesh([1.0; 4])).unwrap();
        let material = engine.create_material(Material::default()).unwrap();
        let draw = Draw {
            mesh: handle,
            material,
            world_from_model: IDENTITY,
        };
        let captured = engine.snapshot(&black_view, &[draw]).unwrap();
        let old_a = engine
            .build_snapshot(&captured, true, |_, c| Ok(c))
            .unwrap();
        let old_b = engine
            .build_snapshot(&captured, true, |_, c| Ok(c))
            .unwrap();
        let old_a_plan = old_a.graph.compile().unwrap();
        let old_b_plan = old_b.graph.compile().unwrap();
        // Both prepared graphs own uploads of the old version. Updating CPU
        // assets before acceptance must not publish those exports as current.
        engine
            .update_mesh(handle, mesh([0.0, 0.0, 1.0, 1.0]))
            .unwrap();
        let a = engine.submit_frame(old_a, &old_a_plan).unwrap();
        let b = engine.submit_frame(old_b, &old_b_plan).unwrap();
        engine.runtime_mut().delete(a.output.key()).unwrap();
        engine.runtime_mut().delete(b.output.key()).unwrap();
        assert_eq!(engine.runtime().stats().registered_resources, 0);
        let new_a = engine
            .build_frame(&black_view, &[draw], true, |_, c| Ok(c))
            .unwrap();
        let new_b = engine
            .build_frame(&black_view, &[draw], true, |_, c| Ok(c))
            .unwrap();
        let plan_a = new_a.graph.compile().unwrap();
        let plan_b = new_b.graph.compile().unwrap();
        let c = engine.submit_frame(new_a, &plan_a).unwrap();
        let d = engine.submit_frame(new_b, &plan_b).unwrap();
        engine.runtime_mut().delete(c.output.key()).unwrap();
        engine.runtime_mut().delete(d.output.key()).unwrap();
        // Two independently prepared uploads of one version replace the cache
        // without leaving the first pair registered forever.
        assert_eq!(engine.runtime().stats().registered_resources, 2);
        collect(
            engine,
            &[
                (a.submission.id(), a.readback.unwrap(), 4 * 8 + 4, [255; 4]),
                (b.submission.id(), b.readback.unwrap(), 4 * 8 + 4, [255; 4]),
                (
                    c.submission.id(),
                    c.readback.unwrap(),
                    4 * 8 + 4,
                    [0, 0, 255, 255],
                ),
                (
                    d.submission.id(),
                    d.readback.unwrap(),
                    4 * 8 + 4,
                    [0, 0, 255, 255],
                ),
            ],
        );
        let cached = engine
            .build_frame(&black_view, &[draw], true, |_, c| Ok(c))
            .unwrap();
        let plan = cached.graph.compile().unwrap();
        assert!(!plan.dump().contains("mesh vertex upload"));
        let pending = engine.submit_frame(cached, &plan).unwrap();
        engine.runtime_mut().delete(pending.output.key()).unwrap();
        engine.delete_mesh(handle).unwrap();
        engine.delete_material(material).unwrap();
        collect(
            engine,
            &[(
                pending.submission.id(),
                pending.readback.unwrap(),
                4 * 8 + 4,
                [0, 0, 255, 255],
            )],
        );
        let old = engine
            .render_snapshot(&captured, true, |_, c| Ok(c))
            .unwrap();
        engine.runtime_mut().delete(old.output.key()).unwrap();
        assert_eq!(engine.runtime().stats().registered_resources, 0);
        collect(
            engine,
            &[(
                old.submission.id(),
                old.readback.unwrap(),
                4 * 8 + 4,
                [255; 4],
            )],
        );
    });
}
