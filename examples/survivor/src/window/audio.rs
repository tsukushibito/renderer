//! Bind research-facility gameplay outcomes to shared audio and lifetime APIs.
use sashiko::{
    audio::{Bus, Clip, Output},
    effects::Effects,
};
use std::num::NonZeroU64;
use survivor::{Game, MAX_ENEMIES, Phase};

const FOOTSTEP: u64 = u64::MAX - 1;
const PLAYER: u64 = u64::MAX;

pub(super) struct Soundscape {
    output: Output,
    warnings: Effects<u64, ()>,
    warning: Clip,
    footstep: Clip,
    damage: Clip,
    knockout: Clip,
    defeat: Clip,
    hp: u32,
    kills: u32,
    phase: Phase,
}

impl Soundscape {
    pub fn open(volume: f32) -> Option<Self> {
        let output = Output::open(64)
            .map_err(|err| eprintln!("game audio unavailable: {err}"))
            .ok()?;
        Some(Self::with_output(output, volume))
    }
    fn with_output(output: Output, volume: f32) -> Self {
        output.edit(|mixer| mixer.set_master(volume));
        Self {
            output,
            warnings: Effects::new(MAX_ENEMIES),
            warning: Clip::from_wav(
                &include_bytes!("../../../../assets/survivor/sounds/warning.wav")[..],
            )
            .expect("warning cue"),
            footstep: Clip::from_wav(
                &include_bytes!("../../../../assets/survivor/sounds/footstep.wav")[..],
            )
            .expect("footstep cue"),
            damage: Clip::from_wav(
                &include_bytes!("../../../../assets/survivor/sounds/damage.wav")[..],
            )
            .expect("damage cue"),
            knockout: Clip::from_wav(
                &include_bytes!("../../../../assets/survivor/sounds/knockout.wav")[..],
            )
            .expect("knockout cue"),
            defeat: Clip::from_wav(
                &include_bytes!("../../../../assets/survivor/sounds/defeat.wav")[..],
            )
            .expect("defeat cue"),
            hp: Game::MAX_HP,
            kills: 0,
            phase: Phase::Playing,
        }
    }

    pub fn update(&mut self, game: &Game, footsteps: usize) {
        self.output.edit(|mixer| {
            self.warnings.update(game.tick, |id, _| {
                let keep = game.phase == Phase::Playing
                    && game.enemies.iter().any(|e| e.id == *id && e.windup);
                if !keep {
                    mixer.stop(*id);
                }
                keep
            });
            if game.phase == Phase::Playing {
                for enemy in game.enemies.iter().filter(|e| e.windup) {
                    if self.warnings.active().any(|(id, _)| *id == enemy.id) {
                        continue;
                    }
                    let offset = [
                        enemy.pos[0] - game.player[0],
                        0.,
                        enemy.pos[1] - game.player[1],
                    ];
                    if mixer.play(enemy.id, &self.warning, Bus::Effects, offset, false) {
                        // A warning remains latched until the windup ends.
                        let _ = self
                            .warnings
                            .start(enemy.id, game.tick, NonZeroU64::MAX, ());
                    }
                }
            }
            for _ in 0..footsteps {
                if !mixer.play(FOOTSTEP, &self.footstep, Bus::Effects, [0.; 3], false) {
                    break;
                }
            }
            let cue = if game.phase == Phase::Defeated && self.phase != Phase::Defeated {
                Some(&self.defeat)
            } else if game.hp < self.hp {
                Some(&self.damage)
            } else if game.kills > self.kills {
                Some(&self.knockout)
            } else {
                None
            };
            if let Some(clip) = cue {
                mixer.stop(PLAYER);
                mixer.play(PLAYER, clip, Bus::Effects, [0.; 3], false);
            }
            if self.phase == Phase::Defeated && game.phase != Phase::Defeated {
                mixer.stop(PLAYER);
            }
        });
        self.hp = game.hp;
        self.kills = game.kills;
        self.phase = game.phase;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an active Windows audio endpoint"]
    fn contact_plays_the_footstep_on_the_default_endpoint() {
        let mut sound =
            Soundscape::with_output(Output::open(1).expect("default audio output"), 0.2);
        let game = Game::new(Default::default(), survivor::Scenario::Early, 506);
        sound.update(&game, 1);
        let clip = &sound.footstep;
        assert!(
            !sound
                .output
                .edit(|m| m.play(42, clip, Bus::Effects, [0.; 3], false)),
            "contact did not occupy the voice"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if sound
                .output
                .edit(|mixer| mixer.play(42, clip, Bus::Effects, [1e8, 0., 0.], false))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "audio output did not consume voices"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
