use sashiko::{
    ui::MenuInput,
    window::{KeyCode, WindowInput},
};
use survivor::{Input, Upgrade};

pub(super) const PLAY_TITLE: &str =
    "Sashiko | Containment Lab    WASD move    Auto-fire    Gather samples to upgrade    Esc quit";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Left,
    Right,
    Forward,
    Back,
    Select(Upgrade),
    Navigate(i8),
    Activate,
    Restart,
}

pub(super) type Controls = WindowInput<Action>;

pub(super) fn controls() -> Controls {
    use Action::*;
    use KeyCode::{
        ArrowLeft, ArrowRight, Digit1, Digit2, Digit3, Enter, Escape, KeyA, KeyD, KeyR, KeyS, KeyW,
    };
    WindowInput::new(
        &[
            (KeyA, Left),
            (KeyD, Right),
            (KeyW, Forward),
            (KeyS, Back),
            (Digit1, Select(Upgrade::Rapid)),
            (Digit2, Select(Upgrade::Pierce)),
            (Digit3, Select(Upgrade::Pulse)),
            (ArrowLeft, Navigate(-1)),
            (ArrowRight, Navigate(1)),
            (Enter, Activate),
            (Enter, Restart),
            (KeyR, Restart),
        ],
        Escape,
    )
}

pub(super) fn next_input(controls: &mut Controls) -> Input {
    use Action::*;
    controls.actions.sample(|actions| Input {
        movement: sashiko::math::Vec2::new(actions.axis(Left, Right), actions.axis(Forward, Back)),
        upgrade: actions.latest_press(|a| if let Select(u) = a { Some(u) } else { None }),
        restart: actions.pressed(Restart),
        menu: MenuInput {
            direction: actions
                .latest_press(|a| if let Navigate(d) = a { Some(d) } else { None })
                .unwrap_or(0),
            activate: actions.pressed(Activate),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko::window::Keyboard;

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
