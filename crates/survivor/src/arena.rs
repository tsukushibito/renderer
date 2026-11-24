//! Metre-scale XZ geometry and the shared crowd navigation field.
use crate::{ENEMY_RADIUS, Enemy, MAX_ENEMIES, PLAYER_RADIUS};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Obstacle {
    pub center: [f32; 2],
    pub half_extent: [f32; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Arena {
    pub half_extent: [f32; 2],
    pub player_spawn: [f32; 2],
    pub marker: [f32; 2],
    pub obstacles: Vec<Obstacle>,
    #[serde(default = "crate::authoring::default_subjects")]
    pub subjects: Vec<crate::authoring::SubjectType>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::from_json(include_str!("../../../assets/survivor/arena.json")).expect("bundled arena")
    }
}

impl Arena {
    pub fn from_json(source: &str) -> Result<Self, String> {
        let a = crate::authoring::decode_arena(source)?;
        crate::authoring::validate_subjects(&a.subjects)?;
        if !a
            .half_extent
            .into_iter()
            .all(|v| (2.0..=128.0).contains(&v))
        {
            return Err("arena half extents must be between 2 and 128 metres".to_owned());
        }
        for b in &a.obstacles {
            if !(0..2).all(|i| {
                b.half_extent[i] > 0. && b.center[i].abs() + b.half_extent[i] <= a.half_extent[i]
            }) {
                return Err("obstacle is empty or outside the room".to_owned());
            }
        }
        if !a.open(a.player_spawn, PLAYER_RADIUS) || !a.open(a.marker, PLAYER_RADIUS) {
            return Err("spawn point and marker must be on passable floor".to_owned());
        }
        Ok(a)
    }

    pub(crate) fn open(&self, p: [f32; 2], r: f32) -> bool {
        (0..2).all(|i| p[i].abs() + r <= self.half_extent[i])
            && !self
                .obstacles
                .iter()
                .any(|b| (0..2).all(|i| (p[i] - b.center[i]).abs() < b.half_extent[i] + r))
    }

    pub(crate) fn slide(
        &self,
        from: [f32; 2],
        dir: [f32; 2],
        step: f32,
        r: f32,
    ) -> ([f32; 2], bool) {
        let mut pos = from;
        let mut hit = false;
        for (i, component) in dir.into_iter().enumerate() {
            let mut next = pos;
            next[i] += component * step;
            if self.open(next, r) {
                pos = next
            } else {
                hit = true
            }
        }
        (pos, hit)
    }
}

pub(crate) fn distance_squared(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

pub(crate) fn unit_or_zero(v: [f32; 2]) -> [f32; 2] {
    let length = v[0].hypot(v[1]);
    if length > 0. && length.is_finite() {
        v.map(|c| c / length)
    } else {
        [0.; 2]
    }
}

pub(crate) fn direction(from: [f32; 2], to: [f32; 2]) -> [f32; 2] {
    unit_or_zero([to[0] - from[0], to[1] - from[1]])
}

const EMPTY: usize = usize::MAX;

pub(crate) struct Navigation {
    extent: [f32; 2],
    width: usize,
    height: usize,
    walkable: Vec<bool>,
    distance: Vec<u32>,
    frontier: VecDeque<usize>,
    target: usize,
    heads: Vec<usize>,
    links: Vec<usize>,
    positions: Vec<[f32; 2]>,
}

impl Navigation {
    pub fn new(arena: &Arena) -> Self {
        let [width, height] = arena.half_extent.map(|v| (v * 2.).ceil() as usize);
        let count = width * height;
        let mut field = Self {
            extent: arena.half_extent,
            width,
            height,
            walkable: vec![false; count],
            distance: vec![u32::MAX; count],
            frontier: VecDeque::with_capacity(count),
            target: EMPTY,
            heads: vec![EMPTY; count],
            links: vec![EMPTY; MAX_ENEMIES],
            positions: vec![[0.; 2]; MAX_ENEMIES],
        };
        for i in 0..count {
            field.walkable[i] = arena.open(field.center(i), ENEMY_RADIUS);
        }
        field
    }
    fn cell(&self, p: [f32; 2]) -> usize {
        let [x, z] = [p[0] + self.extent[0], p[1] + self.extent[1]].map(|v| v as usize);
        z.min(self.height - 1) * self.width + x.min(self.width - 1)
    }
    fn center(&self, c: usize) -> [f32; 2] {
        [
            (c % self.width) as f32 + 0.5 - self.extent[0],
            (c / self.width) as f32 + 0.5 - self.extent[1],
        ]
    }
    fn neighbors(&self, c: usize) -> [Option<usize>; 4] {
        let x = c % self.width;
        [
            (x > 0).then(|| c - 1),
            (x + 1 < self.width).then_some(c + 1),
            c.checked_sub(self.width),
            (c + self.width < self.walkable.len()).then_some(c + self.width),
        ]
    }
    pub fn update(&mut self, player: [f32; 2], enemies: &[Enemy]) {
        let target = self.cell(player);
        if self.target != target {
            self.target = target;
            self.distance.fill(u32::MAX);
            self.distance[target] = 0;
            self.frontier.push_back(target);
            while let Some(c) = self.frontier.pop_front() {
                for n in self.neighbors(c).into_iter().flatten() {
                    if self.walkable[n] && self.distance[n] == u32::MAX {
                        self.distance[n] = self.distance[c] + 1;
                        self.frontier.push_back(n);
                    }
                }
            }
        }
        self.heads.fill(EMPTY);
        for (i, e) in enemies.iter().enumerate() {
            let c = self.cell(e.pos);
            self.positions[i] = e.pos;
            self.links[i] = self.heads[c];
            self.heads[c] = i;
        }
    }
    pub fn steer(&self, index: usize, p: [f32; 2], player: [f32; 2]) -> [f32; 2] {
        let cell = self.cell(p);
        let mut target = player;
        if self.distance[cell] > 2 {
            // Follow the direct bearing in open halls; detour down the shared flow
            // field when the next step would enter a blocked or farther cell.
            let toward = direction(p, player);
            let ahead = self.cell([p[0] + toward[0], p[1] + toward[1]]);
            if (!self.walkable[ahead] || self.distance[ahead] >= self.distance[cell])
                && let Some(n) = self
                    .neighbors(cell)
                    .into_iter()
                    .flatten()
                    .filter(|&n| self.distance[n] < self.distance[cell])
                    .min_by_key(|&n| self.distance[n])
            {
                target = self.center(n);
            }
        }
        let mut v = direction(p, target);
        let x = cell % self.width;
        let z = cell / self.width;
        for nz in z.saturating_sub(1)..=(z + 1).min(self.height - 1) {
            for nx in x.saturating_sub(1)..=(x + 1).min(self.width - 1) {
                let mut j = self.heads[nz * self.width + nx];
                while j != EMPTY {
                    let q = self.positions[j];
                    let d2 = distance_squared(p, q);
                    if j != index && d2 > 1e-6 && d2 < 0.36 {
                        let force = (0.6 / d2.sqrt() - 1.) * 1.6;
                        v[0] += (p[0] - q[0]) * force;
                        v[1] += (p[1] - q[1]) * force;
                    }
                    j = self.links[j];
                }
            }
        }
        unit_or_zero(v)
    }
}
