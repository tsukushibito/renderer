//! One real Win32 event loop exercises presentation and its complete teardown.
//! CPU state-machine tests remain in the implementation file.

use super::super::{
    FrameId, FrameSubmission, RuntimeConfig, RuntimeError, RuntimeErrorKind, VulkanRuntime,
};
use super::{PresentOutcome, PresentationConfig};
use ash::vk;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::graph::{
    AttachmentOps, BufferDesc, ComputeTransform, Graph, ImageDesc, ImportImage, PassHandle,
    ResourceKey,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct WindowHost {
    window: Option<Arc<Window>>,
    resized: Option<PhysicalSize<u32>>,
}

impl ApplicationHandler for WindowHost {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = Some(Arc::new(
                active
                    .create_window(
                        Window::default_attributes()
                            .with_title("Renderer Vulkan WSI acceptance")
                            .with_inner_size(PhysicalSize::new(128, 96)),
                    )
                    .expect("real Win32 window is required"),
            ));
        }
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let WindowEvent::Resized(size) = event {
            self.resized = Some(size);
        }
    }
}

fn mesh(runtime: &mut VulkanRuntime) -> (ResourceKey, ResourceKey) {
    let vertices = [
        [-0.75_f32, -0.75, 0.25, 1.0, 0.0, 0.0, 1.0],
        [0.75, -0.75, 0.25, 1.0, 0.0, 0.0, 1.0],
        [0.0, 0.75, 0.25, 1.0, 0.0, 0.0, 1.0],
    ]
    .into_iter()
    .flatten()
    .flat_map(f32::to_ne_bytes)
    .collect::<Vec<_>>();
    let indices = [0_u32, 1, 2]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    let mut graph = Graph::new();
    let vb = graph
        .buffer(
            "wsi.mesh.vertices",
            BufferDesc {
                size: vertices.len() as u64,
            },
        )
        .unwrap();
    let ib = graph
        .buffer(
            "wsi.mesh.indices",
            BufferDesc {
                size: indices.len() as u64,
            },
        )
        .unwrap();
    graph
        .upload_buffer("wsi.upload.vertices", vb, 0, vertices)
        .unwrap();
    graph
        .upload_buffer("wsi.upload.indices", ib, 0, indices)
        .unwrap();
    graph.export_buffer(vb).unwrap();
    graph.export_buffer(ib).unwrap();
    let submitted = runtime.submit_frame(&graph.compile().unwrap()).unwrap();
    (
        submitted.export_buffer(vb).unwrap().key(),
        submitted.export_buffer(ib).unwrap().key(),
    )
}

fn image_frame(
    runtime: &mut VulkanRuntime,
    mesh: (ResourceKey, ResourceKey),
    width: u32,
    height: u32,
) -> Result<(FrameSubmission, ImportImage, PassHandle), RuntimeError> {
    let mut graph = Graph::new();
    let vertices = graph
        .import_buffer("wsi.vertices", &runtime.refresh_buffer(mesh.0)?)
        .unwrap();
    let indices = graph
        .import_buffer("wsi.indices", &runtime.refresh_buffer(mesh.1)?)
        .unwrap();
    let desc = ImageDesc::rgba8(width, height);
    let color = graph.image("wsi.mesh.color", desc).unwrap();
    let depth = graph
        .image("wsi.mesh.depth", ImageDesc::depth32(width, height))
        .unwrap();
    let transformed = graph.image("wsi.custom.invert", desc).unwrap();
    let output = graph.image("wsi.output", desc).unwrap();
    graph
        .raster(
            "wsi.standard.mesh",
            vertices,
            indices,
            3,
            color,
            depth,
            AttachmentOps::CLEAR_STORE,
            AttachmentOps::CLEAR_STORE,
            [0.0, 0.0, 0.0, 1.0],
            1.0,
        )
        .unwrap();
    graph
        .compute("wsi.custom", color, transformed, ComputeTransform::Invert)
        .unwrap();
    graph
        .composite("wsi.standard.composite", transformed, output)
        .unwrap();
    graph.export_image(output).unwrap();
    let readback = graph
        .readback_image("wsi.offscreen.verify", output)
        .unwrap();
    let submitted = runtime.submit_frame(&graph.compile().unwrap())?;
    let image = submitted.export_image(output)?;
    Ok((submitted, image, readback))
}

type Reads = HashMap<FrameId, (PassHandle, u32, u32)>;

fn observe(runtime: &mut VulkanRuntime, reads: &mut Reads, verified: &mut usize) {
    while let Some(mut completed) = runtime.poll_frame().unwrap() {
        if let Some((pass, width, height)) = reads.remove(&completed.id()) {
            let bytes = completed.result.readback(pass).unwrap();
            let center = (height as usize / 2 * width as usize + width as usize / 2) * 4;
            assert_eq!(
                &bytes[center..center + 4],
                &[0, 255, 255, 255],
                "rendered mesh and custom pass"
            );
            assert_eq!(&bytes[..4], &[255; 4], "background remains defined");
            *verified += 1;
        }
    }
}

