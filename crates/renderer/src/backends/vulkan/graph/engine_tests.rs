//! Real engine-input contracts, including pending versions and external passes.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::{RuntimeConfig, VulkanRuntime};
use crate::{
    EngineRenderError, FrameId, MeshCacheConfig, MeshUploadState, PreparedEngineFrame,
    RenderedFrame, VulkanEngine,
    engine::{Draw, Engine, FrameSnapshot, IDENTITY, Material, MeshData, Vertex, View},
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

fn with_engine(test: impl FnOnce(&mut VulkanEngine, &mut Engine)) {
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
        let mut engine = VulkanEngine::new(
            runtime,
            MeshCacheConfig {
                max_meshes: 8,
                max_bytes: mesh([1.0; 4]).byte_len() * 8,
                max_upload_submissions: 1,
            },
        )
        .unwrap();
        let mut assets = Engine::new(4, 4).unwrap();
        test(&mut engine, &mut assets);
        engine.unload_all_meshes().unwrap();
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

fn prepare_meshes(engine: &mut VulkanEngine, snapshot: &FrameSnapshot) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut progress = engine.prepare_snapshot(snapshot).unwrap();
    while progress.state != MeshUploadState::Ready || progress.pending_submissions != 0 {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("explicit mesh upload preparation timed out");
        progress = engine.wait_uploads(remaining).unwrap();
    }
}

fn build_frame(
    engine: &mut VulkanEngine,
    assets: &Engine,
    view: &View,
    draws: &[Draw],
    readback: bool,
    extension: impl FnOnce(
        &mut Graph,
        crate::graph::ImageHandle,
    ) -> Result<crate::graph::ImageHandle, GraphError>,
) -> Result<PreparedEngineFrame, EngineRenderError> {
    let snapshot = assets.snapshot(view, draws)?;
    prepare_meshes(engine, &snapshot);
    engine.build_snapshot(&snapshot, readback, extension)
}

fn render_frame(
    engine: &mut VulkanEngine,
    assets: &Engine,
    view: &View,
    draws: &[Draw],
    readback: bool,
    extension: impl FnOnce(
        &mut Graph,
        crate::graph::ImageHandle,
    ) -> Result<crate::graph::ImageHandle, GraphError>,
) -> Result<RenderedFrame, EngineRenderError> {
    let prepared = build_frame(engine, assets, view, draws, readback, extension)?;
    let plan = prepared.graph.compile()?;
    engine.submit_frame(prepared, &plan)
}

fn collect(engine: &mut VulkanEngine, expected: &[(FrameId, PassHandle, usize, [u8; 4])]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut completed = Vec::new();
    while completed.len() != expected.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("engine GPU completion timed out");
        let mut completion = engine
            .runtime_mut()
            .wait_frame(remaining)
            .unwrap()
            .expect("engine GPU completion timed out");
        let &(_, pass, pixel, color) = expected
            .iter()
            .find(|(id, ..)| *id == completion.id())
            .unwrap();
        let bytes = completion.result.readback(pass).unwrap();
        assert_eq!(&bytes[pixel * 4..pixel * 4 + 4], &color);
        assert_eq!(&bytes[..4], &[0, 0, 0, 255]);
        completed.push(completion.id());
    }
}

