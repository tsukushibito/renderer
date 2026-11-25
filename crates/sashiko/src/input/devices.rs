//! Device-independent controls. Keyboard numbers are USB HID keyboard usages.
//! Pad button indices and axis orientation follow the browser standard mapping.
use super::ActionMap;
use crate::math::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Key(u16),
    Mouse(u16),
    Pad(u8),
}

pub mod key {
    use super::Button;
    pub const A: Button = Button::Key(4);
    pub const B: Button = Button::Key(5);
    pub const C: Button = Button::Key(6);
    pub const D: Button = Button::Key(7);
    pub const E: Button = Button::Key(8);
    pub const F: Button = Button::Key(9);
    pub const G: Button = Button::Key(10);
    pub const H: Button = Button::Key(11);
    pub const I: Button = Button::Key(12);
    pub const J: Button = Button::Key(13);
    pub const K: Button = Button::Key(14);
    pub const L: Button = Button::Key(15);
    pub const M: Button = Button::Key(16);
    pub const N: Button = Button::Key(17);
    pub const O: Button = Button::Key(18);
    pub const P: Button = Button::Key(19);
    pub const Q: Button = Button::Key(20);
    pub const R: Button = Button::Key(21);
    pub const S: Button = Button::Key(22);
    pub const T: Button = Button::Key(23);
    pub const U: Button = Button::Key(24);
    pub const V: Button = Button::Key(25);
    pub const W: Button = Button::Key(26);
    pub const X: Button = Button::Key(27);
    pub const Y: Button = Button::Key(28);
    pub const Z: Button = Button::Key(29);
    pub const DIGIT_1: Button = Button::Key(30);
    pub const DIGIT_2: Button = Button::Key(31);
    pub const DIGIT_3: Button = Button::Key(32);
    pub const DIGIT_4: Button = Button::Key(33);
    pub const DIGIT_5: Button = Button::Key(34);
    pub const DIGIT_6: Button = Button::Key(35);
    pub const DIGIT_7: Button = Button::Key(36);
    pub const DIGIT_8: Button = Button::Key(37);
    pub const DIGIT_9: Button = Button::Key(38);
    pub const DIGIT_0: Button = Button::Key(39);
    pub const ENTER: Button = Button::Key(40);
    pub const ESCAPE: Button = Button::Key(41);
    pub const BACKSPACE: Button = Button::Key(42);
    pub const TAB: Button = Button::Key(43);
    pub const SPACE: Button = Button::Key(44);
    pub const RIGHT: Button = Button::Key(79);
    pub const LEFT: Button = Button::Key(80);
    pub const DOWN: Button = Button::Key(81);
    pub const UP: Button = Button::Key(82);
}

pub mod pad {
    use super::Button;
    pub const SOUTH: Button = Button::Pad(0);
    pub const EAST: Button = Button::Pad(1);
    pub const WEST: Button = Button::Pad(2);
    pub const NORTH: Button = Button::Pad(3);
    pub const LEFT_SHOULDER: Button = Button::Pad(4);
    pub const RIGHT_SHOULDER: Button = Button::Pad(5);
    pub const LEFT_TRIGGER: Button = Button::Pad(6);
    pub const RIGHT_TRIGGER: Button = Button::Pad(7);
    pub const BACK: Button = Button::Pad(8);
    pub const START: Button = Button::Pad(9);
    pub const LEFT_STICK: Button = Button::Pad(10);
    pub const RIGHT_STICK: Button = Button::Pad(11);
    pub const UP: Button = Button::Pad(12);
    pub const DOWN: Button = Button::Pad(13);
    pub const LEFT: Button = Button::Pad(14);
    pub const RIGHT: Button = Button::Pad(15);
}

/// Standard analog sources. Stick components use right/down as positive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PadState {
    pub buttons: u32,
    pub left: Vec2,
    pub right: Vec2,
    pub triggers: [f32; 2],
}

fn stick(value: Vec2, dead_zone: f32) -> Vec2 {
    let length = value.length();
    if !length.is_finite() || length <= dead_zone {
        return Vec2::ZERO;
    }
    value / length * ((length.min(1.) - dead_zone) / (1. - dead_zone))
}

