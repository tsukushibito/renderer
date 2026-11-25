//! Bind research-facility gameplay outcomes to shared audio and lifetime APIs.
use sashiko::{
    audio::{Bus, Clip, Output},
    effects::Effects,
};
use std::num::NonZeroU64;
use survivor::{Game, MAX_ENEMIES, Phase};

const PLAYER: u64 = u64::MAX;

pub(super) struct Soundscape {
    output: Output,
    warnings: Effects<u64, ()>,
    warning: Clip,
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
        output.edit(|mixer| mixer.set_master(volume));
        Some(Self {
            output,
            warnings: Effects::new(MAX_ENEMIES),
            warning: Clip::from_wav(
                &include_bytes!("../../../../assets/survivor/sounds/warning.wav")[..],
            )
            .expect("warning cue"),
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
        })
    }

    pub fn update(&mut self, game: &Game) {
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
