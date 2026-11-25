use crate::{Drop, Events, Game, MAX_DROPS, Shot, TICK_HZ, Vec2};

impl Game {
    pub(crate) fn shoot(&mut self, dir: Vec2) {
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
                .filter(|e| e.pos.distance_squared(self.player) <= 100.)
                .min_by(|a, b| {
                    a.pos
                        .distance_squared(self.player)
                        .total_cmp(&b.pos.distance_squared(self.player))
                })
                .map(|enemy| (enemy.pos - self.player).normalize_or_zero());
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
                if enemy.pos.distance_squared(self.player) < self.pulse_radius.powi(2) {
                    enemy.hp = enemy.hp.saturating_sub(1 + pulse / 2);
                }
            }
        }
        for shot in &mut self.shots {
            shot.pos += shot.dir * 13. / TICK_HZ as f32;
            shot.remaining_ticks -= 1;
            if !self.arena.floor().open(shot.pos, 0.) {
                shot.remaining_ticks = 0;
                continue;
            }
            for enemy in &mut self.enemies {
                if enemy.hp == 0
                    || shot.last_hit == Some(enemy.id)
                    || enemy.pos.distance_squared(shot.pos) > 0.36 * 0.36
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
                    a.pos
                        .distance_squared(e.pos)
                        .total_cmp(&b.pos.distance_squared(e.pos))
                }) {
                    near.value = near.value.saturating_add(1);
                }
            }
            events.kills += 1;
            false
        });
        self.kills = self.kills.saturating_add(events.kills);
        self.drops.retain(|drop| {
            if drop.pos.distance_squared(self.player) < 1.4 * 1.4 {
                self.xp = self.xp.saturating_add(drop.value);
                false
            } else {
                true
            }
        });
    }
}
