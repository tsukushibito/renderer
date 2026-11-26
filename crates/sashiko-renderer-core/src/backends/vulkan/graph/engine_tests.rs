//! Real engine-input contracts, including pending versions and external passes.

use crate::geometry::test_mesh::{Vertex, mesh_data};
use crate::image_transform::{self, Transform};
use std::time::{Duration, Instant};

use super::{FrameCapacity, ResourceClass, RuntimeConfig, VulkanRuntime};
use crate::backends::vulkan::engine::{
    EngineFrameCapacity,
    test_input::{Assets, Draw, FrameSnapshot, View, build_snapshot, graph, render_snapshot},
};
use crate::{
    geometry::GeometryData,
    graph::{BufferDesc, Graph, GraphError, ImageDesc, PassHandle},
    vulkan::{
        EngineRenderError, FrameId, MeshCacheConfig, MeshUploadState, PreparedEngineFrame,
        RenderedFrame, VulkanEngine,
    },
};

fn mesh(color: [f32; 4]) -> GeometryData {
    mesh_data(
        &[[-0.4, -0.4, 0.25], [0.4, -0.4, 0.25], [0.0, 0.4, 0.25]]
            .map(|position| Vertex { position, color }),
        &[0, 1, 2],
    )
    .unwrap()
}

fn with_engine(test: impl FnOnce(&mut VulkanEngine, &mut Assets)) {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut runtime = VulkanRuntime::new(RuntimeConfig {
            execution_slots: 4,
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert_ne!(
            runtime.owner.device.device_type(),
            ash::vk::PhysicalDeviceType::CPU
        );
        let diagnostics = runtime.diagnostics.clone();
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        runtime
            .prepare_working_set(
                FrameCapacity::default(),
                &[
                    ResourceClass::Buffer {
                        desc: BufferDesc { size: 84 },
                        count: 8,
                    },
                    ResourceClass::Buffer {
                        desc: BufferDesc { size: 12 },
                        count: 8,
                    },
                    ResourceClass::Image {
                        desc: ImageDesc::rgba8(32, 32),
                        count: 12,
                    },
                    ResourceClass::Image {
                        desc: ImageDesc::depth32(32, 32),
                        count: 4,
                    },
                    ResourceClass::Image {
                        desc: ImageDesc::rgba8(8, 8),
                        count: 8,
                    },
                    ResourceClass::Image {
                        desc: ImageDesc::depth32(8, 8),
                        count: 4,
                    },
                ],
            )
            .unwrap();
        runtime
            .prepare_compute_programs(&[
                image_transform::program(Transform::Invert),
                image_transform::program(Transform::SwapRedBlue),
            ])
            .unwrap();
        let mut engine = VulkanEngine::new(
            runtime,
            MeshCacheConfig {
                max_meshes: 8,
                max_bytes: mesh([1.0; 4]).byte_len() * 8,
                max_upload_submissions: 1,
                max_retired_sources: 16,
            },
            EngineFrameCapacity::default(),
        )
        .unwrap();
        let mut assets = Assets::new(4).unwrap();
        test(&mut engine, &mut assets);
        engine.unload_all_meshes();
        assert!(
            engine
                .runtime_mut()
                .drain_frames(Duration::from_secs(10))
                .unwrap()
        );
        engine.collect_retired_meshes(Some(&assets)).unwrap();
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
    let mut progress = engine.prepare_snapshot(snapshot, 2).unwrap();
    while progress.state != MeshUploadState::Ready || progress.pending_submissions != 0 {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("explicit mesh upload preparation timed out");
        progress = engine.wait_uploads(remaining).unwrap();
    }
}

fn build_frame(
    engine: &mut VulkanEngine,
    assets: &Assets,
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
    build_snapshot(engine, &snapshot, readback, extension)
}

fn render_frame(
    engine: &mut VulkanEngine,
    assets: &Assets,
    view: &View,
    draws: &[Draw],
    readback: bool,
    extension: impl FnOnce(
        &mut Graph,
        crate::graph::ImageHandle,
    ) -> Result<crate::graph::ImageHandle, GraphError>,
) -> Result<RenderedFrame, EngineRenderError> {
    let prepared = build_frame(engine, assets, view, draws, readback, extension)?;
    engine.submit_frame(prepared.compile()?)
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
        let bytes = completion.result.test_readback(pass).unwrap();
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
        let tint = [1.0, 0.0, 0.0, 1.0];
        let view = View {
            width: 32,
            height: 32,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        };
        let mut draw = Draw::new(mesh_handle, tint);
        let mut first =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut first).unwrap();
        let mut expected = vec![(
            first.submission().id(),
            first.readback().unwrap(),
            16 * 32 + 16,
            [255, 0, 0, 255],
        )];
        draw.tint = [0.0, 1.0, 0.0, 1.0];
        draw.transform[3][0] = -0.75;
        let mut shifted =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut shifted).unwrap();
        expected.push((
            shifted.submission().id(),
            shifted.readback().unwrap(),
            16 * 32 + 4,
            [0, 255, 0, 255],
        ));
        draw.transform[3][0] = 0.0;
        draw.tint = [0.0, 0.0, 1.0, 1.0];
        let mut centered =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut centered).unwrap();
        expected.push((
            centered.submission().id(),
            centered.readback().unwrap(),
            16 * 32 + 16,
            [0, 0, 255, 255],
        ));
        // Replace vertex content while all three accepted frames are owned by
        // the runtime. The old mesh buffers remain retained by their work.
        assets
            .update_mesh(mesh_handle, mesh([1.0, 0.0, 1.0, 1.0]))
            .unwrap();
        draw.tint = [1.0; 4];
        let mut replaced =
            render_frame(engine, assets, &view, &[draw], true, |_, color| Ok(color)).unwrap();
        engine.release_output(&mut replaced).unwrap();
        expected.push((
            replaced.submission().id(),
            replaced.readback().unwrap(),
            16 * 32 + 16,
            [255, 0, 255, 255],
        ));
        assets.delete_mesh(mesh_handle).unwrap();
        engine.unload_mesh(mesh_handle);
        engine.collect_retired_meshes(Some(assets)).unwrap();

        assert_eq!(engine.runtime().pending_frames(), 4);
        collect(engine, &expected);
        assert!(assets.snapshot(&view, &[draw]).is_err());
    });
}

