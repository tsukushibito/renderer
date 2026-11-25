//! Generate a combat recording from an ordinary fresh run. Drive only the public input API.
use sashiko::math::Vec2;
use std::{
    env,
    error::Error,
    fs::File,
    io::{BufWriter, Write},
};
use survivor::{Arena, Game, Input, Phase, Scenario, Upgrade};

// Follow the service corridor, choosing between actual keyboard directions by
// the clearance of a short projected step. This uses input only.
fn steer(game: &Game, waypoint: &mut usize) -> Vec2 {
    const CORNERS: [[f32; 2]; 4] = [[15., 11.], [15., -11.], [-15., -11.], [-15., 11.]];
    let player = game.player;
    let mut target = Vec2::from_array(CORNERS[*waypoint]);
    if (player - target).length() < 3.0 {
        *waypoint = (*waypoint + 1) % 4;
        target = Vec2::from_array(CORNERS[*waypoint]);
    }
    let toward = target - player;
    let aim = toward.length().max(0.01);
    let mut best = (-f32::INFINITY, Vec2::ZERO);
    for [dx, dz] in [
        [1.0_f32, 0.],
        [-1., 0.],
        [0., 1.],
        [0., -1.],
        [1., 1.],
        [-1., 1.],
        [1., -1.],
        [-1., -1.],
    ] {
        let input = Vec2::new(dx, dz);
        let direction = input.normalize_or_zero();
        let mut cost = 0.;
        for distance in [0.9, 2.4] {
            let next = player + direction * distance;
            let [a, b] = next.to_array();
            if a.abs() > game.arena.half_extent[0] - 0.5
                || b.abs() > game.arena.half_extent[1] - 0.5
                || game.arena.obstacles.iter().any(|ob| {
                    (a - ob.center[0]).abs() < ob.half_extent[0] + 0.4
                        && (b - ob.center[1]).abs() < ob.half_extent[1] + 0.4
                })
            {
                cost += 15.;
            }
            for e in &game.enemies {
                let offset = e.pos - player;
                if offset.x().abs() > 6. || offset.y().abs() > 6. {
                    continue;
                }
                let projected = e.pos + e.facing * distance * 0.45;
                let d = (projected - next).length();
                if d < 1.8 {
                    cost += (1.8 - d) * 3.;
                }
            }
        }
        let score = direction.dot(toward) / aim - cost;
        if score > best.0 {
            best = (score, input);
        }
    }
    best.1
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args().nth(1).ok_or("expected replay output path")?;
    let mut game = Game::new(Arena::default(), Scenario::Early, 506);
    let mut tape = Vec::new();
    let mut qualifying_ticks = 0;
    let mut waypoint = 0;
    let mut movement = Vec2::new(0., 1.);
    for _ in 0..130_000 {
        if game.tick.is_multiple_of(4) {
            movement = steer(&game, &mut waypoint);
        }
        let mut input = Input {
            movement,
            ..Input::default()
        };
        let [rapid, pierce, pulse] = game.upgrades;
        input.upgrade = Some(if pulse < 2 {
            Upgrade::Pulse
        } else if rapid < 5 {
            Upgrade::Rapid
        } else if pierce < 3 {
            Upgrade::Pierce
        } else {
            Upgrade::Pulse
        });
        tape.push(input);
        game.step(input);
        if game.phase == Phase::Defeated {
            return Err(format!(
                "run ended before the overlap segment: {:?}",
                game.snapshot()
            )
            .into());
        }
        if game.enemies.len() == 500 && rapid >= 5 && pierce >= 2 && pulse > 0 {
            qualifying_ticks += 1;
        } else {
            qualifying_ticks = 0;
        }
        if qualifying_ticks == 720 {
            let mut writer = BufWriter::new(File::create(path)?);
            serde_json::to_writer(&mut writer, &tape)?;
            writer.flush()?;
            println!(
                "replay_from={} snapshot={:?}",
                tape.len() - 720,
                game.snapshot()
            );
            return Ok(());
        }
    }
    Err(format!("no qualifying segment: {:?}", game.snapshot()).into())
}
