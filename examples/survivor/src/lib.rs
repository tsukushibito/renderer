//! Fixed-step research facility simulation shared by the window and replay hosts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecordedVisual {
    pub character: String,
    #[serde(default)]
    pub character_shadows: bool,
    catalog: sashiko::asset::Digest,
    models: Vec<[sashiko::asset::Digest; 2]>,
    environment: [sashiko::asset::Digest; 6],
}

mod arena;
pub mod authoring;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod character;
mod combat;
pub mod controls;
#[cfg(all(
    feature = "embed",
    any(
        target_arch = "wasm32",
        all(
            windows,
            feature = "presentation",
            any(feature = "d3d12", feature = "vulkan")
        )
    )
))]
mod embed;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
pub mod visual;
pub use arena::{Arena, Obstacle};
use sashiko::{math::Vec2, space::CrowdNavigation};
use serde::{Deserialize, Serialize};
pub const TICK_HZ: u32 = 60;
pub const MAX_ENEMIES: usize = 500;
pub const MAX_DROPS: usize = 1024;
pub const MAX_SHOTS: usize = 128;
const STARTING_ENEMIES: usize = 8;
const PLAYER_RADIUS: f32 = 0.32;
const ENEMY_RADIUS: f32 = 0.23;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Early,
    Endgame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Playing,
    Choosing,
    Defeated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Upgrade {
    Rapid,
    Pierce,
    Pulse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Input {
    pub movement: Vec2,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub analog_movement: bool,
    pub upgrade: Option<Upgrade>,
    pub restart: bool,
    #[serde(default, skip_serializing_if = "sashiko::ui::MenuInput::is_idle")]
    pub menu: sashiko::ui::MenuInput,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
pub struct Events {
    pub contact: bool,
    pub reached_marker: bool,
    pub died: bool,
    pub kills: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Enemy {
    pub archetype: usize,
    pub id: u64,
    pub pos: Vec2,
    pub facing: Vec2,
    pub windup: bool,
    hp: u8,
    windup_left: u8,
    dash_left: u8,
    recovery: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub pos: Vec2,
    pub dir: Vec2,
    remaining_ticks: u8,
    pierce_left: u8,
    last_hit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drop {
    pub pos: Vec2,
    value: u32,
}

struct Rng(u64);
impl Rng {
    fn uniform(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((z ^ (z >> 31)) >> 40) as f32 / 16777216.
    }
}

pub struct Game {
    pub arena: Arena,
    pub tick: u64,
    pub phase: Phase,
    pub upgrade_focus: sashiko::ui::Focus,
    pub player: Vec2,
    pub hp: u32,
    pub xp: u32,
    pub level: u32,
    pub kills: u32,
    pub enemies: Vec<Enemy>,
    pub shots: Vec<Shot>,
    pub drops: Vec<Drop>,
    pub pulse_radius: f32,
    pub upgrades: [u8; 3],
    pub contacts: u64,
    pub marker_entries: u64,
    scenario: Scenario,
    seed: u64,
    rng: Rng,
    elapsed: u64,
    next_id: u64,
    reload: u8,
    invulnerability: u8,
    nav: CrowdNavigation,
    scene: sashiko::scene::World<authoring::RuleContext>,
}

impl Game {
    pub const MAX_HP: u32 = 100;

    pub fn new(arena: Arena, scenario: Scenario, seed: u64) -> Self {
        let nav = CrowdNavigation::new(arena.floor(), ENEMY_RADIUS, MAX_ENEMIES)
            .expect("validated arena");
        let scene = authoring::game_world(&arena);
        let mut game = Self {
            player: arena.player_spawn,
            arena,
            tick: 0,
            phase: Phase::Playing,
            upgrade_focus: Default::default(),
            hp: Self::MAX_HP,
            xp: 0,
            level: 1,
            kills: 0,
            enemies: Vec::with_capacity(MAX_ENEMIES),
            shots: Vec::with_capacity(MAX_SHOTS),
            drops: Vec::with_capacity(MAX_DROPS),
            pulse_radius: 0.,
            upgrades: [0; 3],
            scenario,
            seed,
            rng: Rng(seed),
            elapsed: 0,
            next_id: 0,
            reload: 0,
            invulnerability: 0,
            scene,
            nav,
            contacts: 0,
            marker_entries: 0,
        };
        if scenario == Scenario::Endgame {
            game.elapsed = (MAX_ENEMIES - STARTING_ENEMIES) as u64 * 180;
            game.upgrades = [2, 2, 1];
            game.level = 6;
        }
        game.repopulate();
        if scenario == Scenario::Endgame {
            for i in 0..128 {
                let mut pos = game.random_pos();
                if !game.arena.floor().open(pos, 0.) {
                    let Some(subject) = game.enemies.get(i) else {
                        continue;
                    };
                    pos = subject.pos;
                }
                game.drops.push(Drop { pos, value: 1 });
            }
            for i in 0..32 {
                let angle = i as f32 * std::f32::consts::TAU / 32.;
                game.shoot(Vec2::new(angle.cos(), angle.sin()));
            }
        }
        game
    }

    pub fn xp_required(&self) -> u32 {
        self.level.saturating_mul(2).saturating_add(1)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            tick: self.tick,
            phase: self.phase,
            player: self.player,
            hp: self.hp,
            xp: self.xp,
            level: self.level,
            kills: self.kills,
            enemies: self.enemies.len(),
            shots: self.shots.len(),
            drops: self.drops.len(),
            upgrades: self.upgrades,
            contacts: self.contacts,
            marker_entries: self.marker_entries,
        }
    }

    pub fn step(&mut self, input: Input) -> Events {
        let tick = self.tick + 1;
        self.tick = tick;
        let mut events = Events::default();
        match self.phase {
            Phase::Defeated => {
                if input.restart {
                    *self = Self::new(self.arena.clone(), self.scenario, self.seed);
                    self.tick = tick;
                }
                return events;
            }
            Phase::Choosing => {
                let choices = [Upgrade::Rapid, Upgrade::Pierce, Upgrade::Pulse];
                let selected = self.upgrade_focus.update(input.menu, &choices).copied();
                if let Some(choice) = input.upgrade.or(selected) {
                    let slot = &mut self.upgrades[choice as usize];
                    *slot = slot.saturating_add(1);
                    self.level += 1;
                    self.phase = Phase::Playing;
                }
                return events;
            }
            Phase::Playing => {}
        }
        self.elapsed += 1;
        self.invulnerability = self.invulnerability.saturating_sub(1);
        let movement = if input.analog_movement && input.movement.length() <= 1. {
            input.movement
        } else {
            input.movement.normalize_or_zero()
        };
        let (pos, contact) =
            self.arena
                .floor()
                .slide(self.player, movement, 4.5 / TICK_HZ as f32, PLAYER_RADIUS);
        self.player = pos;
        events.contact = contact;

        let mut rules = authoring::RuleContext {
            player: self.player,
            marker: self.arena.marker,
            entered_marker: false,
        };
        self.scene.fixed_update(&mut rules);
        events.reached_marker = rules.entered_marker;
        self.move_enemies(&mut events);
        self.contacts += u64::from(events.contact);
        self.marker_entries += u64::from(events.reached_marker);
        self.combat(&mut events);
        self.repopulate();
        if self.hp == 0 {
            self.phase = Phase::Defeated;
            events.died = true;
        } else if self.xp >= self.xp_required() {
            self.xp -= self.xp_required();
            self.upgrade_focus = Default::default();
            self.phase = Phase::Choosing;
        }
        events
    }

    fn random_pos(&mut self) -> Vec2 {
        self.arena
            .half_extent
            .to_array()
            .map(|v| (self.rng.uniform() * 2. - 1.) * (v - ENEMY_RADIUS))
            .into()
    }

    fn repopulate(&mut self) {
        let count = (STARTING_ENEMIES + (self.elapsed / 180) as usize).min(MAX_ENEMIES);
        while self.enemies.len() < count {
            let spawn = (0..64).find_map(|_| {
                let p = self.random_pos();
                (p.distance_squared(self.player) >= 16. && self.arena.floor().open(p, ENEMY_RADIUS))
                    .then_some(p)
            });
            let Some(spawn) = spawn else {
                break;
            };
            let id = self.next_id;
            self.next_id += 1;
            let mut slot = id
                % self
                    .arena
                    .subjects
                    .iter()
                    .map(|s| u64::from(s.copies))
                    .sum::<u64>();
            let archetype = self
                .arena
                .subjects
                .iter()
                .position(|s| {
                    if slot < u64::from(s.copies) {
                        return true;
                    }
                    slot -= u64::from(s.copies);
                    false
                })
                .expect("validated nonempty spawn table");
            self.enemies.push(Enemy {
                archetype,
                id,
                pos: spawn,
                facing: (self.player - spawn).normalize_or_zero(),
                windup: false,
                hp: self.arena.subjects[archetype].hp,
                windup_left: 0,
                dash_left: 0,
                recovery: (id % 40) as u8,
            });
        }
    }

    fn move_enemies(&mut self, events: &mut Events) {
        self.nav
            .update(self.player, &self.enemies, |enemy| enemy.pos)
            .expect("enemies fit prepared capacity");
        for (index, enemy) in self.enemies.iter_mut().enumerate() {
            let d = self.player.distance_squared(enemy.pos);
            let subject = &self.arena.subjects[enemy.archetype];
            enemy.recovery = enemy.recovery.saturating_sub(1);
            let speed = if enemy.windup_left > 0 {
                enemy.windup_left -= 1;
                if enemy.windup_left == 0 {
                    enemy.windup = false;
                    enemy.dash_left = subject.lunge.as_ref().expect("active windup").dash_ticks;
                }
                0.
            } else if enemy.dash_left > 0 {
                enemy.dash_left -= 1;
                if enemy.dash_left == 0 {
                    enemy.recovery = subject.lunge.as_ref().expect("active dash").recovery_ticks;
                }
                subject.lunge.as_ref().expect("active dash").speed
            } else if let Some(lunge) = &subject.lunge
                && d < lunge.range * lunge.range
                && enemy.recovery == 0
            {
                enemy.facing = (self.player - enemy.pos).normalize_or_zero();
                enemy.windup = true;
                enemy.windup_left = lunge.windup_ticks;
                0.
            } else {
                enemy.facing = self.nav.steer(index, 0.6, 1.6).expect("indexed snapshot");
                subject.walk_speed
            };
            enemy.pos = self
                .arena
                .floor()
                .slide(
                    enemy.pos,
                    enemy.facing,
                    speed / TICK_HZ as f32,
                    ENEMY_RADIUS,
                )
                .0;
            if enemy.pos.distance_squared(self.player) < (PLAYER_RADIUS + ENEMY_RADIUS).powi(2) {
                events.contact = true;
                if self.invulnerability == 0 && !enemy.windup {
                    self.hp = self
                        .hp
                        .saturating_sub(if enemy.dash_left > 0 { 16 } else { 10 });
                    self.invulnerability = 75;
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Snapshot {
    pub tick: u64,
    pub phase: Phase,
    pub player: Vec2,
    pub hp: u32,
    pub xp: u32,
    pub level: u32,
    pub kills: u32,
    pub enemies: usize,
    pub shots: usize,
    pub drops: usize,
    pub upgrades: [u8; 3],
    pub contacts: u64,
    pub marker_entries: u64,
}

/// Fixed replay route through the outer walkway. Input production only; the
/// game still performs normal collision, damage, collection and weapon updates.
pub fn benchmark_input(game: &Game) -> Input {
    let t = game
        .elapsed
        .saturating_sub(if game.scenario == Scenario::Endgame {
            (MAX_ENEMIES - STARTING_ENEMIES) as u64 * 180
        } else {
            0
        });
    let along = t.saturating_sub(54) % 1300;
    let movement = if t < 54 {
        Vec2::new(0., 1.)
    } else if along < 190 {
        Vec2::new(1., 0.)
    } else if along < 460 {
        Vec2::new(0., -1.)
    } else if along < 840 {
        Vec2::new(-1., 0.)
    } else if along < 1110 {
        Vec2::new(0., 1.)
    } else {
        Vec2::new(1., 0.)
    };
    Input {
        movement,
        analog_movement: false,
        upgrade: Some(Upgrade::Pulse),
        restart: true,
        menu: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_tick_inputs_ignore_frame_grouping_and_respect_edited_bench() {
        let run = |arena: Arena, per_frame: u32| {
            let mut game = Game::new(arena, Scenario::Early, 1);
            let input = Input {
                movement: Vec2::new(0., -1.),
                ..Input::default()
            };
            for _ in 0..(60 / per_frame) {
                for _ in 0..per_frame {
                    game.step(input);
                }
            }
            game
        };
        let smooth = run(Arena::default(), 1);
        let coarse = run(Arena::default(), 5);
        assert_eq!(smooth.snapshot(), coarse.snapshot());
        assert_eq!(smooth.enemies, coarse.enemies);
        assert_eq!(smooth.shots, coarse.shots);
        assert_eq!(smooth.drops, coarse.drops);
        assert_eq!(smooth.marker_entries, 1);
        let mut edited = Arena::default();
        edited.obstacles.push(Obstacle {
            center: Vec2::new(0., 4.5),
            half_extent: Vec2::new(0.8, 0.3),
        });
        let blocked = run(
            Arena::from_json(&serde_json::to_string(&edited).unwrap()).unwrap(),
            1,
        );
        assert_eq!(blocked.marker_entries, 0);
        assert!(blocked.contacts > 0 && blocked.player[1] > 5.);
    }

    #[test]
    fn invalid_placement_is_rejected_at_load() {
        let mut arena = Arena::default();
        arena.marker = arena.obstacles[0].center;
        assert!(Arena::from_json(&serde_json::to_string(&arena).unwrap()).is_err());
    }

    #[test]
    fn confined_arena_defers_spawns_and_keeps_endgame_drops_on_open_floor() {
        let source = r#"{"half_extent":[2,2],"player_spawn":[0,0],"marker":[0,1],"obstacles":[{"center":[1.2,0],"half_extent":[0.7,1.8]}]}"#;
        let mut game = Game::new(Arena::from_json(source).unwrap(), Scenario::Endgame, 506);
        assert!(!game.drops.is_empty());
        assert!(
            game.drops
                .iter()
                .all(|drop| game.arena.floor().open(drop.pos, 0.))
        );
        for _ in 0..2 {
            game.step(Input::default());
            assert!(game.enemies.is_empty());
            assert_eq!(game.hp, Game::MAX_HP);
        }
    }

    #[test]
    fn a_run_earns_upgrades_dies_and_restarts_without_old_state() {
        let mut game = Game::new(Arena::default(), Scenario::Early, 506);
        let initial = game.snapshot();
        for _ in 0..4000 {
            let events = game.step(Input {
                upgrade: Some(Upgrade::Pierce),
                ..Input::default()
            });
            if events.died {
                break;
            }
        }
        assert_eq!(game.phase, Phase::Defeated);
        assert!(game.level > 1 && game.kills > 0 && game.upgrades[1] > 0);
        let death_tick = game.tick;
        game.step(Input {
            restart: true,
            ..Input::default()
        });
        assert_eq!(
            game.snapshot(),
            Snapshot {
                tick: death_tick + 1,
                ..initial
            }
        );
    }

    #[test]
    fn recorded_menu_gestures_drive_upgrade_selection_on_fixed_ticks() {
        let mut game = Game::new(Arena::default(), Scenario::Early, 5);
        game.phase = Phase::Choosing;
        let tape: Vec<Input> =
            serde_json::from_str(r#"[{"menu":{"direction":1}},{"menu":{"activate":true}}]"#)
                .unwrap();
        game.step(tape[0]);
        assert_eq!(game.phase, Phase::Choosing);
        assert_eq!(game.upgrade_focus.index(), 1);
        game.step(tape[1]);
        assert_eq!(game.phase, Phase::Playing);
        assert_eq!(game.upgrades, [0, 1, 0]);
    }

    #[test]
    fn each_choice_changes_a_different_weapon_behavior() {
        let choose = |upgrade| {
            let mut game = Game::new(Arena::default(), Scenario::Early, 1);
            game.phase = Phase::Choosing;
            game.step(Input {
                upgrade: Some(upgrade),
                ..Input::default()
            });
            game.enemies[0].pos = game.player;
            game.elapsed = 89;
            game.step(Input::default());
            game
        };
        let rapid = choose(Upgrade::Rapid);
        let mut pierce = choose(Upgrade::Pierce);
        let pulse = choose(Upgrade::Pulse);
        assert!(rapid.reload < pierce.reload);
        pierce.shoot(Vec2::new(0., 1.));
        assert_eq!(pierce.shots.last().unwrap().pierce_left, 1);
        assert_eq!(rapid.pulse_radius, 0.);
        assert!(pulse.pulse_radius > 1.);
    }

    #[test]
    fn endgame_capture_keeps_all_five_hundred_subjects_active() {
        let mut game = Game::new(Arena::default(), Scenario::Endgame, 506);
        assert_eq!((game.shots.len(), game.drops.len()), (32, 128));
        for _ in 0..720 {
            let input = benchmark_input(&game);
            let events = game.step(input);
            assert!(!events.died);
            assert_eq!(game.enemies.len(), 500);
        }
        assert!(game.kills > 0 && game.contacts > 0 && game.level > 6);
    }
}
