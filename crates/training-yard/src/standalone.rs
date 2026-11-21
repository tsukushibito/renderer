//! Standard Frame entry points shared by the interactive window and headless capture.
use crate::{Yard, camera, replay_action, validation};
use renderer::{
    Frame, Renderer,
    canvas::{Layer, Paint},
    core::{graph::RasterRect, runtime::RuntimeConfig},
    pipeline::{CameraView, PipelineResult, RenderObject},
};
use std::{io::Write, path::Path, time::Duration};

pub(crate) fn draw(
    frame: &mut Frame<'_>,
    yard: &Yard,
    camera: &CameraView,
    scratch: &mut Vec<RenderObject>,
) -> PipelineResult<()> {
    frame.draw_scene(yard.extract(camera, scratch), 1.)?;
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
    let mut renderer = Renderer::headless(config, camera, yard.assets())?;
    let mut scratch = Vec::with_capacity(4);
    let mut pixels = [0; 48 * 24 * 4];
    if let Some(dir) = output {
        std::fs::create_dir_all(dir)?;
    }
    for step in 0..7 {
        if let Some(action) = replay_action(step)
            && yard.apply(action)?
        {
            renderer.prepare_assets(yard.assets())?;
        }
        let mut frame = renderer.begin_frame()?;
        draw(&mut frame, &yard, &camera, &mut scratch)?;
        let capture = frame.capture()?;
        if !renderer.read_capture(&capture, &mut pixels, Duration::from_secs(15))? {
            return Err(format!("capture timeout at replay step {step}").into());
        }
        if pixels[..4] != [255; 4] {
            return Err(format!("HUD missing at replay step {step}").into());
        }
        if let Some(dir) = output {
            let path = dir.join(format!("frame-{step:02}.ppm"));
            let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
            file.write_all(b"P6\n48 24\n255\n")?;
            for rgba in pixels.as_chunks::<4>().0 {
                file.write_all(&rgba[..3])?;
            }
            file.flush()?;
        }
    }
    let mut runtime = renderer.into_runtime()?;
    yard.release(&mut runtime)?;
    validation::check_validation(runtime)
}
