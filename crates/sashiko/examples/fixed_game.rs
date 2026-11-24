//! Run a game rule for a fixed number of ticks, with no window or GPU.
use sashiko::{App, input::ActionMap};
use std::num::NonZeroU32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Left,
    Right,
    Jump,
}

#[derive(Default, Debug)]
struct Player {
    x: f32,
    jumps: u32,
}

fn main() {
    use Action::*;
    let mut game = App::new(Player::default(), NonZeroU32::new(60).unwrap());
    let mut input = ActionMap::new(&[('a', Left), ('d', Right), (' ', Jump)]);
    input.button('d', true);
    input.button(' ', true);
    input.button(' ', false);
    game.advance(
        3,
        |_, _| input.sample(|map| (map.axis(Left, Right), map.pressed(Jump))),
        |player, (motion, jump)| {
            player.x += motion;
            player.jumps += u32::from(jump);
        },
    );
    assert_eq!((game.game.x, game.game.jumps), (3., 1));
    println!("{:?}", game.game);
}
