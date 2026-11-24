use std::sync::Arc;

use survivor::{Input, Upgrade};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

pub(super) const PLAY_TITLE: &str =
    "Sashiko | Containment Lab    WASD move    Auto-fire    Gather samples to upgrade    Esc quit";

pub(super) struct Controls {
    initial_extent: [u32; 2],
    // Horizontal axes match the top-down camera (forward is negative Z).
    keys: [bool; 4],
    choice: Option<Upgrade>,
    restart: bool,
    menu: sashiko::ui::MenuInput,
    pub window: Option<Arc<Window>>,
    pub resize: Option<[u32; 2]>,
    pub closed: bool,
    pub error: Option<winit::error::OsError>,
}

impl Controls {
    pub fn new(initial_extent: [u32; 2]) -> Self {
        Self {
            initial_extent,
            keys: [false; 4],
            choice: None,
            restart: false,
            menu: Default::default(),
            window: None,
            resize: None,
            closed: false,
            error: None,
        }
    }

    pub fn next_input(&mut self) -> Input {
        let [left, right, forward, back] = self.keys.map(u8::from).map(f32::from);
        Input {
            movement: [right - left, back - forward],
            upgrade: self.choice.take(),
            restart: std::mem::take(&mut self.restart),
            menu: std::mem::take(&mut self.menu),
        }
    }
}

impl ApplicationHandler for Controls {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let [w, h] = self.initial_extent;
        let attributes = Window::default_attributes()
            .with_title(PLAY_TITLE)
            .with_inner_size(PhysicalSize::new(w, h));
        match events.create_window(attributes) {
            Ok(window) => self.window = Some(Arc::new(window)),
            Err(error) => self.error = Some(error),
        }
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            WindowEvent::Resized(size) => self.resize = Some([size.width, size.height]),
            WindowEvent::Focused(false) => {
                self.keys.fill(false);
                self.restart = false;
                self.choice = None;
                self.menu = Default::default();
            }
            WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let pressed = event.state == ElementState::Pressed;
                match code {
                    KeyCode::KeyA => self.keys[0] = pressed,
                    KeyCode::KeyD => self.keys[1] = pressed,
                    KeyCode::KeyW => self.keys[2] = pressed,
                    KeyCode::KeyS => self.keys[3] = pressed,
                    KeyCode::Digit1 if pressed => self.choice = Some(Upgrade::Rapid),
                    KeyCode::Digit2 if pressed => self.choice = Some(Upgrade::Pierce),
                    KeyCode::Digit3 if pressed => self.choice = Some(Upgrade::Pulse),
                    KeyCode::ArrowLeft if pressed => self.menu.direction = -1,
                    KeyCode::ArrowRight if pressed => self.menu.direction = 1,
                    KeyCode::Enter if pressed => {
                        self.menu.activate = true;
                        self.restart = true;
                    }
                    KeyCode::KeyR if pressed => self.restart = true,
                    KeyCode::Escape if pressed => self.closed = true,
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
