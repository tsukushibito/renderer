//! Generate a combat recording from an ordinary fresh run. Drive only the public input API.
use std::{
    env,
    error::Error,
    fs::File,
    io::{BufWriter, Write},
};
use survivor::{Arena, Game, Input, Phase, Scenario, Upgrade};

// Follow the service corridor, choosing between actual keyboard directions by
// the clearance of a short projected step. This uses input only.
fn steer(game: &Game, waypoint: &mut usize) -> [f32; 2] {
    const CORNERS: [[f32; 2]; 4] = [[15., 11.], [15., -11.], [-15., -11.], [-15., 11.]];
    let [x, z] = game.player;
    let mut target = CORNERS[*waypoint];
    if (x - target[0]).hypot(z - target[1]) < 3.0 {
        *waypoint = (*waypoint + 1) % 4;
        target = CORNERS[*waypoint];
    }
    let mut best = (-f32::INFINITY, [0., 0.]);
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
        let len = dx.hypot(dz);
        let [u, v] = [dx / len, dz / len];
        let mut cost = 0.;
        for distance in [0.9, 2.4] {
            let [a, b] = [x + distance * u, z + distance * v];
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
                if (e.pos[0] - x).abs() > 6. || (e.pos[1] - z).abs() > 6. {
                    continue;
                }
                let ex = e.pos[0] + e.facing[0] * distance * 0.45;
                let ez = e.pos[1] + e.facing[1] * distance * 0.45;
                let d = (ex - a).hypot(ez - b);
                if d < 1.8 {
                    cost += (1.8 - d) * 3.;
                }
            }
        }
        let aim = (target[0] - x).hypot(target[1] - z).max(0.01);
        let score = (u * (target[0] - x) + v * (target[1] - z)) / aim - cost;
        if score > best.0 {
            best = (score, [dx, dz]);
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
    let mut movement = [0., 1.];
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
