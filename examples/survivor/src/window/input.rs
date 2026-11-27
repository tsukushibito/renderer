use sashiko::window::{KeyCode, WindowInput};
use survivor::{
    Input,
    controls::{self, Action},
};
pub(super) const PLAY_TITLE: &str =
    "Sashiko | Containment Lab    WASD / left stick move    Auto-fire    Esc quit";
#[cfg(not(feature = "diagnostics"))]
pub(super) type Controls = WindowInput<Action>;
#[cfg(feature = "diagnostics")]
pub(super) struct Controls {
    inner: WindowInput<Action>,
    pub report: bool,
    report_held: bool,
    pub video: bool,
    video_held: bool,
}
#[cfg(feature = "diagnostics")]
impl std::ops::Deref for Controls {
    type Target = WindowInput<Action>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
#[cfg(feature = "diagnostics")]
impl std::ops::DerefMut for Controls {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
#[cfg(feature = "diagnostics")]
impl sashiko::window::WindowControls for Controls {
    fn key(&mut self, key: KeyCode, pressed: bool) {
        if key == KeyCode::F8 {
            self.report |= pressed && !self.report_held;
            self.report_held = pressed;
        } else if key == KeyCode::F9 {
            self.video |= pressed && !self.video_held;
            self.video_held = pressed;
        } else {
            self.inner.key(key, pressed);
        }
    }
    fn focus_gained(&mut self) {
        self.inner.focus_gained();
    }
    fn focus_lost(&mut self) {
        self.report_held = false;
        self.video_held = false;
        self.inner.focus_lost();
    }
    fn mouse_button(&mut self, button: u16, pressed: bool) {
        self.inner.mouse_button(button, pressed);
    }
    fn poll_devices(&mut self) {
        self.inner.poll_devices();
    }
    fn quit_requested(&self) -> bool {
        self.inner.quit_requested()
    }
}
pub(super) fn controls() -> Controls {
    let inner = WindowInput::new(controls::controls(), KeyCode::Escape);
    #[cfg(feature = "diagnostics")]
    return Controls {
        inner,
        report: false,
        report_held: false,
        video: false,
        video_held: false,
    };
    #[cfg(not(feature = "diagnostics"))]
    inner
}
pub(super) fn next_input(input: &mut Controls) -> Input {
    controls::sample(&mut input.devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko::window::WindowControls;
    use survivor::Upgrade;

    #[cfg(feature = "diagnostics")]
    #[test]
    fn report_key_latches_once_until_release() {
        let mut keys = controls();
        keys.key(KeyCode::F8, true);
        assert!(std::mem::take(&mut keys.report));
        keys.key(KeyCode::F8, true);
        assert!(!keys.report);
        keys.key(KeyCode::F8, false);
        keys.key(KeyCode::F8, true);
        assert!(keys.report);
    }

    #[test]
    fn fixed_ticks_preserve_held_motion_and_consume_last_selection_once() {
        let mut keys = controls();
        for key in [
            KeyCode::KeyW,
            KeyCode::Digit1,
            KeyCode::Digit3,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
            KeyCode::Enter,
        ] {
            keys.key(key, true);
        }
        keys.key(KeyCode::Enter, false);
        let tick = next_input(&mut keys);
        assert_eq!(tick.movement.to_array(), [0., -1.]);
        assert_eq!(tick.upgrade, Some(Upgrade::Pulse));
        assert!(tick.restart && tick.menu.activate);
        assert_eq!(tick.menu.direction, 1);
        let after = next_input(&mut keys);
        assert_eq!(after.movement.to_array(), [0., -1.]);
        assert!(!after.restart && !after.menu.activate && after.upgrade.is_none());
        keys.focus_lost();
        assert_eq!(next_input(&mut keys).movement.to_array(), [0., 0.]);
    }
}