#[test]
#[ignore = "explicit real Win32 Surface/Swapchain/Graph acceptance; requires Core+SyncVal and desktop"]
fn vulkan_presentation_graph_resize_cancel_and_pending_shutdown() {
    let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        let mut builder = EventLoop::builder();
        builder.with_any_thread(true);
        let mut event_loop = builder.build().expect("Win32 event loop is required");
        let mut host = WindowHost::default();
        event_loop.pump_app_events(Some(Duration::ZERO), &mut host);
        let window = Arc::clone(host.window.as_ref().expect("resumed must create a window"));
        let (mut runtime, mut presenter) = VulkanRuntime::for_window(
            RuntimeConfig {
                execution_slots: 6,
                ..RuntimeConfig::default()
            },
            Arc::clone(&window),
            PresentationConfig::default(),
        )
        .expect("real Surface/maintenance1 presentation support is required");
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let diagnostics = Arc::clone(&runtime.diagnostics);
        crate::backends::vulkan::assert_real_validation(&diagnostics);
        let mesh = mesh(&mut runtime);
        let mut reads = Reads::new();
        let mut verified = 0;
        let start = Instant::now();
        let mut displayed = 0;
        let mut output: Option<ImportImage> = None;
        let mut resized = false;
        let mut rapid_resized = false;
        let mut suspended = false;
        let mut cancelled = false;

        while displayed < 18 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "WSI acceptance made no progress"
            );
            event_loop.pump_app_events(Some(Duration::from_millis(1)), &mut host);
            if let Some(size) = host.resized.take() {
                presenter.resize(size.width, size.height);
            }
            observe(&mut runtime, &mut reads, &mut verified);
            presenter.poll().unwrap();

            if displayed >= 4 && !resized {
                resized = true;
                let _ = window.request_inner_size(PhysicalSize::new(160, 112));
                event_loop.pump_app_events(Some(Duration::ZERO), &mut host);
                let size = window.inner_size();
                presenter.resize(size.width, size.height);
            }
            if displayed >= 8 && !rapid_resized {
                rapid_resized = true;
                for size in [(176, 120), (144, 104), (192, 128)] {
                    let _ = window.request_inner_size(PhysicalSize::new(size.0, size.1));
                    presenter.resize(size.0, size.1);
                    event_loop.pump_app_events(Some(Duration::ZERO), &mut host);
                }
            }
            match presenter.prepare().unwrap() {
                Some(PresentOutcome::Recreated) => {
                    if let Some(image) = output.take() {
                        runtime.delete(image.key()).unwrap();
                    }
                }
                Some(PresentOutcome::NotReady | PresentOutcome::Suspended) => continue,
                _ => {}
            }
            if output.is_none() {
                let (width, height) = presenter.extent().expect("visible window extent");
                match image_frame(&mut runtime, mesh, width, height) {
                    Ok((frame, image, pass)) => {
                        reads.insert(frame.id(), (pass, width, height));
                        output = Some(image);
                    }
                    Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => {
                        continue;
                    }
                    Err(error) => panic!("Graph frame failed: {error}"),
                }
            }
            let image = output.as_ref().unwrap();
            if displayed >= 12 && !suspended {
                suspended = true;
                window.set_minimized(true);
                presenter.resize(0, 0);
                assert!(matches!(
                    runtime.present(&mut presenter, image).unwrap(),
                    PresentOutcome::Suspended
                ));
                event_loop.pump_app_events(Some(Duration::ZERO), &mut host);
                window.set_minimized(false);
                let _ = window.request_inner_size(PhysicalSize::new(160, 112));
                event_loop.pump_app_events(Some(Duration::ZERO), &mut host);
                let size = window.inner_size();
                presenter.resize(size.width, size.height);
            }
            if displayed >= 2 && !cancelled {
                // The test-only hook cancels only after a successful native
                // acquire, exercising binary consumption and image release.
                presenter.cancel_next_acquire();
                cancelled = true;
            }
            match runtime.present(&mut presenter, image) {
                Ok(PresentOutcome::Presented { .. }) => {
                    runtime.delete(image.key()).unwrap();
                    output = None;
                    displayed += 1;
                }
                Ok(PresentOutcome::Recreated) => {
                    runtime.delete(image.key()).unwrap();
                    output = None;
                }
                Ok(PresentOutcome::NotReady) => {}
                Ok(PresentOutcome::Suspended) => {}
                Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => {}
                Err(error) => panic!("presentation failed: {error}"),
            }
        }
        assert!(
            !presenter.inner.as_ref().unwrap().cancel_next_acquire,
            "native acquire cancellation hook must run"
        );
        assert!(resized && rapid_resized && suspended);
        assert!(
            runtime.pending_frames() > 0,
            "close includes unobserved in-flight ownership"
        );
        let weak_window = Arc::downgrade(&window);
        host.window = None;
        drop(window);
        assert!(
            weak_window.upgrade().is_some(),
            "Presentation retains its Window owner"
        );
        assert!(
            presenter.close(WAIT).unwrap(),
            "presentation shutdown must complete"
        );
        while runtime.pending_frames() != 0 {
            observe(&mut runtime, &mut reads, &mut verified);
            assert!(start.elapsed() < Duration::from_secs(30));
        }
        assert!(
            verified >= 18,
            "headless pixel verification accompanies presentation"
        );
        assert!(reads.is_empty());
        runtime.delete(mesh.0).unwrap();
        runtime.delete(mesh.1).unwrap();
        drop(presenter);
        drop(runtime);
        assert!(
            weak_window.upgrade().is_none(),
            "retired Surface releases Window ownership"
        );
        let report = diagnostics.snapshot();
        assert!(
            report.is_clean(),
            "Core/SyncVal including complete WSI shutdown: {report:#?}"
        );
    });
}
