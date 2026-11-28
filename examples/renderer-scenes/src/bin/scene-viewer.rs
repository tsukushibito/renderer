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

#[path = "scene_viewer/route.rs"]
mod route;

#[path = "scene_viewer/film.rs"]
mod film;

#[cfg(feature = "scene-film")]
#[path = "scene_viewer/tracing.rs"]
mod tracing;

static BACKEND: std::sync::OnceLock<RuntimeConfig> = std::sync::OnceLock::new();

fn backend() -> RuntimeConfig {
    *BACKEND.get().expect("viewer backend selected at startup")
}

fn select_backend(name: &std::ffi::OsStr) -> PipelineResult<RuntimeConfig> {
    if name == "vulkan" {
        return Ok(RuntimeConfig::Vulkan(VulkanConfig {
            validation: if cfg!(feature = "development-validation") {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
            ..Default::default()
        }));
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if name == "d3d12" {
        return Ok(RuntimeConfig::D3D12(Default::default()));
    }
    Err(format!("unavailable scene-viewer API: {name:?}").into())
}

fn finish(
    scene: DisplayScene,
    mut runtime: sashiko_renderer::core::runtime::Runtime,
) -> PipelineResult<()> {
    scene.release(&mut runtime)?;
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(native) = runtime.as_d3d12() {
        let monitor = native.validation_monitor();
        drop(runtime);
        let report = monitor.snapshot();
        let strict = cfg!(feature = "development-validation");
        if !report.is_clean()
            || (strict
                && !(report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
                    && report.teardown_checked))
        {
            return Err(format!("D3D12 validation: {report:?}").into());
        }
        return Ok(());
    }
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

fn capture(package: &Path, view: &str, extent: [u32; 2], output: &Path) -> PipelineResult<()> {
    let (mut scene, mut renderer) = DisplayScene::headless(package, view, extent, backend())?;
    let pose = scene.settings.views[view];
    scene.select_lods(&pose, extent[1]);
    let mut frame = renderer.begin_frame()?;
    scene.draw(&mut frame)?;
    let ticket = frame.capture()?;
    let mut rgba = vec![0; extent[0] as usize * extent[1] as usize * 4];
    if !renderer.read_capture(&ticket, &mut rgba, Duration::from_secs(120))? {
        return Err("scene capture timed out".into());
    }
    finish(scene, renderer.into_runtime()?)?;
    write_capture(File::create(output)?, extent, &rgba)
}

fn write_capture(file: File, extent: [u32; 2], rgba: &[u8]) -> PipelineResult<()> {
    let mut encoder = png::Encoder::new(BufWriter::new(file), extent[0], extent[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

fn visibility(package: &Path, name: &str) -> PipelineResult<()> {
    use sashiko_asset::{
        ObjectStore,
        scene::{ScenePackage, SceneSpatialIndex},
    };
    use sashiko_renderer::pbr::{ShadowSettings, Sun};
    let scene = ScenePackage::open(package)?;
    let index = SceneSpatialIndex::new(&scene, &ObjectStore::new(package), 8.)?;
    let pose = scene.view(name)?.camera([3840, 2160], [0.02, 1500.])?;
    let s = &scene.settings;
    let sun = Sun::from_lux(s.sun_direction, s.sun_color, s.sun_lux)?;
    // Count directional candidates even for an IBL-only authoring recipe, so
    // switching the sun on can be compared with exactly the same spatial bins.
    let view = pose.camera.culling(
        index.bounds(),
        sun,
        Some(ShadowSettings {
            resolution: s.shadow_resolution,
            distance: s.shadow_distance,
            split_lambda: s
                .shadow_split_lambda
                .unwrap_or(ShadowSettings::default().split_lambda),
            ..Default::default()
        }),
    )?;
    let mut camera = [0u64; 2];
    let mut cascades = [[0u64; 2]; 4];
    let mut offscreen_casters = 0;
    index.visit(&view, |p| {
        if p.camera {
            camera[0] += 1;
            camera[1] += u64::from(p.triangles);
        } else {
            offscreen_casters += 1;
        }
        for (sum, inside) in cascades.iter_mut().zip(p.cascades) {
            if inside {
                sum[0] += 1;
                sum[1] += u64::from(p.triangles);
            }
        }
    });
    println!(
        "{}",
        serde_json::json!({
            "unique_meshes":index.unique_meshes,"unique_triangles":index.unique_triangles,
            "placements":scene.placements.len(),"placed_triangles":index.placed_triangles,
            "cells":index.cell_count(),"cell_metres":8,"camera_placements_triangles":camera,
            "cascade_placements_triangles":cascades,"offscreen_caster_candidates":offscreen_casters,
        })
    );
    Ok(())
}

fn film_rect(arg: &std::ffi::OsStr) -> PipelineResult<[u32; 4]> {
    let values: Vec<u32> = arg
        .to_str()
        .ok_or("invalid film rectangle")?
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    values
        .try_into()
        .map_err(|_| "expected x,y,width,height".into())
}

fn main() -> PipelineResult<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (api, args) = match args.as_slice() {
        [flag, api, rest @ ..] if flag == "--api" => (api.as_os_str(), rest),
        rest => (std::ffi::OsStr::new("vulkan"), rest),
    };
    BACKEND.set(select_backend(api)?).expect("single startup");
    match args {
        #[cfg(feature = "scene-film")]
        [package, view, flag, samples, rect, png] if flag == "--coverage" => {
            tracing::capture(Path::new(package),view.to_str().ok_or("invalid view name")?,
                samples.to_str().ok_or("invalid sample count")?.parse()?,
                film_rect(rect)?, tracing::Output::Coverage(Path::new(png)))
        },
        #[cfg(feature = "scene-film")]
        [package, view, flag, samples, rect, exr, png] if matches!(flag.to_str(), Some("--radiance" | "--direct" | "--indirect" | "--unshadowed")) => {
            use sashiko_lighting_bake::FilmLighting;
            let lighting = match flag.to_str() {
                Some("--direct") => FilmLighting::Direct,
                Some("--indirect") => FilmLighting::Indirect,
                Some("--unshadowed") => FilmLighting::Unshadowed,
                _ => FilmLighting::Combined,
            };
            tracing::capture(Path::new(package),view.to_str().ok_or("invalid view name")?,
                samples.to_str().ok_or("invalid sample count")?.parse()?,
                film_rect(rect)?, tracing::Output::Radiance {lighting, exr: Path::new(exr), png: Path::new(png)})
        },
        [package, view, flag, samples, rect, exr, png] if flag == "--film-crop" => {
            film::capture(Path::new(package),view.to_str().ok_or("invalid view name")?,
                samples.to_str().ok_or("invalid film sample count")?.parse()?,
                Some(film_rect(rect)?),
                Path::new(exr),Path::new(png))
        },
        [package, view, flag] if flag == "--visibility" => visibility(Path::new(package), view.to_str().ok_or("invalid view name")?),
        [package, view, flag, samples, exr, png] if flag == "--film" => film::capture(
            Path::new(package), view.to_str().ok_or("invalid view name")?,
            samples.to_str().ok_or("invalid film sample count")?.parse()?, None,
            Path::new(exr), Path::new(png),
        ),
        [package, view, flag, exr, png] if flag == "--sky" => film::sky(Path::new(package),view.to_str().ok_or("invalid view name")?,Path::new(exr),Path::new(png)),
        [package, view, flag, png] if flag == "--capture-1080p" => capture(
            Path::new(package),
            view.to_str().ok_or("invalid view name")?,
            [1920, 1080],
            Path::new(png),
        ),
        [package, view, flag, png] if flag == "--capture" => capture(
            Path::new(package),
            view.to_str().ok_or("invalid view name")?,
            [3840, 2160],
            Path::new(png),
        ),
        [package, view, flag, route_path, fps] if flag == "--route-rgba" => route::record(
            Path::new(package), view.to_str().ok_or("invalid view name")?,
            Path::new(route_path),fps.to_str().ok_or("invalid frame rate")?.parse()?),
        #[cfg(windows)]
        [package, view, flag, route_path] if flag == "--walk-route" => window::walk(
            package.into(), view.to_str().ok_or("invalid view name")?.to_owned(), route::Route::load(Path::new(route_path))?),
        #[cfg(windows)]
        [package, view, flag, route_path, report] if flag == "--benchmark-route" => window::benchmark(
            package.into(),view.to_str().ok_or("invalid view name")?.to_owned(),
            route::Route::load(Path::new(route_path))?,report.into()),
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
            "usage: scene-viewer [--api vulkan|d3d12] <package> <view> [--capture output.png | --capture-1080p output.png | --walk-route route.json | --benchmark-route route.json report.json | --route-rgba route.json fps | --film N linear.exr display.png | --film-crop N x,y,width,height linear.exr display.png | --sky linear.exr display.png | --visibility | --present-frames N]; scene-film also supports --coverage N x,y,width,height mask.png and --radiance N x,y,width,height linear.exr display.png"
                .into(),
        ),
    }
}
