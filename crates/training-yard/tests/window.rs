#![cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
use renderer::WindowRenderer;
#[path = "../../renderer/examples/support/validation.rs"]
mod validation;
use renderer::{
    material::Materials,
    mesh::{MeshCapacity, Meshes},
    pbr::{Projection, ViewInput},
    pipeline::{AssetPreparation, CameraView, PipelineResult, SceneAssets, SceneInput, ViewId},
    unlit_forward::IDENTITY,
};
use renderer_core::{PresentOutcome, runtime::RuntimeConfig};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    event_loop::EventLoop,
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

#[derive(Default)]
struct Host(Option<Arc<Window>>);
impl ApplicationHandler for Host {
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        self.0.get_or_insert_with(|| {
            Arc::new(
                events
                    .create_window(
                        Window::default_attributes()
                            .with_title("Training yard")
                            .with_inner_size(PhysicalSize::new(128, 96)),
                    )
                    .unwrap(),
            )
        });
    }
}

#[test]
#[ignore = "requires a Windows desktop and a real GPU"]
fn present_before_and_after_suspension_and_resize() -> PipelineResult<()> {
    let mut events = EventLoop::builder().with_any_thread(true).build()?;
    for config in [
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(Default::default()),
        #[cfg(feature = "d3d12")]
        RuntimeConfig::D3D12(Default::default()),
    ] {
        let mut host = Host::default();
        let deadline = Instant::now() + Duration::from_secs(20);
        while host.0.is_none() {
            events.pump_app_events(Some(Duration::from_millis(1)), &mut host);
            assert!(Instant::now() < deadline, "window creation timed out");
        }
        let window = host.0.as_ref().unwrap().clone();
        let extent = window.inner_size();
        let camera = CameraView {
            id: ViewId(1),
            extent: [extent.width, extent.height],
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
        let mut view = WindowRenderer::new(
            Arc::clone(&window),
            config,
            camera,
            SceneAssets::new(&mut meshes, &mut materials, &[]),
        )?;
        let diffuse = renderer::pbr::BakedLighting::uniform([[0.4; 3]])?;
        let lighting = view.load_probe_lighting(renderer::pipeline::ProbeLighting {
            diffuse: Some(&diffuse),
            ..Default::default()
        })?;
        let mut presented = 0;
        let mut resized = false;
        let mut streaming_ready = false;
        while presented < 6 {
            if presented >= 2 && !streaming_ready {
                streaming_ready = view
                    .replace_asset_reservations(
                        "streaming texture",
                        &[],
                        &[renderer_core::graph::ImageDesc::rgba8(7, 3)],
                        &[],
                    )?
                    .is_ready();
            }
            if presented == 3 && !resized {
                window.set_minimized(true);
                view.resize([0, 0], SceneAssets::new(&mut meshes, &mut materials, &[]))?;
                loop {
                    events.pump_app_events(Some(Duration::from_millis(1)), &mut host);
                    match view.draw(|_| panic!("drawing while suspended"))? {
                        PresentOutcome::Suspended => break,
                        PresentOutcome::NotReady | PresentOutcome::Recreated => {}
                        other => panic!("unexpected minimized result: {other:?}"),
                    }
                    assert!(Instant::now() < deadline, "suspension timed out");
                }
                window.set_minimized(false);
                let requested = PhysicalSize::new(160, 112);
                let _ = window.request_inner_size(requested);
                while window.inner_size() != requested {
                    events.pump_app_events(Some(Duration::from_millis(1)), &mut host);
                    assert!(Instant::now() < deadline, "resize timed out");
                }
                view.resize(
                    [160, 112],
                    SceneAssets::new(&mut meshes, &mut materials, &[]),
                )?;
                resized = true;
            }
            events.pump_app_events(Some(Duration::from_millis(1)), &mut host);
            match view.draw(|frame| {
                frame.draw_scene(
                    SceneInput::new(&meshes, &materials, &[]).with_baked_lighting(&lighting, 0.),
                    1.,
                )?;
                Ok(())
            })? {
                PresentOutcome::Presented { .. } => presented += 1,
                PresentOutcome::NotReady | PresentOutcome::Recreated => {}
                other => panic!("unexpected presentation outcome: {other:?}"),
            }
            assert!(Instant::now() < deadline, "presentation timed out");
        }
        assert!(streaming_ready, "window starved the stream reservation");
        drop(lighting);
        validation::check_validation(view.into_runtime()?)?;
    }
    Ok(())
}

#[test]
#[ignore = "requires a Windows desktop and a real GPU"]
fn replay_streams_a_new_image_class_during_window_presentation() -> PipelineResult<()> {
    use renderer_asset::{AssetId, Catalog, Digest, ImageBlob};
    use std::{fs, path::Path, process::Command};
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/equipment/dist");
    let root = std::env::temp_dir().join(format!("renderer-stream-window-{}", std::process::id()));
    fs::create_dir_all(root.join("catalogs"))?;
    fs::create_dir_all(root.join("objects"))?;
    for object in fs::read_dir(dist.join("objects"))? {
        let object = object?;
        fs::copy(object.path(), root.join("objects").join(object.file_name()))?;
    }
    let old = fs::read_to_string(dist.join("current"))?;
    let mut catalog: Catalog =
        serde_json::from_slice(&fs::read(dist.join("catalogs").join(old.trim()))?)?;
    let wide = catalog.assets.get_mut(&AssetId::from_u128(2)).unwrap();
    let image = wide.primitives[0]
        .images
        .iter_mut()
        .flatten()
        .next()
        .unwrap();
    let mut swatch = ImageBlob::decode(&fs::read(
        root.join("objects").join(image.hash.to_string()),
    )?)?;
    assert_eq!((swatch.width, swatch.height, swatch.mip_levels), (1, 1, 1));
    swatch.width = 2;
    swatch.pixels.extend_from_within(..);
    let bytes = swatch.encode()?;
    image.hash = Digest::of(&bytes);
    image.size = bytes.len() as u64;
    fs::write(root.join("objects").join(image.hash.to_string()), bytes)?;
    let bytes = serde_json::to_vec(&catalog)?;
    let id = Digest::of(&bytes).to_string();
    fs::write(root.join("catalogs").join(&id), bytes)?;
    fs::write(root.join("current"), format!("{id}\n"))?;
    // Both the standard window and the externally presented security monitor run
    // the normal replay, including drawing old equipment, resizing and selection.
    for api in [
        #[cfg(feature = "vulkan")]
        "vulkan",
        #[cfg(feature = "d3d12")]
        "d3d12",
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_training-yard"))
            .args(["--api", api, "--mode", "window"])
            .env("TRAINING_YARD_ASSETS", &root)
            .status()?;
        assert!(
            status.success(),
            "{api} alternate catalog replay exited with {status}"
        );
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