#[test]
#[ignore = "explicit real Vulkan engine input/lifetime test; requires Core+SyncVal"]
fn vulkan_engine_owned_inputs_transform_and_survive_pending_updates() {
    with_engine(|engine, assets| {
        let mesh_handle = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let material = assets
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
        let mut first =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut first).unwrap();
        let mut expected = vec![(
            first.submission.id(),
            first.readback.unwrap(),
            16 * 32 + 16,
            [255, 0, 0, 255],
        )];
        assets
            .update_material(
                material,
                Material {
                    tint: [0.0, 1.0, 0.0, 1.0],
                },
            )
            .unwrap();
        draw.world_from_model[3][0] = -0.75;
        let mut shifted =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut shifted).unwrap();
        expected.push((
            shifted.submission.id(),
            shifted.readback.unwrap(),
            16 * 32 + 4,
            [0, 255, 0, 255],
        ));
        view.clip_from_world[3][0] = 0.75;
        assets
            .update_material(
                material,
                Material {
                    tint: [0.0, 0.0, 1.0, 1.0],
                },
            )
            .unwrap();
        let mut camera =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut camera).unwrap();
        expected.push((
            camera.submission.id(),
            camera.readback.unwrap(),
            16 * 32 + 16,
            [0, 0, 255, 255],
        ));
        // Replace vertex content while all three accepted frames are owned by
        // the runtime. The old mesh buffers remain retained by their work.
        assets
            .update_mesh(mesh_handle, mesh([1.0, 0.0, 1.0, 1.0]))
            .unwrap();
        assets
            .update_material(material, Material::default())
            .unwrap();
        let mut replaced =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut replaced).unwrap();
        expected.push((
            replaced.submission.id(),
            replaced.readback.unwrap(),
            16 * 32 + 16,
            [255, 0, 255, 255],
        ));
        assets.delete_mesh(mesh_handle).unwrap();
        engine.unload_mesh(mesh_handle).unwrap();
        assets.delete_material(material).unwrap();
        assert_eq!(engine.runtime().pending_frames(), 4);
        collect(engine, &expected);
        assert!(assets.snapshot(&view, &[draw]).is_err());
    });
}

#[test]
#[ignore = "real-GPU opaque draw-list ownership through pending mesh/material deletion; requires Core/SyncVal"]
fn vulkan_engine_opaque_batch_keeps_pending_draws_and_each_mesh_alive() {
    with_engine(|engine, assets| {
        let near = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let far_vertices =
            [[-0.4, -0.4, 0.75], [0.4, -0.4, 0.75], [0.0, 0.4, 0.75]].map(|position| Vertex {
                position,
                color: [1.0; 4],
            });
        let farther = MeshData::new(&far_vertices, &[0, 1, 2]).unwrap();
        let far = assets.create_mesh(farther).unwrap();
        let equal = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let red = assets
            .create_material(crate::engine::Material {
                tint: [1.0, 0.0, 0.0, 1.0],
            })
            .unwrap();
        let green = assets
            .create_material(crate::engine::Material {
                tint: [0.0, 1.0, 0.0, 1.0],
            })
            .unwrap();
        let blue = assets
            .create_material(crate::engine::Material {
                tint: [0.0, 0.0, 1.0, 1.0],
            })
            .unwrap();
        let draws = [
            Draw {
                mesh: near,
                material: red,
                world_from_model: IDENTITY,
            },
            Draw {
                mesh: far,
                material: green,
                world_from_model: IDENTITY,
            },
            Draw {
                mesh: equal,
                material: blue,
                world_from_model: IDENTITY,
            },
        ]
        .repeat(32);
        let view = View {
            width: 32,
            height: 32,
            clip_from_world: IDENTITY,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        };
        let extension = |graph: &mut Graph, color| {
            let output = graph.image("custom swap", graph.image_desc(color)?)?;
            graph.compute(
                "swap after opaque",
                color,
                output,
                ComputeTransform::SwapRedBlue,
            )?;
            Ok(output)
        };
        let mut first = render_frame(engine, assets, &view, &draws, true, extension).unwrap();
        engine.release_output(&mut first).unwrap();
        assets
            .update_material(
                red,
                crate::engine::Material {
                    tint: [0.0, 1.0, 0.0, 1.0],
                },
            )
            .unwrap();
        assets.update_mesh(near, mesh([1.0; 4])).unwrap();
        let mut second = render_frame(engine, assets, &view, &draws, true, extension).unwrap();
        engine.release_output(&mut second).unwrap();
        for handle in [near, far, equal] {
            assets.delete_mesh(handle).unwrap();
            engine.unload_mesh(handle).unwrap();
        }
        for handle in [red, green, blue] {
            assets.delete_material(handle).unwrap();
        }
        assert_eq!(engine.runtime().pending_frames(), 2);
        collect(
            engine,
            &[
                (
                    first.submission.id(),
                    first.readback.unwrap(),
                    16 * 32 + 16,
                    [0, 0, 255, 255],
                ),
                (
                    second.submission.id(),
                    second.readback.unwrap(),
                    16 * 32 + 16,
                    [0, 255, 0, 255],
                ),
            ],
        );
    });
}

