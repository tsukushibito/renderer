use sashiko::window::{KeyCode, Keyboard};
use survivor::{Input, Upgrade};

pub(super) const PLAY_TITLE: &str =
    "Sashiko | Containment Lab    WASD move    Auto-fire    Gather samples to upgrade    Esc quit";

#[derive(Default)]
pub(super) struct Controls {
    // Horizontal axes match the top-down camera (forward is negative Z).
    keys: [bool; 4],
    choice: Option<Upgrade>,
    restart: bool,
    menu: sashiko::ui::MenuInput,
    pub closed: bool,
}

impl Controls {
    pub fn next_input(&mut self) -> Input {
        let [left, right, forward, back] = self.keys.map(u8::from).map(f32::from);
        Input {
            movement: [right - left, back - forward],
            upgrade: self.choice.take(),
            menu: std::mem::take(&mut self.menu),
            restart: std::mem::take(&mut self.restart),
        }
    }
}

impl Keyboard for Controls {
    fn quit_requested(&self) -> bool {
        self.closed
    }
    fn focus_lost(&mut self) {
        self.menu = Default::default();
        self.keys.fill(false);
        self.choice = None;
        self.restart = false;
    }
    fn key(&mut self, code: KeyCode, pressed: bool) {
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
}
