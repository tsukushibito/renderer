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
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
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
    #[cfg(feature = "live-inspector")]
    fn cursor_moved(&mut self, _point: [f64; 2]) {}
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
            #[cfg(feature = "live-inspector")]
            WindowEvent::CursorMoved { position, .. } => {
                self.controls.cursor_moved([position.x, position.y])
            }
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
    /// Run the standalone game with one event handler until exit. Retry a
    /// pending presentation without advancing the game or consuming more input.
    pub fn run<G: WindowGame<A>>(
        self,
        game: &mut G,
        limit: Option<usize>,
        timeout: Duration,
    ) -> Result<(), G::Error> {
        if limit == Some(0) {
            return Ok(());
        }
        let size = self.window().inner_size();
        let start = Instant::now();
        let mut play = PlayLoop {
            state: self.state,
            game,
            limit,
            timeout,
            extent: [size.width, size.height],
            presented: 0,
            pending: false,
            previous_present: start,
            clock: start,
            error: None,
        };
        self.events.set_control_flow(ControlFlow::Poll);
        self.events
            .run_app(&mut play)
            .map_err(Box::<dyn std::error::Error>::from)?;
        if let Some(error) = play.error {
            return Err(error);
        }
        if limit.is_some_and(|n| play.presented < n) {
            return Err(Box::<dyn std::error::Error>::from(format!(
                "window closed early after {} frames",
                play.presented
            ))
            .into());
        }
        Ok(())
    }
}

struct PlayLoop<'a, A, G: WindowGame<A>> {
    state: State<A>,
    game: &'a mut G,
    limit: Option<usize>,
    timeout: Duration,
    extent: [u32; 2],
    presented: usize,
    pending: bool,
    previous_present: Instant,
    clock: Instant,
    error: Option<G::Error>,
}

impl<A: WindowControls, G: WindowGame<A>> ApplicationHandler for PlayLoop<'_, A, G> {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        self.state.resumed(events);
    }
    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.state.window_event(events, id, event);
    }
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        if events.exiting() {
            return;
        }
        if let Err(error) = self.step(events) {
            self.error = Some(error);
            events.exit();
        }
    }
}

impl<A: WindowControls, G: WindowGame<A>> PlayLoop<'_, A, G> {
    fn step(&mut self, events: &ActiveEventLoop) -> Result<(), G::Error> {
        self.state.controls.poll_devices();
        if self.state.closed || self.state.controls.quit_requested() {
            events.exit();
            return Ok(());
        }
        if let Some(size) = self.state.resized.take() {
            if self.extent.contains(&0) {
                self.clock = Instant::now();
            }
            self.extent = size;
            self.game.resize(size, self.pending)?;
        }
        if self.extent.contains(&0) {
            events.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(16)));
            return Ok(());
        }
        events.set_control_flow(ControlFlow::Poll);
        if !self.pending {
            let start = Instant::now();
            self.game
                .update(&mut self.state.controls, start.duration_since(self.clock))?;
            self.clock = start;
            self.pending = true;
        }
        match self.game.draw()? {
            Presentation::Presented(now) => {
                self.game
                    .presented(now.duration_since(self.previous_present));
                self.previous_present = now;
                self.presented += 1;
                self.pending = false;
                if self.limit == Some(self.presented) {
                    events.exit();
                }
            }
            Presentation::Resize => {
                let size = self
                    .state
                    .window
                    .as_ref()
                    .expect("opened window")
                    .inner_size();
                self.state.resized = Some([size.width, size.height]);
            }
            Presentation::Retry => std::thread::yield_now(),
        }
        if self.limit.is_some() && self.pending && self.previous_present.elapsed() > self.timeout {
            return Err(Box::<dyn std::error::Error>::from(format!(
                "presentation timed out after {} frames",
                self.presented
            ))
            .into());
        }
        Ok(())
    }
}
