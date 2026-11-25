use sashiko::window::{KeyCode, WindowInput};
use survivor::{
    Input,
    controls::{self, Action},
};
pub(super) const PLAY_TITLE: &str =
    "Sashiko | Containment Lab    WASD / left stick move    Auto-fire    Esc quit";
pub(super) type Controls = WindowInput<Action>;
pub(super) fn controls() -> Controls {
    WindowInput::new(controls::controls(), KeyCode::Escape)
}
pub(super) fn next_input(input: &mut Controls) -> Input {
    controls::sample(&mut input.devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko::window::WindowControls;
    use survivor::Upgrade;

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
