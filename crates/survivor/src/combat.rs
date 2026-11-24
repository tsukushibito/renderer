use crate::{Drop, Events, Game, MAX_DROPS, Shot, TICK_HZ, direction, distance_squared};

impl Game {
    pub(crate) fn shoot(&mut self, dir: [f32; 2]) {
        self.shots.push(Shot {
            pos: self.player,
            dir,
            remaining_ticks: 60,
            pierce_left: self.upgrades[1].min(8),
            last_hit: None,
        });
    }
    pub(crate) fn combat(&mut self, events: &mut Events) {
        if self.reload == 0 {
            let dir = self
                .enemies
                .iter()
                .filter(|e| distance_squared(e.pos, self.player) <= 100.)
                .min_by(|a, b| {
                    distance_squared(a.pos, self.player)
                        .total_cmp(&distance_squared(b.pos, self.player))
                })
                .map(|enemy| direction(self.player, enemy.pos));
            if let Some(dir) = dir {
                self.shoot(dir);
                self.reload = 30 / (1 + self.upgrades[0].min(5));
            }
        }
        self.reload = self.reload.saturating_sub(1);
        let pulse = self.upgrades[2];
        self.pulse_radius = (self.pulse_radius - 0.18).max(0.);
        if pulse > 0 && self.elapsed.is_multiple_of(90) {
            self.pulse_radius = 1.7 + 0.6 * f32::from(pulse.min(5));
            for enemy in &mut self.enemies {
                if distance_squared(enemy.pos, self.player) < self.pulse_radius.powi(2) {
                    enemy.hp = enemy.hp.saturating_sub(1 + pulse / 2);
                }
            }
        }
        for shot in &mut self.shots {
            for i in 0..2 {
                shot.pos[i] += shot.dir[i] * 13. / TICK_HZ as f32;
            }
            shot.remaining_ticks -= 1;
            if !self.arena.floor().open(shot.pos, 0.) {
                shot.remaining_ticks = 0;
                continue;
            }
            for enemy in &mut self.enemies {
                if enemy.hp == 0
                    || shot.last_hit == Some(enemy.id)
                    || distance_squared(enemy.pos, shot.pos) > 0.36 * 0.36
                {
                    continue;
                }
                enemy.hp -= 1;
                shot.last_hit = Some(enemy.id);
                if shot.pierce_left == 0 {
                    shot.remaining_ticks = 0;
                    break;
                }
                shot.pierce_left -= 1;
            }
        }
        self.shots.retain(|s| s.remaining_ticks > 0);
        self.enemies.retain(|e| {
            if e.hp != 0 {
                return true;
            }
            if self.drops.len() < MAX_DROPS {
                self.drops.push(Drop {
                    pos: e.pos,
                    value: 1,
                });
            } else {
                // Preserve XP at capacity by stacking it in the nearest drop.
                if let Some(near) = self.drops.iter_mut().min_by(|a, b| {
                    distance_squared(a.pos, e.pos).total_cmp(&distance_squared(b.pos, e.pos))
                }) {
                    near.value = near.value.saturating_add(1);
                }
            }
            events.kills += 1;
            false
        });
        self.kills = self.kills.saturating_add(events.kills);
        self.drops.retain(|drop| {
            if distance_squared(drop.pos, self.player) < 1.4 * 1.4 {
                self.xp = self.xp.saturating_add(drop.value);
                false
            } else {
                true
            }
        });
    }
}
