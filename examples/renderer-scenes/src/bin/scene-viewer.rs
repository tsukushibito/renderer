use sashiko_asset::scene::DisplayScene;
use sashiko_renderer::{
    core::{
        runtime::{RuntimeConfig, VulkanConfig},
        vulkan::ValidationMode,
    },
    pipeline::PipelineResult,
};
use std::{fs::File, io::BufWriter, path::Path, time::Duration};

#[cfg(windows)]
#[path = "scene_viewer/window.rs"]
mod window;

#[path = "scene_viewer/film.rs"]
mod film;

fn backend() -> RuntimeConfig {
    RuntimeConfig::Vulkan(VulkanConfig {
        validation: if cfg!(feature = "development-validation") {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
        ..Default::default()
    })
}

fn finish(
    scene: DisplayScene,
    mut runtime: sashiko_renderer::core::runtime::Runtime,
) -> PipelineResult<()> {
    scene.release(&mut runtime)?;
    let monitor = runtime
        .as_vulkan()
        .ok_or("expected Vulkan")?
        .validation_monitor();
    drop(runtime);
    let report = monitor.report();
    if !report.is_clean()
        || (cfg!(feature = "development-validation")
            && (report.core_validation != Some(true)
                || report.synchronization_validation != Some(true)))
    {
        return Err(format!("{report:?}").into());
    }
    Ok(())
}

fn capture(package: &Path, view: &str, output: &Path) -> PipelineResult<()> {
    let extent = [3840, 2160];
    let (scene, mut renderer) = DisplayScene::headless(package, view, extent, backend())?;
    let mut frame = renderer.begin_frame()?;
    scene.draw(&mut frame)?;
    let ticket = frame.capture()?;
    let mut rgba = vec![0; extent[0] as usize * extent[1] as usize * 4];
    if !renderer.read_capture(&ticket, &mut rgba, Duration::from_secs(120))? {
        return Err("scene capture timed out".into());
    }
    finish(scene, renderer.into_runtime()?)?;
    let mut encoder =
        png::Encoder::new(BufWriter::new(File::create(output)?), extent[0], extent[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

fn main() -> PipelineResult<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [package, view, flag, samples, exr, png] if flag == "--film" => film::capture(
            Path::new(package), view.to_str().ok_or("invalid view name")?,
            samples.to_str().ok_or("invalid film sample count")?.parse()?,
            Path::new(exr), Path::new(png),
        ),
        [package, view, flag, exr, png] if flag == "--sky" => film::sky(Path::new(package),view.to_str().ok_or("invalid view name")?,Path::new(exr),Path::new(png)),
        [package, view, flag, png] if flag == "--capture" => capture(
            Path::new(package),
            view.to_str().ok_or("invalid view name")?,
            Path::new(png),
        ),
        #[cfg(windows)]
        [package, view, flag, n] if flag == "--present-frames" => window::run(
            package.into(),
            view.to_str().ok_or("invalid view name")?.to_owned(),
            Some(
                n.to_str()
                    .ok_or("invalid frame count")?
                    .parse::<std::num::NonZeroUsize>()?
                    .get(),
            ),
        ),
        #[cfg(windows)]
        [package, view] => window::run(
            package.into(),
            view.to_str().ok_or("invalid view name")?.to_owned(),
            None,
        ),
        _ => Err(
            "usage: scene-viewer <package> <view> [--capture output.png | --film N linear.exr display.png | --sky linear.exr display.png | --present-frames N]"
                .into(),
        ),
    }
}