/// One independently focusable input destination, sampled by the game once per tick.
pub struct DeviceInput<A> {
    actions: ActionMap<Button, A>,
    baseline_buttons: bool,
    suppressed: u32,
    axes: Vec<(Axis, A)>,
    pad: PadState,
    focused: bool,
    dead_zone: f32,
}

impl<A: Eq + Copy> DeviceInput<A> {
    pub fn new(bindings: &[(Button, A)]) -> Self {
        Self {
            actions: ActionMap::new(bindings),
            baseline_buttons: false,
            suppressed: 0,
            axes: Vec::new(),
            pad: PadState::default(),
            focused: true,
            dead_zone: 0.18,
        }
    }

    /// Assign continuous controls at setup. The strongest bound source wins.
    pub fn with_axes(mut self, bindings: &[(Axis, A)]) -> Self {
        self.axes = bindings.to_vec();
        self
    }

    pub fn analog(&self, action: A) -> f32 {
        let mut strongest = 0.0_f32;
        for &(source, target) in &self.axes {
            if target != action {
                continue;
            }
            let value = match source {
                Axis::LeftX => self.left_stick().x(),
                Axis::LeftY => self.left_stick().y(),
                Axis::RightX => self.right_stick().x(),
                Axis::RightY => self.right_stick().y(),
                Axis::LeftTrigger => self.triggers()[0],
                Axis::RightTrigger => self.triggers()[1],
            };
            if value.abs() > strongest.abs() {
                strongest = value;
            }
        }
        strongest
    }

    pub fn focus(&mut self, active: bool) {
        self.focused = active;
        if !active {
            self.baseline_buttons = true;
            self.suppressed = 0;
            self.actions.clear();
            self.pad = PadState::default();
        }
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn button(&mut self, source: Button, down: bool) {
        if self.focused {
            self.actions.button(source, down);
        }
    }

    pub fn set_dead_zone(&mut self, radius: f32) -> Result<(), &'static str> {
        if !(0.0..1.0).contains(&radius) {
            return Err("dead zone must be in [0, 1)");
        }
        self.dead_zone = radius;
        Ok(())
    }

    /// Supply the current pad snapshot, or None on disconnect.
    pub fn gamepad(&mut self, state: Option<PadState>) {
        if !self.focused {
            return;
        }
        let mut next = state.unwrap_or_default();
        // A held menu button must not become a new press when focus returns.
        if self.baseline_buttons {
            self.suppressed = next.buttons;
            self.baseline_buttons = false;
        }
        self.suppressed &= next.buttons;
        next.buttons &= !self.suppressed;
        let mut changed = self.pad.buttons ^ next.buttons;
        while changed != 0 {
            let index = changed.trailing_zeros() as u8;
            self.actions
                .button(Button::Pad(index), next.buttons & (1 << index) != 0);
            changed &= changed - 1;
        }
        self.pad = next;
    }

    pub fn sample<T>(&mut self, read: impl FnOnce(&ActionMap<Button, A>) -> T) -> T {
        self.actions.sample(read)
    }

    pub fn left_stick(&self) -> Vec2 {
        stick(self.pad.left, self.dead_zone)
    }
    pub fn right_stick(&self) -> Vec2 {
        stick(self.pad.right, self.dead_zone)
    }
    pub fn triggers(&self) -> [f32; 2] {
        self.pad
            .triggers
            .map(|v| if v.is_finite() { v.clamp(0., 1.) } else { 0. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disconnect_focus_and_multiple_bindings() {
        let mut input = DeviceInput::new(&[(Button::Pad(0), 1), (Button::Mouse(0), 1)]);
        let pad = PadState {
            buttons: 1,
            left: Vec2::new(0.59, 0.),
            ..Default::default()
        };
        input.gamepad(Some(pad));
        assert!((input.left_stick().x() - 0.5).abs() < 0.001);
        input.button(Button::Mouse(0), true);
        input.gamepad(None);
        input
            .actions
            .sample(|m| assert!(m.pressed(1) && m.held(1) && !m.released(1)));
        input.focus(false);
        input.gamepad(Some(pad));
        input.button(Button::Mouse(0), true);
        input.focus(true);
        assert_eq!(input.left_stick(), Vec2::ZERO);
        input
            .actions
            .sample(|m| assert!(!m.held(1) && !m.pressed(1)));
    }
}