#[test]
#[ignore = "explicit real Vulkan engine Graph extension/cancellation test; requires Core+SyncVal"]
fn vulkan_engine_custom_empty_view_and_prepared_frame_failures() {
    with_engine(|engine, assets| {
        let view = View {
            width: 8,
            height: 8,
            clip_from_world: IDENTITY,
            clear_color: [0.0, 0.0, 1.0, 1.0],
        };
        let mut frame = render_frame(engine, assets, &view, &[], true, |graph, color| {
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
        engine.release_output(&mut frame).unwrap();
        let mut complete = engine
            .runtime_mut()
            .wait_frame(Duration::from_secs(10))
            .unwrap()
            .expect("engine GPU completion timed out");
        let bytes = complete.result.readback(frame.readback.unwrap()).unwrap();
        assert_eq!(bytes, [255, 255, 0, 255].repeat(64));
        let before = engine.runtime().stats().registered_resources;
        let rejected = build_frame(engine, assets, &view, &[], false, |_, _| {
            Err(GraphError::custom("extension rejected"))
        });
        assert!(matches!(rejected, Err(EngineRenderError::Graph(_))));
        let mut prepared =
            build_frame(engine, assets, &view, &[], false, |_, color| Ok(color)).unwrap();
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
        let handle = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let material = assets.create_material(Material::default()).unwrap();
        let draw = Draw {
            mesh: handle,
            material,
            world_from_model: IDENTITY,
        };
        let captured = assets.snapshot(&black_view, &[draw]).unwrap();
        prepare_meshes(engine, &captured);
        let old_a = engine
            .build_snapshot(&captured, true, |_, c| Ok(c))
            .unwrap();
        let old_b = engine
            .build_snapshot(&captured, true, |_, c| Ok(c))
            .unwrap();
        let old_a_plan = old_a.graph.compile().unwrap();
        let old_b_plan = old_b.graph.compile().unwrap();
        // Both prepared graphs pin the same resident old version. Later CPU
        // updates do not alter their immutable content or registration pins.
        assets
            .update_mesh(handle, mesh([0.0, 0.0, 1.0, 1.0]))
            .unwrap();
        let mut a = engine.submit_frame(old_a, &old_a_plan).unwrap();
        let mut b = engine.submit_frame(old_b, &old_b_plan).unwrap();
        engine.release_output(&mut a).unwrap();
        engine.release_output(&mut b).unwrap();
        assert_eq!(engine.runtime().stats().registered_resources, 2);
        let new_a = build_frame(engine, assets, &black_view, &[draw], true, |_, c| Ok(c)).unwrap();
        let new_b = build_frame(engine, assets, &black_view, &[draw], true, |_, c| Ok(c)).unwrap();
        let plan_a = new_a.graph.compile().unwrap();
        let plan_b = new_b.graph.compile().unwrap();
        let mut c = engine.submit_frame(new_a, &plan_a).unwrap();
        let mut d = engine.submit_frame(new_b, &plan_b).unwrap();
        engine.release_output(&mut c).unwrap();
        engine.release_output(&mut d).unwrap();
        // Two views share each immutable resident version: the old and new
        // content each have one buffer pair, independent of CPU asset lifetime.
        assert_eq!(engine.runtime().stats().registered_resources, 4);
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
        let cached = build_frame(engine, assets, &black_view, &[draw], true, |_, c| Ok(c)).unwrap();
        let plan = cached.graph.compile().unwrap();
        assert!(!plan.dump().contains("mesh vertex upload"));
        let mut pending = engine.submit_frame(cached, &plan).unwrap();
        engine.release_output(&mut pending).unwrap();
        assets.delete_mesh(handle).unwrap();
        engine.unload_mesh(handle).unwrap();
        assets.delete_material(material).unwrap();
        collect(
            engine,
            &[(
                pending.submission.id(),
                pending.readback.unwrap(),
                4 * 8 + 4,
                [0, 0, 255, 255],
            )],
        );
        prepare_meshes(engine, &captured);
        let mut old = engine
            .render_snapshot(&captured, true, |_, c| Ok(c))
            .unwrap();
        engine.release_output(&mut old).unwrap();
        assert_eq!(engine.runtime().stats().registered_resources, 2);
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
