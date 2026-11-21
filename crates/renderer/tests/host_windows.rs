//! Two engine-owned windows consume one runtime image, with independent lifetimes.
#![cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]

#[path = "support/validation.rs"]
mod validation;

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use renderer::{
    PresentOutcome, PresentationConfig,
    graph::{GraphPool, ImageDesc, ImportImage},
    runtime::{
        ExecutionErrorKind, ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, WindowPresenter,
        WorkingSet,
    },
};
use std::{
    num::NonZeroUsize,
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

const WAIT: Duration = Duration::from_secs(10);
struct EngineWindow(Window);
impl HasWindowHandle for EngineWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let RawWindowHandle::Win32(mut handle) = self.0.window_handle()?.as_raw() else {
            return Err(HandleError::NotSupported);
        };
        // Engines may supply only the HWND. Renderer recovers the module for Vulkan.
        handle.hinstance = None;
        // SAFETY: Only optional information was removed from the handle borrowed
        // from this live Window. The returned borrow is still tied to self.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

#[derive(Default)]
struct Host {
    windows: Vec<Rc<EngineWindow>>,
}
impl ApplicationHandler for Host {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn about_to_wait(&mut self, active: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        for x in [80, 360] {
            let attrs = Window::default_attributes()
                .with_title("Renderer host window test")
                .with_position(PhysicalPosition::new(x, 100))
                .with_inner_size(PhysicalSize::new(128, 96));
            self.windows
                .push(Rc::new(EngineWindow(active.create_window(attrs).unwrap())));
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn present(
    rt: &mut Runtime,
    window: &mut WindowPresenter,
    image: &ImportImage,
    events: &mut EventLoop<()>,
    host: &mut Host,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        events.pump_app_events(Some(Duration::ZERO), host);
        match rt.present(window, image).unwrap() {
            PresentOutcome::Presented { .. } => return,
            PresentOutcome::NotReady
            | PresentOutcome::Recreated
            | PresentOutcome::RecreateRequired => {
                window.prepare().unwrap();
            }
            PresentOutcome::Suspended => panic!("test window suspended"),
        }
        assert!(Instant::now() < deadline, "presentation timed out");
    }
}

fn exercise(config: RuntimeConfig, events: &mut EventLoop<()>, host: &mut Host) {
    let limits = RuntimeLimits::default();
    let desc = ImageDesc::rgba8(128, 96);
    let working = [ResourceClass::Image { desc, count: 1 }];
    let mut rt = Runtime::new_with_presentation(
        config,
        limits,
        WorkingSet {
            resources: &working,
            ..Default::default()
        },
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let mut pool = GraphPool::new(limits.graph, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let color = graph.image("both windows", desc).unwrap();
    graph
        .upload_image("initial color", color, &vec![192; 128 * 96 * 4])
        .unwrap();
    graph.export_image(color).unwrap();
    let receipt = rt.submit(&graph.compile().unwrap()).unwrap();
    let output = receipt.export_image(color).unwrap();
    let presentation = PresentationConfig {
        max_frames: 1,
        ..Default::default()
    };
    let [left, right] = host.windows.as_slice() else {
        panic!("two windows required")
    };
    let left = Rc::clone(left);
    let initial_owners = Rc::strong_count(&left);
    let mut first = rt.create_presenter(Rc::clone(&left), presentation).unwrap();
    let mut second = rt.create_presenter(Rc::clone(right), presentation).unwrap();
    let Err(duplicate) = rt.create_presenter(Rc::clone(&left), presentation) else {
        panic!("a native window cannot own a second live presenter");
    };
    assert_eq!(duplicate.kind(), ExecutionErrorKind::InvalidInput);
    assert_eq!(Rc::strong_count(&left), initial_owners + 1);
    present(&mut rt, &mut first, &output, events, host);
    present(&mut rt, &mut second, &output, events, host);
    assert!(first.close(WAIT).unwrap());
    drop(first);
    assert_eq!(Rc::strong_count(&left), initial_owners);
    let mut replacement = rt.create_presenter(Rc::clone(&left), presentation).unwrap();
    present(&mut rt, &mut replacement, &output, events, host);
    assert!(replacement.close(WAIT).unwrap());
    drop(replacement);
    present(&mut rt, &mut second, &output, events, host);
    assert!(second.close(WAIT).unwrap());
    drop(second);
    rt.delete(output.key()).unwrap();
    drop(rt.wait_for_frame(receipt.id(), WAIT).unwrap().unwrap());
    drop(receipt);
    validation::check_after_teardown(rt);
}

#[test]
#[ignore = "requires a Windows desktop, real GPU and each selected native API"]
fn host_windows_share_resource_registrations() {
    let mut events = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut host = Host::default();
    for config in [
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(Default::default()),
        #[cfg(feature = "d3d12")]
        RuntimeConfig::D3D12(Default::default()),
    ] {
        // Each API owns fresh native windows. Drivers can restrict reusing
        // an HWND previously associated with another presentation API.
        host.windows.clear();
        let deadline = Instant::now() + WAIT;
        while host.windows.is_empty() {
            events.pump_app_events(Some(Duration::from_millis(10)), &mut host);
            assert!(Instant::now() < deadline, "host window creation timed out");
        }
        exercise(config, &mut events, &mut host);
    }
}
