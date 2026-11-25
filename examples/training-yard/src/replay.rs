//! Two cameras and four directional shadow cascades recorded on the host's Graph.
use crate::{REPLAY_FRAMES, Yard, camera, replay_action, validation};
use sashiko_renderer::{
    canvas::{self, Layer, Paint},
    core::{
        ImageFormat,
        graph::{GraphPool, ImageDesc, LoadOp, RasterRect, SampleCount},
        runtime::{ManagedImage, Runtime, RuntimeConfig},
    },
    pbr::ShadowSettings,
    pipeline::{
        CameraGroup, PipelineResult, PreparedSetup, RendererSetup, SceneRenderer, StandardConfig,
        Technique,
    },
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
mod lighting;
mod monitor;
use monitor::SecurityMonitor;
mod selection;
use selection::Selection;

struct FrameRecord(Option<PathBuf>);
impl FrameRecord {
    fn start(output: Option<&Path>, step: usize) -> PipelineResult<Self> {
        let dir = output.map(|root| root.join(format!("frame-{step:02}")));
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir)?;
            std::fs::write(
                dir.join("input.txt"),
                format!(
                    "frame {}\naction {:?}\npreview cut {}\nmonitor update {}\n",
                    step + 1,
                    replay_action(step),
                    matches!(step, 8 | 9),
                    (step + 1).is_multiple_of(6)
                ),
            )?;
        }
        Ok(Self(dir))
    }
    fn save(&self, file: &str, bytes: &[u8]) -> PipelineResult<()> {
        if let Some(dir) = &self.0 {
            std::fs::write(dir.join(file), bytes)?;
        }
        Ok(())
    }
}

pub fn host(config: RuntimeConfig, output: Option<&Path>) -> PipelineResult<()> {
    let (runtime, ()) = squad(output, |plan| Ok((plan.build(config)?, ())))?;
    let runtime = crate::effects::host(runtime)?;
    validation::check_validation(lighting::check(runtime)?)
}

/// Record the same composed viewport for an offscreen host or for presentation
/// on a window sharing this device. Only the caller builds the device and presents.
pub(crate) trait Viewport {
    /// Break on close, or continue with the size of the drawable output.
    fn begin(
        &mut self,
        _: &mut SceneRenderer,
        _: bool,
    ) -> PipelineResult<std::ops::ControlFlow<(), Option<[u32; 2]>>> {
        Ok(std::ops::ControlFlow::Continue(None))
    }
    /// Keep the prior screen responsive and retire its presentation references
    /// while the replay waits for an asynchronous equipment change.
    fn poll_loading(&mut self) -> PipelineResult<std::ops::ControlFlow<()>> {
        Ok(std::ops::ControlFlow::Continue(()))
    }
    fn present(&mut self, _: &mut Runtime, _: ManagedImage) -> PipelineResult<()>;
}

impl Viewport for () {
    fn present(&mut self, _: &mut Runtime, _: ManagedImage) -> PipelineResult<()> {
        unreachable!("offscreen viewport did not request a display image")
    }
}

