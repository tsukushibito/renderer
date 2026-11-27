#![cfg(all(
    windows,
    feature = "window-capture",
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]

#[path = "../examples/support/validation.rs"]
mod validation;

use sashiko_renderer::{
    CapturePlane, WindowRenderer,
    canvas::{Layer, Paint},
    material::Materials,
    mesh::{MeshCapacity, Meshes},
    pipeline::{
        CameraView, PipelineResult, Projection, SceneAssets, SceneInput, StandardConfig, ViewId,
        ViewInput,
    },
    unlit_forward::IDENTITY,
};
use sashiko_renderer_core::{
    PresentOutcome,
    graph::{RasterRect, SampleCount},
    runtime::RuntimeConfig,
};
use std::{
    num::NonZeroUsize,
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

#[derive(Default)]
struct Host(Option<Rc<Window>>);
impl ApplicationHandler for Host {
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        self.0.get_or_insert_with(|| {
            Rc::new(
                events
                    .create_window(
                        Window::default_attributes()
                            .with_title("Sashiko capture test")
                            .with_inner_size(PhysicalSize::new(160, 100)),
                    )
                    .unwrap(),
            )
        });
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

const WAIT: Duration = Duration::from_secs(20);

#[test]
#[ignore = "requires native GPUs for the configured backends"]
fn film_and_scaled_scene_depth_keep_their_native_readback_layouts() -> PipelineResult<()> {
    for backend in [
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(Default::default()),
        #[cfg(feature = "d3d12")]
        RuntimeConfig::D3D12(Default::default()),
    ] {
        let extent = [32, 16];
        let camera = CameraView {
            id: ViewId(1),
            extent,
            camera: ViewInput::new(
                IDENTITY,
                [0., 0., -1.],
                [0., 0., 1.],
                [1., 2.],
                Projection::Orthographic,
            )?,
        };
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let mut config = StandardConfig::new(extent);
        config.film_output = true;
        let mut renderer = sashiko_renderer::Renderer::headless_configured(
            backend,
            camera,
            config,
            SceneAssets::new(&mut meshes, &mut materials, &[]),
            NonZeroUsize::MIN,
            |_| Ok(()),
        )?;
        for scene in [[12, 8], [128, 80]] {
            renderer.set_scene_extent(
                Some(scene),
                SceneAssets::new(&mut meshes, &mut materials, &[]),
            )?;
            let mut f = renderer.begin_frame()?;
            f.draw_scene(SceneInput::new(&meshes, &materials, &[]), 1.)?;
            let depth = f.capture_depth()?;
            assert_eq!(depth.extent(), scene);
            let mut values = vec![0; (scene[0] * scene[1] * 4) as usize];
            assert!(renderer.read_capture(&depth, &mut values, WAIT)?);
            assert!(
                values
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|v| f32::from_le_bytes(*v) == 1.)
            );
            let mut f = renderer.begin_frame()?;
            f.draw_overlay(&[Layer {
                rect: RasterRect::new(0, 0, extent[0], extent[1]),
                clip: None,
                paint: Paint::Color([2., 0., 0., 1.]),
            }])?;
            let film = f.capture_linear()?;
            assert_eq!(film.extent(), extent);
            let mut linear = vec![0; (extent[0] * extent[1] * 8) as usize];
            assert!(renderer.read_capture(&film, &mut linear, WAIT)?);
            // Binary16 encodes 2.0 as 0x4000 and 1.0 as 0x3c00.
            assert!(
                linear
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .all(|v| v == &[0, 64, 0, 0, 0, 0, 0, 60])
            );
        }
        validation::check_validation(renderer.into_runtime()?)?;
    }
    Ok(())
}

#[test]
#[ignore = "requires a Windows desktop and the selected native GPUs"]
fn captured_presenter_input_survives_resize_and_slot_pressure() -> PipelineResult<()> {
    let mut events = EventLoop::builder().with_any_thread(true).build()?;
    for backend in [
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(Default::default()),
        #[cfg(feature = "d3d12")]
        RuntimeConfig::D3D12(Default::default()),
    ] {
        for samples in [SampleCount::X4, SampleCount::X1] {
            let mut host = Host::default();
            let deadline = Instant::now() + WAIT;
            while host.0.is_none() {
                events.pump_app_events(Some(Duration::ZERO), &mut host);
                assert!(Instant::now() < deadline);
            }
            let window = host.0.as_ref().unwrap().clone();
            let mut meshes = Meshes::new(MeshCapacity {
                meshes: 1,
                retired_mesh_versions: 1,
            })?;
            let mut materials = Materials::default();
            let extent: [u32; 2] = window.inner_size().into();
            let camera = CameraView {
                id: ViewId(1),
                extent,
                camera: ViewInput::new(
                    IDENTITY,
                    [0., 0., -1.],
                    [0., 0., 1.],
                    [1., 2.],
                    Projection::Orthographic,
                )?,
            };
            let mut config = StandardConfig::new(extent);
            config.samples = samples;
            let mut r = WindowRenderer::new_configured(
                window.clone(),
                backend,
                camera,
                config,
                SceneAssets::new(&mut meshes, &mut materials, &[]),
                NonZeroUsize::MIN,
                |_| Ok(()),
            )?;
            r.prepare_capture(SceneAssets::new(&mut meshes, &mut materials, &[]))?;
            if samples == SampleCount::X4 {
                assert!(
                    r.draw_with_capture(0, CapturePlane::SceneDepth, |_| panic!(
                        "unsupported depth"
                    ))
                    .is_err()
                );
            }
            let plane = if samples == SampleCount::X1 {
                CapturePlane::SceneDepth
            } else {
                CapturePlane::DisplayColor
            };
            for (index, size) in [extent, [192, 112]].into_iter().enumerate() {
                let deadline = Instant::now() + WAIT;
                if index > 0 {
                    let _ = window.request_inner_size(PhysicalSize::<u32>::from(size));
                    while <[u32; 2]>::from(window.inner_size()) != size {
                        events.pump_app_events(Some(Duration::ZERO), &mut host);
                        assert!(Instant::now() < deadline);
                    }
                    r.resize(size, SceneAssets::new(&mut meshes, &mut materials, &[]))?;
                    // Old pixels remain readable at their original dimensions after resize.
                    let image = r.take_capture()?.expect("first capture");
                    assert_eq!((image.tag, image.extent), (0, extent));
                    assert_eq!(image.plane, plane);
                    if plane == CapturePlane::SceneDepth {
                        assert_eq!(
                            image.bytes.len(),
                            extent[0] as usize * extent[1] as usize * 4
                        );
                        assert!(
                            image
                                .bytes
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .all(|v| f32::from_le_bytes(*v) == 1.0)
                        );
                    } else {
                        assert_eq!(&image.bytes[..4], &[64, 0, 0, 255]);
                        assert_eq!(&image.bytes[image.bytes.len() - 4..], &[0, 0, 255, 255]);
                    }
                }
                let mut draws = 0;
                loop {
                    events.pump_app_events(Some(Duration::ZERO), &mut host);
                    let outcome = if r.capture_available() {
                        r.draw_with_capture(index as u64, plane, |f| {
                            draws += 1;
                            f.draw_scene(SceneInput::new(&meshes, &materials, &[]), 1.)?;
                            f.draw_overlay(&[
                                Layer {
                                    rect: RasterRect::new(0, 0, size[0], size[1] / 2),
                                    clip: None,
                                    paint: Paint::Color([0.25, 0., 0., 1.]),
                                },
                                Layer {
                                    rect: RasterRect::new(0, size[1] / 2, size[0], size[1] / 2),
                                    clip: None,
                                    paint: Paint::Color([0., 0., 1., 1.]),
                                },
                            ])?;
                            Ok(())
                        })?
                    } else {
                        r.draw(|_| panic!("present retry must reuse the accepted image"))?
                    };
                    if matches!(outcome, PresentOutcome::Presented { .. }) {
                        break;
                    }
                    assert!(Instant::now() < deadline, "presentation timed out");
                }
                assert_eq!(draws, 1);
                assert!(
                    r.draw_with_capture(9, plane, |_| panic!("slot occupied"))
                        .is_err()
                );
            }
            // Shut down with the second readback still owned by the renderer.
            validation::check_validation(r.into_runtime()?)?;
        }
    }
    Ok(())
}
