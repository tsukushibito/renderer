//! Window events and replay feed the same yard actions.
use crate::{Action, Yard, camera, replay_action, standalone, validation};
use renderer::{
    WindowRenderer,
    core::{PresentOutcome, runtime::RuntimeConfig},
    pipeline::PipelineResult,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

#[derive(Default)]
struct Events {
    window: Option<Arc<Window>>,
    size: Option<[u32; 2]>,
    actions: std::collections::VecDeque<Action>,
    closed: bool,
    error: Option<winit::error::OsError>,
}

impl ApplicationHandler for Events {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Training yard")
            .with_inner_size(PhysicalSize::new(384, 192));
        match events.create_window(attributes) {
            Ok(window) => self.window = Some(Arc::new(window)),
            Err(error) => self.error = Some(error),
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            WindowEvent::Resized(size) => self.size = Some([size.width, size.height]),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let step = match code {
                    KeyCode::Digit1 => 1,
                    KeyCode::Digit2 => 2,
                    KeyCode::Digit3 => 3,
                    KeyCode::Digit4 => 4,
                    KeyCode::Digit5 => 5,
                    KeyCode::Digit6 => 6,
                    KeyCode::Escape => {
                        self.closed = true;
                        return;
                    }
                    _ => return,
                };
                if let Some(action) = replay_action(step) {
                    self.actions.push_back(action);
                }
            }
            _ => {}
        }
    }
}

pub fn run(config: RuntimeConfig, interactive: bool) -> PipelineResult<()> {
    let mut events = EventLoop::builder().with_any_thread(true).build()?;
    let mut input = Events::default();
    let start = Instant::now();
    let window = loop {
        events.pump_app_events(Some(Duration::from_millis(5)), &mut input);
        if let Some(error) = input.error.take() {
            return Err(error.into());
        }
        if let Some(window) = &input.window {
            break Arc::clone(window);
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err("window creation timed out".into());
        }
    };
    let size = window.inner_size();
    let mut camera = camera(1, [size.width, size.height])?;
    let mut yard = Yard::new()?;
    let mut renderer = WindowRenderer::new(Arc::clone(&window), config, camera, yard.assets())?;
    let mut objects = Vec::with_capacity(4);
    let mut step = 0;
    let mut advance = true;
    let mut last_present = Instant::now();
    while !input.closed && (interactive || step < 7) {
        events.pump_app_events(Some(Duration::from_millis(4)), &mut input);
        if let Some(extent) = input.size.take() {
            renderer.resize(extent, yard.assets())?;
            if !extent.contains(&0) {
                camera.extent = extent;
            }
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let measured = (!interactive && step >= 4).then(renderer::core::measurement::Snapshot::now);
        if advance {
            let action = if interactive {
                input.actions.pop_front()
            } else {
                replay_action(step)
            };
            if let Some(action) = action
                && yard.apply(action)?
            {
                renderer.prepare_assets(yard.assets())?;
            }
            advance = false;
        }
        match renderer.draw(|frame| standalone::draw(frame, &yard, &camera, &mut objects))? {
            PresentOutcome::Presented { .. } => {
                step += 1;
                advance = true;
                last_present = Instant::now();
            }
            PresentOutcome::RecreateRequired => {
                let size = window.inner_size();
                input.size = Some([size.width, size.height]);
            }
            _ => {}
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        if let Some(before) = measured {
            if step == 7 && !renderer.wait_idle(Duration::from_secs(15))? {
                return Err("final presentation retirement timed out".into());
            }
            let events = renderer::core::measurement::Snapshot::now().since(before);
            assert!(
                events.is_allocation_free(),
                "window replay step {step}: {events:?}"
            );
        }
        if !interactive && last_present.elapsed() > Duration::from_secs(30) {
            return Err(format!("presentation timed out at replay step {step}").into());
        }
    }
    let mut runtime = renderer.into_runtime()?;
    yard.release(&mut runtime)?;
    validation::check_validation(runtime)?;
    if !interactive && step != 7 {
        return Err("window closed during replay".into());
    }
    Ok(())
}
