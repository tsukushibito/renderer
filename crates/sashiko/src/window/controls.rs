use super::{KeyCode, WindowControls, poll_gamepad};
use crate::input::{Button, DeviceInput};

pub struct WindowInput<A> {
    pub devices: DeviceInput<A>,
    pub gamepad_slot: Option<u32>,
    quit_key: KeyCode,
    quit: bool,
}

impl<A: Eq + Copy> WindowInput<A> {
    pub fn new(devices: DeviceInput<A>, quit_key: KeyCode) -> Self {
        Self {
            devices,
            gamepad_slot: Some(0),
            quit_key,
            quit: false,
        }
    }
}

impl<A: Eq + Copy> WindowControls for WindowInput<A> {
    fn key(&mut self, key: KeyCode, pressed: bool) {
        if !self.devices.focused() {
            return;
        }
        self.quit |= pressed && key == self.quit_key;
        let usage = match key {
            KeyCode::KeyA => 4,
            KeyCode::KeyB => 5,
            KeyCode::KeyC => 6,
            KeyCode::KeyD => 7,
            KeyCode::KeyE => 8,
            KeyCode::KeyF => 9,
            KeyCode::KeyG => 10,
            KeyCode::KeyH => 11,
            KeyCode::KeyI => 12,
            KeyCode::KeyJ => 13,
            KeyCode::KeyK => 14,
            KeyCode::KeyL => 15,
            KeyCode::KeyM => 16,
            KeyCode::KeyN => 17,
            KeyCode::KeyO => 18,
            KeyCode::KeyP => 19,
            KeyCode::KeyQ => 20,
            KeyCode::KeyR => 21,
            KeyCode::KeyS => 22,
            KeyCode::KeyT => 23,
            KeyCode::KeyU => 24,
            KeyCode::KeyV => 25,
            KeyCode::KeyW => 26,
            KeyCode::KeyX => 27,
            KeyCode::KeyY => 28,
            KeyCode::KeyZ => 29,
            KeyCode::Digit1 => 30,
            KeyCode::Digit2 => 31,
            KeyCode::Digit3 => 32,
            KeyCode::Digit4 => 33,
            KeyCode::Digit5 => 34,
            KeyCode::Digit6 => 35,
            KeyCode::Digit7 => 36,
            KeyCode::Digit8 => 37,
            KeyCode::Digit9 => 38,
            KeyCode::Digit0 => 39,
            KeyCode::Enter => 40,
            KeyCode::Escape => 41,
            KeyCode::Backspace => 42,
            KeyCode::Tab => 43,
            KeyCode::Space => 44,
            KeyCode::ArrowRight => 79,
            KeyCode::ArrowLeft => 80,
            KeyCode::ArrowDown => 81,
            KeyCode::ArrowUp => 82,
            _ => return,
        };
        self.devices.button(Button::Key(usage), pressed);
    }
    fn poll_devices(&mut self) {
        if self.devices.focused()
            && let Some(slot) = self.gamepad_slot
        {
            self.devices.gamepad(poll_gamepad(slot));
        }
    }
    fn mouse_button(&mut self, button: u16, down: bool) {
        self.devices.button(Button::Mouse(button), down);
    }
    fn quit_requested(&self) -> bool {
        self.quit
    }
    fn focus_lost(&mut self) {
        self.devices.focus(false);
    }
    fn focus_gained(&mut self) {
        self.devices.focus(true);
    }
}
