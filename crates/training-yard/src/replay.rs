//! Two cameras and four directional shadow cascades recorded on the host's Graph.
use crate::{REPLAY_FRAMES, Yard, camera, replay_action, validation};
use renderer::{
    canvas::{self, Layer, Paint},
    core::{
        ImageFormat,
        graph::{GraphPool, ImageDesc, LoadOp, RasterRect, SampleCount},
        runtime::RuntimeConfig,
    },
    pbr::ShadowSettings,
    pipeline::{CameraGroup, PipelineResult, RendererSetup, StandardConfig, Technique},
};
use std::time::Duration;
mod monitor;
use monitor::SecurityMonitor;
mod selection;
use selection::Selection;

pub fn host(config: RuntimeConfig) -> PipelineResult<()> {
    let mut yard = Yard::new()?;
    let mut cameras = [camera(1, [48, 24])?, camera(2, [12, 6])?];
    let mut detailed = StandardConfig::new(cameras[0].extent);
    detailed.shadows = Some(ShadowSettings {
        resolution: 32,
        distance: 2.,
        ..Default::default()
    });
    let mut setup = RendererSetup::new(CameraGroup::standard(
        cameras[0],
        Technique::Forward,
        detailed,
    ));
    let mut preview = StandardConfig::new(cameras[1].extent);
    preview.shadows = Some(ShadowSettings {
        resolution: 16,
        distance: 2.,
        ..Default::default()
    });
    let mut monitors = CameraGroup::standard(cameras[1], Technique::Forward, preview);
    let mut monitor = SecurityMonitor::new()?;
    monitors.add_view(monitor.camera)?;
    setup.add_group(monitors);
    // A mouse query can remain outstanding while the main view submits its next frame.
    setup.frames_in_flight = std::num::NonZeroUsize::new(2).unwrap();
    let mut plan = setup.prepare(yard.assets())?;
    let mut selection = Selection::prepare(&mut plan, cameras[0])?;
    // The host's known asset pack can stream one four-vertex armor and its material.
    for size in [
        4 * renderer::passes::pbr::VERTEX_BYTES as u64,
        6 * 4,
        renderer::pbr::MaterialUniform::DESC.size,
    ] {
        plan.preparation_mut()
            .reserve_buffer(renderer::core::graph::BufferDesc { size }, 1)?;
    }
    monitor.prepare(&mut plan)?;
    for [w, h] in cameras.map(|c| c.extent) {
        plan.reserve_readback("viewport", ImageDesc::rgba8(w, h))?;
    }
    for _ in 0..renderer::pbr::CASCADE_COUNT {
        plan.reserve_readback("caster depth", ImageDesc::depth32(32, 32))?;
    }
    plan.preparation_mut()
        .reserve_raster_pass("canvas", SampleCount::X1, |pass| {
            pass.attachment()?;
            pass.draws(&canvas::programs(ImageFormat::Rgba8Unorm)?[1], 1)
        })?;
    let mut drawing = plan.build(config)?;
    let mut host_pool = GraphPool::new(drawing.runtime().limits().graph, 1)?;
    let mut objects = Vec::with_capacity(4);
    let mut rest_shadow = vec![false; 32 * 32 * renderer::pbr::CASCADE_COUNT];
    let mut depth = Vec::with_capacity(rest_shadow.len());
    let mut rest_mask = [vec![false; 24 * 24], vec![false; 24 * 24]];
    let mut mask = Vec::with_capacity(rest_mask[0].len());
    let mut shadow_reads = Vec::with_capacity(renderer::pbr::CASCADE_COUNT);
    let mut storage = vec![0; 48 * 24 * 4];
    let mut screen = [0; 48 * 24 * 4];
    for step in 0..REPLAY_FRAMES {
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let before = renderer::core::measurement::Snapshot::now();
        if let Some(action) = replay_action(step) {
            yard.apply(action, |assets| {
                assets.materials.collect_gpu(drawing.runtime())?;
                drawing.prepare_assets(assets)
            })?;
        }
        if step == 8 || step == 9 {
            if step == 8 {
                let mut clip = renderer::unlit_forward::IDENTITY;
                clip[3][0] = 0.125;
                cameras[1].camera = renderer::pbr::ViewInput::new(
                    clip,
                    [-0.125, 0., -1.],
                    [0., 0., 1.],
                    [1., 2.],
                    renderer::pbr::Projection::Orthographic,
                )?;
            }
            drawing.camera_cut(cameras[1].id)?;
        }
        let mut graph = host_pool.begin()?;
        let target = graph.image("host viewport", ImageDesc::rgba8(48, 24))?;
        let (mut frame, runtime) = drawing.begin_host_frame(graph)?;
        let mut reads = [None; 2];
        for (camera, read) in cameras.iter().zip(&mut reads) {
            let input = yard.extract(camera, &mut objects);
            let hdr = frame.draw_scene(camera, input)?.color;
            let main = camera.id == cameras[0].id;
            if (7..=10).contains(&step) {
                let reused = frame
                    .standard_output(camera.id)
                    .unwrap()
                    .reused_shadows()
                    .unwrap();
                assert_eq!(
                    reused,
                    [main || !matches!(step, 8 | 9); 4],
                    "history after preview cut, step {step} view {:?}",
                    camera.id
                );
            }
            let image = frame.write_output(camera.id, hdr, 1., main.then_some(target))?;
            if !main {
                *read = Some(
                    frame
                        .graph()
                        .readback_image("viewport", image.raw_image())?,
                );
            }
        }
        let shadows = frame
            .standard_output(cameras[0].id)
            .unwrap()
            .scene
            .shadows
            .unwrap();
        shadow_reads.clear();
        for shadow in shadows {
            shadow_reads.push(frame.graph().readback_image("caster depth", shadow)?);
        }
        let refresh = monitor.record(&mut frame, step, &yard, &mut objects)?;
        let (ids, samples) = selection.sample(&mut frame, &yard, &mut objects)?;
        let (mut graph, mut views) = frame.finish_recording()?;
        canvas::record(
            &mut graph,
            target,
            LoadOp::Load,
            &[Layer {
                paint: Paint::Color([1.; 4]),
                rect: RasterRect::new(0, 0, 1, 1),
                clip: None,
            }],
        )?;
        reads[0] = Some(graph.readback_image("viewport", target)?);
        let outline = selection.highlight(&mut graph, target, ids, &yard)?;
        let monitor_read = monitor.composite(&mut graph, target, refresh)?;
        let composed = graph.readback_image("composite viewport", target)?;
        let graph = graph.compile()?;
        let ticket = if let Some(fresh) = refresh {
            let (ticket, image) = runtime.submit_with_image(&graph, fresh)?;
            monitor.latest = Some(image);
            ticket
        } else {
            runtime.submit(&graph)?
        };
        views.publish(&ticket)?;
        drop(views);
        let mut result = runtime
            .wait_for_frame(ticket.id(), Duration::from_secs(15))?
            .ok_or("view timeout")?;
        result.readback_into(composed, &mut screen)?;
        selection.check(samples, &mut result, &yard, step)?;
        if let Some(read) = monitor_read {
            result.readback_into(read, &mut monitor.reference)?;
            monitor.verify_subject(step);
        }
        depth.clear();
        for &read in &shadow_reads {
            let mut layer = [0; 32 * 32 * 4];
            result.readback_into(read, &mut layer)?;
            depth.extend(
                layer
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&v| f32::from_le_bytes(v) < 1.),
            );
        }
        if step == 0 {
            assert!(depth.iter().any(|&occupied| occupied), "empty cascades");
            rest_shadow.copy_from_slice(&depth);
        } else {
            assert_eq!(
                depth != rest_shadow,
                (4..=5).contains(&step) || (16..=21).contains(&step),
                "caster coverage at step {step}"
            );
        }
        for (camera, read) in cameras.iter().zip(reads) {
            let [w, h] = camera.extent.map(|n| n as usize);
            let pixels = &mut storage[..w * h * 4];
            result.readback_into(read.unwrap(), pixels)?;
            if camera.id == cameras[0].id {
                assert_eq!(pixels[..4], [255; 4]);
                let mut outlined = [0; 48 * 24 * 4];
                outline.verify(&mut result, pixels, &mut outlined)?;
                monitor.verify_composite(step, &screen, &outlined);
                pixels[..4].copy_from_slice(&[0, 0, 0, 255]);
            }
            for (side, baseline) in rest_mask.iter_mut().enumerate() {
                // Bins are black, red body, blue armor, green armor and cyan body.
                let mut counts = [0usize; 6];
                mask.clear();
                for row in pixels.chunks_exact(w * 4) {
                    for p in row[side * w * 2..(side + 1) * w * 2].as_chunks::<4>().0 {
                        let bin = match [p[0] > 100, p[1] > 100, p[2] > 100] {
                            [false, false, false] => 0,
                            [true, false, false] => 1,
                            [false, false, true] => 2,
                            [false, true, false] => 3,
                            [false, true, true] => 4,
                            [true, true, false] => 5,
                            rgb => panic!("unexpected color {rgb:?}"),
                        };
                        counts[bin] += 1;
                        mask.push(bin != 0);
                    }
                }
                let detailed = camera.id == cameras[0].id;
                if detailed {
                    if step == 0 {
                        baseline.copy_from_slice(&mask);
                    } else {
                        assert_eq!(
                            mask != *baseline,
                            (step, side) == (4, 0)
                                || (step, side) == (5, 1)
                                || (side == 0 && (16..=21).contains(&step)),
                            "posed silhouette at step {step} side {side}"
                        );
                    }
                }
                let glowing = side == 0 && (1..=2).contains(&step);
                let equipped = side == 0 && (16..=20).contains(&step);
                let occupied = side != 0 || step != 21;
                assert_eq!(
                    counts.map(|n| n > 0),
                    [
                        true,
                        step < 2,
                        detailed && occupied && !glowing && !equipped,
                        detailed && glowing,
                        occupied && step >= 2,
                        detailed && equipped,
                    ],
                    "step {step} side {side} size {w}x{h}: {counts:?}"
                );
            }
        }
        drop((result, ticket));
        if step == 20 {
            selection.click(&mut drawing, &yard, &mut objects)?;
        }
        if step == 22 {
            selection.resolve_after_respawn(&mut drawing, &yard)?;
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        if (4..14).contains(&step) {
            let delta = renderer::core::measurement::Snapshot::now().since(before);
            assert!(
                delta.is_allocation_free(),
                "animated frame {step}: {delta:?}"
            );
        }
    }
    drop(monitor);
    let mut runtime = drawing.into_runtime()?;
    yard.release(&mut runtime)?;
    validation::check_validation(runtime)
}