#[test]
#[ignore = "real-GPU opaque draw-list ownership through pending mesh/tint deletion; requires Core/SyncVal"]
fn vulkan_engine_opaque_batch_keeps_pending_draws_and_each_mesh_alive() {
    with_engine(|engine, assets| {
        let near = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let far_vertices =
            [[-0.4, -0.4, 0.75], [0.4, -0.4, 0.75], [0.0, 0.4, 0.75]].map(|position| Vertex {
                position,
                color: [1.0; 4],
            });
        let farther = mesh_data(&far_vertices, &[0, 1, 2]).unwrap();
        let far = assets.create_mesh(farther).unwrap();
        let equal = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let red = [1.0, 0.0, 0.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let mut draws = [
            Draw::new(near, red),
            Draw::new(far, green),
            Draw::new(equal, blue),
        ]
        .repeat(32);
        let view = View {
            width: 32,
            height: 32,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        };
        let extension = |graph: &mut Graph, color| {
            let output = graph.image("custom swap", graph.image_desc(color)?)?;
            image_transform::dispatch(
                graph,
                "swap after opaque",
                color,
                output,
                Transform::SwapRedBlue,
            )?;
            Ok(output)
        };
        let mut first = render_frame(engine, assets, &view, &draws, true, extension).unwrap();
        engine.release_output(&mut first).unwrap();
        for draw in &mut draws {
            if draw.mesh == near {
                draw.tint = [0.0, 1.0, 0.0, 1.0];
            }
        }
        assets.update_mesh(near, mesh([1.0; 4])).unwrap();
        let mut second = render_frame(engine, assets, &view, &draws, true, extension).unwrap();
        engine.release_output(&mut second).unwrap();
        for handle in [near, far, equal] {
            assets.delete_mesh(handle).unwrap();
            engine.unload_mesh(handle);
        }
        engine.collect_retired_meshes(Some(assets)).unwrap();
        assert_eq!(engine.runtime().pending_frames(), 2);
        collect(
            engine,
            &[
                (
                    first.submission().id(),
                    first.readback().unwrap(),
                    16 * 32 + 16,
                    [0, 0, 255, 255],
                ),
                (
                    second.submission().id(),
                    second.readback().unwrap(),
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
            clear_color: [0.0, 0.0, 1.0, 1.0],
        };
        let mut frame = render_frame(engine, assets, &view, &[], true, |graph, color| {
            let target = graph.image("external custom invert", ImageDesc::rgba8(8, 8))?;
            image_transform::dispatch(
                graph,
                "external inversion",
                color,
                target,
                Transform::Invert,
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
        let bytes = complete
            .result
            .test_readback(frame.readback().unwrap())
            .unwrap();
        assert_eq!(bytes, [255, 255, 0, 255].repeat(64));
        let before = engine.runtime().stats().registered_resources;
        let rejected = build_frame(engine, assets, &view, &[], false, |_, _| {
            Err(GraphError::custom("extension rejected"))
        });
        assert!(matches!(rejected, Err(EngineRenderError::Graph(_))));
        let prepared =
            build_frame(engine, assets, &view, &[], false, |_, color| Ok(color)).unwrap();
        drop(prepared.compile().unwrap());
        assert_eq!(engine.runtime().pending_frames(), 0);
        assert_eq!(engine.runtime().stats().registered_resources, before);

        let black_view = View {
            clear_color: [0.0, 0.0, 0.0, 1.0],
            ..view
        };
        let handle = assets.create_mesh(mesh([1.0; 4])).unwrap();
        let tint = [1.0; 4];
        let draw = Draw::new(handle, tint);
        let captured = assets.snapshot(&black_view, &[draw]).unwrap();
        prepare_meshes(engine, &captured);
        let old_a = build_snapshot(engine, &captured, true, |_, c| Ok(c)).unwrap();
        let old_b = build_snapshot(engine, &captured, true, |_, c| Ok(c)).unwrap();
        let old_a = old_a.compile().unwrap();
        let old_b = old_b.compile().unwrap();
        // Both prepared graphs pin the same resident old version. Later CPU
        // updates do not alter their immutable content or registration pins.
        assets
            .update_mesh(handle, mesh([0.0, 0.0, 1.0, 1.0]))
            .unwrap();
        let mut a = engine.submit_frame(old_a).unwrap();
        let mut b = engine.submit_frame(old_b).unwrap();
        engine.release_output(&mut a).unwrap();
        engine.release_output(&mut b).unwrap();
        assert_eq!(engine.runtime().stats().registered_resources, 2);
        let new_a = build_frame(engine, assets, &black_view, &[draw], true, |_, c| Ok(c)).unwrap();
        let new_b = build_frame(engine, assets, &black_view, &[draw], true, |_, c| Ok(c)).unwrap();
        let mut c = engine.submit_frame(new_a.compile().unwrap()).unwrap();
        let mut d = engine.submit_frame(new_b.compile().unwrap()).unwrap();
        engine.release_output(&mut c).unwrap();
        engine.release_output(&mut d).unwrap();
        // Two views share each immutable resident version: the old and new
        // content each have one buffer pair, independent of CPU asset lifetime.
        assert_eq!(engine.runtime().stats().registered_resources, 4);
        collect(
            engine,
            &[
                (
                    a.submission().id(),
                    a.readback().unwrap(),
                    4 * 8 + 4,
                    [255; 4],
                ),
                (
                    b.submission().id(),
                    b.readback().unwrap(),
                    4 * 8 + 4,
                    [255; 4],
                ),
                (
                    c.submission().id(),
                    c.readback().unwrap(),
                    4 * 8 + 4,
                    [0, 0, 255, 255],
                ),
                (
                    d.submission().id(),
                    d.readback().unwrap(),
                    4 * 8 + 4,
                    [0, 0, 255, 255],
                ),
            ],
        );
        let cached = build_frame(engine, assets, &black_view, &[draw], true, |_, c| Ok(c)).unwrap();
        let cached = cached.compile().unwrap();
        assert!(
            !cached
                .graph()
                .ordered_pass_names()
                .any(|name| name == "mesh vertex upload")
        );
        let mut pending = engine.submit_frame(cached).unwrap();
        engine.release_output(&mut pending).unwrap();
        assets.delete_mesh(handle).unwrap();
        engine.unload_mesh(handle);
        engine.collect_retired_meshes(Some(assets)).unwrap();

        collect(
            engine,
            &[(
                pending.submission().id(),
                pending.readback().unwrap(),
                4 * 8 + 4,
                [0, 0, 255, 255],
            )],
        );
        prepare_meshes(engine, &captured);
        let mut old = render_snapshot(engine, &captured, true, |_, c| Ok(c)).unwrap();
        engine.release_output(&mut old).unwrap();
        assert_eq!(engine.runtime().stats().registered_resources, 2);
        collect(
            engine,
            &[(
                old.submission().id(),
                old.readback().unwrap(),
                4 * 8 + 4,
                [255; 4],
            )],
        );
    });
}

/// The shipping path needs neither Renderer diagnostic state nor native layers.
/// This is a separate content acceptance run; strict Core/SyncVal tests above
/// remain unchanged and are run independently with development diagnostics.
#[cfg(not(feature = "vulkan-diagnostics"))]
#[test]
#[ignore = "explicit shipping Vulkan content/ownership acceptance; native layers disabled"]
fn vulkan_shipping_mesh_guarantees_preserve_pixels_without_cpu_retention() {
    use std::sync::Arc;

    let mut runtime = VulkanRuntime::new_with_validation(
        RuntimeConfig::default(),
        crate::vulkan::ValidationMode::Disabled,
    )
    .unwrap();
    assert_ne!(
        runtime.owner.device.device_type(),
        ash::vk::PhysicalDeviceType::CPU
    );
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_working_set(
            FrameCapacity::default(),
            &[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 84 },
                    count: 1,
                },
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 12 },
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(32, 32),
                    count: 1,
                },
                ResourceClass::Image {
                    desc: ImageDesc::depth32(32, 32),
                    count: 1,
                },
            ],
        )
        .unwrap();
    let mut engine = VulkanEngine::new(
        runtime,
        MeshCacheConfig::default(),
        EngineFrameCapacity::default(),
    )
    .unwrap();
    let mut assets = Assets::new(4).unwrap();
    let data = mesh([1.0, 0.0, 0.0, 1.0]);
    let vertices = Arc::downgrade(&data.vertices);
    let indices = Arc::downgrade(&data.indices);
    let mesh_handle = assets.create_mesh(data).unwrap();
    let tint = [1.0; 4];
    let view = View {
        width: 32,
        height: 32,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    };
    let draw = Draw::new(mesh_handle, tint);
    let snapshot = assets.snapshot(&view, &[draw]).unwrap();
    prepare_meshes(&mut engine, &snapshot);
    let prepared = build_snapshot(&mut engine, &snapshot, true, |_, color| Ok(color)).unwrap();
    let prepared = prepared.compile().unwrap();
    drop(snapshot);
    assets.delete_mesh(mesh_handle).unwrap();

    engine.release_idle_inputs();
    engine.collect_retired_meshes(Some(&assets)).unwrap();
    assets.collect_retired_meshes();
    assert!(
        vertices.upgrade().is_none(),
        "resident vertex buffers retain no CPU mesh payload"
    );
    assert!(
        indices.upgrade().is_none(),
        "resident index buffers retain only their immutable bound"
    );
    let mut frame = engine.submit_frame(prepared).unwrap();
    engine.release_output(&mut frame).unwrap();
    for entry in &engine.runtime().entries {
        if entry.registered {
            assert_eq!(
                entry.physical().lock().scheduled.provenance.test_evidence(),
                (0, 0),
                "managed streams retain no byte provenance and perform zero index scans"
            );
        }
    }
    collect(
        &mut engine,
        &[(
            frame.submission().id(),
            frame.readback().unwrap(),
            16 * 32 + 16,
            [255, 0, 0, 255],
        )],
    );
    engine.unload_all_meshes();
    engine.collect_retired_meshes(Some(&assets)).unwrap();
    assert!(
        engine
            .runtime_mut()
            .drain_frames(Duration::from_secs(10))
            .unwrap()
    );
    drop(engine);
    let report = monitor.report();
    assert_eq!(report.requested, crate::vulkan::ValidationMode::Disabled);
    assert!(report.is_clean());
    assert!(report.layer_status_messages.is_empty());
    assert!(!report.startup_status_recognized);
    assert_eq!(report.core_validation, None);
    assert_eq!(report.synchronization_validation, None);
    eprintln!(
        "shipping mesh acceptance: diagnostic feature absent; native Disabled requested; pixels checked; retained provenance=0; index scans=0; external layer injection unobserved, not disproved"
    );
}
