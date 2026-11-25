//! Standard Frame entry points shared by the interactive window and headless capture.
use crate::{
    REPLAY_FRAMES, Yard, camera,
    effects::{self, FiringLane},
    replay_action, validation,
};
use sashiko_renderer::{
    Frame, Renderer,
    canvas::{Layer, Paint},
    core::{graph::RasterRect, runtime::RuntimeConfig},
    pipeline::{PipelineResult, RenderObject},
};
use std::{io::Write, path::Path, time::Duration};

pub(crate) fn draw(
    frame: &mut Frame<'_>,
    yard: &Yard,
    scratch: &mut Vec<RenderObject>,
) -> PipelineResult<()> {
    let scene_view = *frame.scene_view();
    frame.draw_scene(yard.extract(&scene_view, scratch), 1.)?;
    frame.draw_overlay(&[Layer {
        paint: Paint::Color([1.; 4]),
        rect: RasterRect::new(0, 0, 1, 1),
        clip: None,
    }])?;
    Ok(())
}

pub fn headless(config: RuntimeConfig, output: Option<&Path>) -> PipelineResult<()> {
    let mut yard = Yard::new()?;
    let camera = camera(1, [48, 24])?;
    let mut renderer = Renderer::headless_with(
        config,
        camera,
        yard.assets(),
        std::num::NonZeroUsize::new(2).unwrap(),
        Yard::reserve_equipment,
    )?;
    let mut scratch = Vec::with_capacity(crate::yard::EQUIPMENT_DRAWS);
    let mut pixels = [0; 48 * 24 * 4];
    if let Some(dir) = output {
        std::fs::create_dir_all(dir)?;
    }
    let mut delayed = None;
    for step in 0..REPLAY_FRAMES {
        if let Some(action) = replay_action(step) {
            yard.apply(action, &mut renderer, |renderer, mut assets| {
                if let Err(error) = renderer.prepare_scene(assets.reborrow()) {
                    let recovery = error.capacity().map(|c| c.recovery());
                    if recovery != Some(sashiko_renderer::CapacityRecovery::ReleaseOwners) {
                        return Err(error.into());
                    }
                    let Some((previous, capture)) = delayed.take() else {
                        return Err(error.into());
                    };
                    consume(renderer, capture, &mut pixels, previous, output)?;
                    renderer.prepare_scene(assets)?;
                }
                Ok(())
            })?;
        }
        let started = std::time::Instant::now();
        loop {
            yard.advance_equipment(&mut renderer)?;
            if !yard.pending_equipment() {
                break;
            }
            if started.elapsed() > Duration::from_secs(15) {
                return Err("equipment replay timed out".into());
            }
            // Draw the current equipment while the replay awaits its next input checkpoint.
            if let Some((previous, capture)) = delayed.take() {
                consume(&mut renderer, capture, &mut pixels, previous, output)?;
            }
            let mut frame = renderer.begin_frame()?;
            draw(&mut frame, &yard, &mut scratch)?;
            let capture = frame.capture()?;
            if !renderer.read_capture(&capture, &mut pixels, Duration::from_secs(15))? {
                return Err("old equipment capture timed out".into());
            }
        }
        assert_eq!(yard.load_error().is_some(), step >= 19);
        if let Some((step, capture)) = delayed.take() {
            consume(&mut renderer, capture, &mut pixels, step, output)?;
        }
        let mut frame = renderer.begin_frame()?;
        draw(&mut frame, &yard, &mut scratch)?;
        let capture = frame.capture()?;
        if matches!(step, 15 | 20) {
            // Keep the old result through the next asset change. Preparation may
            // explicitly request its return; deleting the actor needs no GPU wait.
            delayed = Some((step, capture));
        } else {
            consume(&mut renderer, capture, &mut pixels, step, output)?;
        }
    }
    let mut lane = FiringLane::new()?;
    for step in 0..effects::FRAMES {
        match step {
            0 => renderer.prepare_scene(lane.assets())?,
            2 => renderer.set_scene_extent(Some([24, 12]), lane.assets())?,
            4 => renderer.set_scene_extent(None, lane.assets())?,
            _ => {}
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let before = sashiko_renderer::core::measurement::Snapshot::now();
        let mut frame = renderer.begin_frame()?;
        let extent = frame.scene_view().extent;
        frame.draw_scene(lane.input(step)?, 1.)?;
        frame.draw_overlay(&[effects::HUD])?;
        let capture = frame.capture()?;
        if !renderer.read_capture(&capture, &mut pixels, Duration::from_secs(15))? {
            return Err("firing lane capture timed out".into());
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        {
            let events = sashiko_renderer::core::measurement::Snapshot::now().since(before);
            assert!(
                events.is_allocation_free(),
                "capture lane step {step}: {events:?}"
            );
        }
        if let Some(dir) = output {
            write_frame(dir, REPLAY_FRAMES + step, &pixels)?;
        }
        effects::verify(&pixels, camera.extent, extent, step)?;
    }
    let mut runtime = renderer.into_runtime()?;
    lane.release(&mut runtime)?;
    yard.release(&mut runtime)?;
    validation::check_validation(runtime)
}

fn consume(
    renderer: &mut Renderer,
    capture: sashiko_renderer::Capture,
    pixels: &mut [u8; 48 * 24 * 4],
    step: usize,
    output: Option<&Path>,
) -> PipelineResult<()> {
    if !renderer.read_capture(&capture, pixels, Duration::from_secs(15))? {
        return Err(format!("capture timeout at replay step {step}").into());
    }
    if let Some(dir) = output {
        write_frame(dir, step, pixels)?;
    }
    if pixels[..4] != [255; 4] {
        return Err(format!("HUD missing at replay step {step}").into());
    }
    if step >= 14 {
        // The upper left armor has a different silhouette and color after B loads.
        let mut yellow = 0;
        let mut blue = 0;
        let mut cyan = 0;
        for row in pixels.as_chunks::<{ 48 * 4 }>().0 {
            for &[r, g, b, _] in &row.as_chunks::<4>().0[..24] {
                yellow += usize::from(r > 100 && g > 100 && b < 100);
                blue += usize::from(r < 100 && g < 100 && b > 100);
                cyan += usize::from(r < 100 && g > 100 && b > 100);
            }
        }
        assert_eq!(
            [yellow > 0, blue > 0, cyan > 0],
            [
                (16..=20).contains(&step),
                !(16..22).contains(&step),
                step != 21
            ],
            "equipment step {step}"
        );
    }
    Ok(())
}

fn write_frame(dir: &Path, step: usize, pixels: &[u8; 48 * 24 * 4]) -> PipelineResult<()> {
    let path = dir.join(format!("frame-{step:02}.ppm"));
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(b"P6\n48 24\n255\n")?;
    for rgba in pixels.as_chunks::<4>().0 {
        file.write_all(&rgba[..3])?;
    }
    file.flush()?;
    Ok(())
}
