//! Native Windows event host shared by standalone games and embedded previews.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
pub use winit::keyboard::KeyCode;
pub use winit::window::Window as NativeWindow;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::PhysicalKey,
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

mod child;
pub use child::HostChild;
mod controls;
pub use controls::WindowInput;
mod gamepad;
pub use gamepad::poll_gamepad;

pub trait WindowControls {
    fn focus_gained(&mut self) {}
    fn mouse_button(&mut self, _button: u16, _pressed: bool) {}
    fn poll_devices(&mut self) {}
    fn key(&mut self, key: KeyCode, pressed: bool);
    fn focus_lost(&mut self) {}
    fn quit_requested(&self) -> bool {
        false
    }
}

pub struct WindowHost<A> {
    events: EventLoop<()>,
    state: State<A>,
}

struct State<A> {
    title: String,
    initial: [u32; 2],
    controls: A,
    window: Option<Arc<Window>>,
    resized: Option<[u32; 2]>,
    closed: bool,
    error: Option<winit::error::OsError>,
}

impl<A: WindowControls> ApplicationHandler for State<A> {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let [w, h] = self.initial;
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(PhysicalSize::new(w, h));
        match events.create_window(attrs) {
            Ok(window) => self.window = Some(Arc::new(window)),
            Err(error) => self.error = Some(error),
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            WindowEvent::Resized(size) => self.resized = Some([size.width, size.height]),
            WindowEvent::Focused(true) => self.controls.focus_gained(),
            WindowEvent::MouseInput { state, button, .. } => {
                use winit::event::MouseButton::*;
                let button = match button {
                    Left => 0,
                    Middle => 1,
                    Right => 2,
                    Back => 3,
                    Forward => 4,
                    Other(n) => n.saturating_add(5),
                };
                self.controls
                    .mouse_button(button, state == ElementState::Pressed);
            }
            WindowEvent::Focused(false) => self.controls.focus_lost(),
            WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.controls
                        .key(code, event.state == ElementState::Pressed);
                }
            }
            _ => {}
        }
    }
}

impl<A: WindowControls> WindowHost<A> {
    pub fn open(
        title: &str,
        extent: [u32; 2],
        controls: A,
        timeout: Duration,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut host = Self {
            events: EventLoop::builder().with_any_thread(true).build()?,
            state: State {
                title: title.to_owned(),
                initial: extent,
                controls,
                window: None,
                resized: None,
                closed: false,
                error: None,
            },
        };
        let start = Instant::now();
        loop {
            host.pump(Duration::from_millis(5));
            if let Some(error) = host.state.error.take() {
                return Err(error.into());
            }
            if host.state.closed || start.elapsed() > timeout {
                return Err("window creation interrupted or timed out".into());
            }
            if host.state.window.is_some() {
                return Ok(host);
            }
        }
    }

    pub fn pump(&mut self, timeout: Duration) {
        self.events.pump_app_events(Some(timeout), &mut self.state);
        self.state.controls.poll_devices();
    }
    pub fn input(&mut self) -> &mut A {
        &mut self.state.controls
    }
    pub fn closed(&self) -> bool {
        self.state.closed
    }
    pub fn window(&self) -> Arc<Window> {
        Arc::clone(self.state.window.as_ref().expect("opened window"))
    }
    pub fn take_resize(&mut self) -> Option<[u32; 2]> {
        self.state.resized.take()
    }
    pub fn pending_resize(&self) -> bool {
        self.state.resized.is_some()
    }
    pub fn queue_resize(&mut self, size: [u32; 2]) {
        self.state.resized = Some(size);
    }
}

/// Actions the host takes after attempting to present the current state.
pub enum Presentation {
    /// The call succeeded at this instant (before optional diagnostics).
    Presented(Instant),
    Retry,
    Resize,
}

pub trait WindowGame<A> {
    type Error: From<Box<dyn std::error::Error>>;
    fn resize(&mut self, size: [u32; 2], retrying: bool) -> Result<(), Self::Error>;
    fn update(&mut self, controls: &mut A, elapsed: Duration) -> Result<(), Self::Error>;
    fn draw(&mut self) -> Result<Presentation, Self::Error>;
    fn presented(&mut self, interval: Duration);
}

impl<A: WindowControls> WindowHost<A> {
    /// One update batch per accepted frame. A pending frame can be resized or
    /// redrawn without consuming further inputs. Minimized time is suspended.
    pub fn run<G: WindowGame<A>>(
        &mut self,
        game: &mut G,
        limit: Option<usize>,
        timeout: Duration,
    ) -> Result<(), G::Error> {
        let size = self.window().inner_size();
        let mut extent = [size.width, size.height];
        let mut presented = 0;
        let mut pending = false;
        let mut previous_present = Instant::now();
        let mut clock = previous_present;
        while limit.is_none_or(|n| presented < n) {
            self.pump(Duration::ZERO);
            if self.closed() || self.state.controls.quit_requested() {
                break;
            }
            if let Some(size) = self.take_resize() {
                extent = size;
                game.resize(size, pending)?;
            }
            if extent.contains(&0) {
                self.pump(Duration::from_millis(16));
                clock = Instant::now();
                continue;
            }
            if !pending {
                let start = Instant::now();
                game.update(self.input(), start.duration_since(clock))?;
                clock = start;
                pending = true;
            }
            match game.draw()? {
                Presentation::Presented(now) => {
                    game.presented(now.duration_since(previous_present));
                    previous_present = now;
                    presented += 1;
                    pending = false;
                }
                Presentation::Resize => {
                    let size = self.window().inner_size();
                    self.queue_resize([size.width, size.height]);
                }
                Presentation::Retry => std::thread::yield_now(),
            }
            if limit.is_some() && pending && previous_present.elapsed() > timeout {
                return Err(Box::<dyn std::error::Error>::from(format!(
                    "presentation timed out after {presented} frames"
                ))
                .into());
            }
        }
        if limit.is_some_and(|n| presented < n) {
            return Err(Box::<dyn std::error::Error>::from(format!(
                "window closed early after {presented} frames"
            ))
            .into());
        }
        Ok(())
    }
}
