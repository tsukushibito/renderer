#![cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
use renderer::{
    material::Materials,
    mesh::{MeshCapacity, Meshes},
    pbr::{Projection, ViewInput},
    pipeline::{CameraView, PipelineResult, SceneAssets, SceneInput, ViewId},
    unlit_forward::IDENTITY,
};
use renderer_core::{PresentOutcome, runtime::RuntimeConfig};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use training_yard::window::WindowView;
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
fn standalone_empty_scene_presents_consecutive_frames() -> PipelineResult<()> {
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
        let mut view = WindowView::new(
            window,
            config,
            camera,
            SceneAssets::new(&mut meshes, &mut materials, &[]),
        )?;
        let mut presented = 0;
        while presented < 3 {
            events.pump_app_events(Some(Duration::from_millis(1)), &mut host);
            match view.draw(SceneInput::new(&meshes, &materials, &[]))? {
                PresentOutcome::Presented { .. } => presented += 1,
                PresentOutcome::NotReady | PresentOutcome::Recreated => {}
                other => panic!("unexpected presentation outcome: {other:?}"),
            }
            assert!(Instant::now() < deadline, "presentation timed out");
        }
    }
    Ok(())
}
