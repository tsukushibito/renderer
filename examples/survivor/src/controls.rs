//! Default bindings shared by the standalone game, desktop control and browser.
use crate::{Input, Upgrade};
use sashiko::{
    input::{Axis, Button, DeviceInput, key, pad},
    math::Vec2,
    ui::MenuInput,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveX,
    MoveZ,
    Left,
    Right,
    Forward,
    Back,
    Select(Upgrade),
    Navigate(i8),
    Activate,
    Restart,
}

pub fn controls() -> DeviceInput<Action> {
    use Action::*;
    DeviceInput::new(&[
        (key::A, Left),
        (key::D, Right),
        (key::W, Forward),
        (key::S, Back),
        (key::DIGIT_1, Select(Upgrade::Rapid)),
        (key::DIGIT_2, Select(Upgrade::Pierce)),
        (key::DIGIT_3, Select(Upgrade::Pulse)),
        (key::LEFT, Navigate(-1)),
        (key::RIGHT, Navigate(1)),
        (key::ENTER, Activate),
        (key::ENTER, Restart),
        (key::R, Restart),
        (Button::Mouse(0), Activate),
        (pad::LEFT, Navigate(-1)),
        (pad::RIGHT, Navigate(1)),
        (pad::SOUTH, Activate),
        (pad::SOUTH, Restart),
    ])
    .with_axes(&[(Axis::LeftX, MoveX), (Axis::LeftY, MoveZ)])
}

pub fn sample(input: &mut DeviceInput<Action>) -> Input {
    use Action::*;
    let stick = Vec2::new(input.analog(MoveX), input.analog(MoveZ));
    input.sample(|actions| {
        let keys = Vec2::new(actions.axis(Left, Right), actions.axis(Forward, Back));
        Input {
            analog_movement: keys == Vec2::ZERO && stick != Vec2::ZERO,
            movement: if keys == Vec2::ZERO { stick } else { keys },
            upgrade: actions.latest_press(|a| if let Select(u) = a { Some(u) } else { None }),
            restart: actions.pressed(Restart),
            menu: MenuInput {
                direction: actions
                    .latest_press(|a| if let Navigate(d) = a { Some(d) } else { None })
                    .unwrap_or(0),
                activate: actions.pressed(Activate),
            },
        }
    })
}