pub(crate) fn squad<P: Viewport>(
    output: Option<&Path>,
    build: impl FnOnce(PreparedSetup<'_>) -> PipelineResult<(SceneRenderer, P)>,
) -> PipelineResult<(Runtime, P)> {
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
    setup.additional_objects = crate::yard::EQUIPMENT_DRAWS - 4;
    // A mouse query can remain outstanding while the main view submits its next frame.
    setup.frames_in_flight = std::num::NonZeroUsize::new(2).unwrap();
    let mut plan = setup.prepare(yard.assets())?;
    if output.is_some() {
        plan.reserve_readback("main depth", ImageDesc::depth32(48, 24))?;
    }
    let mut selection = Selection::prepare(&mut plan, cameras[0])?;
    Yard::reserve_equipment(plan.preparation_mut())?;
    monitor.prepare(&mut plan)?;
    for [w, h] in cameras.map(|c| c.extent) {
        plan.reserve_readback("viewport", ImageDesc::rgba8(w, h))?;
    }
    for _ in 0..sashiko_renderer::pbr::CASCADE_COUNT {
        plan.reserve_readback("caster depth", ImageDesc::depth32(32, 32))?;
    }
    plan.preparation_mut()
        .reserve_raster_pass("canvas", SampleCount::X1, |pass| {
            pass.attachment()?;
            pass.draws(&canvas::programs(ImageFormat::Rgba8Unorm)?[1], 1)
        })?;
    let (mut drawing, mut destination) = build(plan)?;
    if let Some(dir) = output {
        std::fs::create_dir_all(dir)?;
        #[cfg(feature = "vulkan")]
        if let Some(device) = drawing.runtime().as_vulkan() {
            std::fs::write(
                dir.join("adapter.txt"),
                format!("{:#?}", device.device_info()),
            )?;
        }
        #[cfg(all(windows, feature = "d3d12"))]
        if let Some(device) = drawing.runtime().as_d3d12() {
            std::fs::write(
                dir.join("adapter.txt"),
                format!("{:#?}", device.device_info()),
            )?;
        }
    }
    let mut host_pool = GraphPool::new(drawing.runtime().limits().graph, 1)?;
    let mut objects = Vec::with_capacity(crate::yard::EQUIPMENT_DRAWS);
    let mut rest_shadow = vec![false; 32 * 32 * sashiko_renderer::pbr::CASCADE_COUNT];
    let mut depth = Vec::with_capacity(rest_shadow.len());
    let mut rest_mask = [vec![false; 24 * 24], vec![false; 24 * 24]];
    let mut mask = Vec::with_capacity(rest_mask[0].len());
    let mut shadow_reads = Vec::with_capacity(sashiko_renderer::pbr::CASCADE_COUNT);
    let mut storage = vec![0; 48 * 24 * 4];
    let mut screen = [0; 48 * 24 * 4];
    'frames: for step in 0..REPLAY_FRAMES {
        let std::ops::ControlFlow::Continue(window_extent) =
            destination.begin(&mut drawing, selection.has_pending_click())?
        else {
            selection.discard_pending(&mut drawing)?;
            break;
        };
        let record = FrameRecord::start(output, step)?;
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let before = sashiko_renderer::core::measurement::Snapshot::now();
        if let Some(action) = replay_action(step) {
            yard.apply(action, &mut drawing, |r, assets| {
                Ok(r.prepare_assets(assets)?)
            })?;
        }
        let started = std::time::Instant::now();
        loop {
            yard.advance_equipment(&mut drawing)?;
            if !yard.pending_equipment() {
                break;
            }
            if destination.poll_loading()?.is_break() {
                selection.discard_pending(&mut drawing)?;
                break 'frames;
            }
            if started.elapsed() > Duration::from_secs(15) {
                return Err("equipment replay timed out".into());
            }
            let graph = host_pool.begin()?;
            let (mut frame, runtime) = drawing.begin_host_frame(graph)?;
            let scene = frame.draw_scene(&cameras[0], yard.extract(&cameras[0], &mut objects))?;
            let out = frame.write_output(cameras[0].id, scene.color, 1., None)?;
            frame.graph().readback_image("viewport", out.raw_image())?;
            let (graph, mut views) = frame.finish_recording()?;
            let submission = runtime.submit(&graph.compile()?)?;
            views.publish(&submission)?;
            drop(views);
            runtime
                .wait_for_frame(submission.id(), Duration::from_secs(15))?
                .ok_or("old equipment render timed out")?;
        }
        if step == 8 || step == 9 {
            if step == 8 {
                let mut clip = sashiko_renderer::unlit_forward::IDENTITY;
                clip[3][0] = 0.125;
                cameras[1].camera = sashiko_renderer::pbr::ViewInput::new(
                    clip,
                    [-0.125, 0., -1.],
                    [0., 0., 1.],
                    [1., 2.],
                    sashiko_renderer::pbr::Projection::Orthographic,
                )?;
            }
            drawing.camera_cut(cameras[1].id)?;
        }
        let mut graph = host_pool.begin()?;
        let target = graph.image("host viewport", ImageDesc::rgba8(48, 24))?;
        let (mut frame, runtime) = drawing.begin_host_frame(graph)?;
        let mut reads = [None; 2];
        let mut main_depth = None;
        for (camera, read) in cameras.iter().zip(&mut reads) {
            let input = yard.extract(camera, &mut objects);
            let scene = frame.draw_scene(camera, input)?;
            let hdr = scene.color;
            let main = camera.id == cameras[0].id;
            if main && output.is_some() {
                main_depth =
                    Some(frame.graph().readback_image(
                        "main depth",
                        scene.depth.ok_or("missing primary depth")?,
                    )?);
            }
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
        let display = if let Some([w, h]) = window_extent {
            let layer = Layer {
                paint: Paint::Image(graph.image_view(target, 0, 0)?),
                rect: RasterRect::new(0, 0, w, h),
                clip: None,
            };
            let handle = graph.image("window viewport", ImageDesc::rgba8(w, h))?;
            canvas::record(&mut graph, handle, LoadOp::Clear([0.; 4]), &[layer])?;
            graph.export_image(handle)?;
            Some(handle)
        } else {
            None
        };
        let graph = graph.compile()?;
        let (ticket, display) = match (refresh, display) {
            (Some(fresh), Some(screen)) => {
                let (ticket, [feed, screen]) =
                    runtime.submit_with_images(&graph, [fresh, screen])?;
                monitor.latest = Some(feed);
                (ticket, Some(screen))
            }
            (Some(fresh), None) => {
                let (ticket, feed) = runtime.submit_with_image(&graph, fresh)?;
                monitor.latest = Some(feed);
                (ticket, None)
            }
            (None, Some(screen)) => {
                let (ticket, screen) = runtime.submit_with_image(&graph, screen)?;
                (ticket, Some(screen))
            }
            (None, None) => (runtime.submit(&graph)?, None),
        };
        views.publish(&ticket)?;
        drop(views);
        if let Some(screen) = display {
            destination.present(runtime, screen)?;
        }
        let mut result = runtime
            .wait_for_frame(ticket.id(), Duration::from_secs(15))?
            .ok_or("view timeout")?;
        result.readback_into(composed, &mut screen)?;
        record.save("composed.rgba8", &screen)?;
        if let Some(depth) = main_depth {
            result.readback_into(depth, &mut storage)?;
            record.save("main.depth32", &storage)?;
        }
        selection.check(samples, &mut result, &yard, step)?;
        if let Some(read) = monitor_read {
            result.readback_into(read, &mut monitor.reference)?;
            monitor.verify_subject(step);
        }
        depth.clear();
        for (&read, name) in shadow_reads.iter().zip([
            "shadow0.depth32",
            "shadow1.depth32",
            "shadow2.depth32",
            "shadow3.depth32",
        ]) {
            let mut layer = [0; 32 * 32 * 4];
            result.readback_into(read, &mut layer)?;
            record.save(name, &layer)?;
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
            record.save(
                if camera.id == cameras[0].id {
                    "main.rgba8"
                } else {
                    "preview.rgba8"
                },
                pixels,
            )?;
            if camera.id == cameras[0].id {
                assert_eq!(pixels[..4], [255; 4]);
                let mut outlined = [0; 48 * 24 * 4];
                outline.verify(&mut result, pixels, &mut outlined, &record)?;
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
        if output.is_none() && window_extent.is_none() && (4..14).contains(&step) {
            let delta = sashiko_renderer::core::measurement::Snapshot::now().since(before);
            assert!(
                delta.is_allocation_free(),
                "animated frame {step}: {delta:?}"
            );
        }
    }
    drop(monitor);
    let mut runtime = drawing.into_runtime()?;
    yard.release(&mut runtime)?;
    Ok((runtime, destination))
}
