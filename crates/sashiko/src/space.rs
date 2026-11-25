//! Geometry and shared-goal crowd navigation on a metre-scale XZ floor.
use crate::math::Vec2;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Axis-aligned obstacle in world metres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub center: Vec2,
    pub half_extent: Vec2,
}

/// Borrow the same static placements used by the renderer or level editor.
#[derive(Clone, Copy)]
pub struct Floor<'a> {
    pub half_extent: Vec2,
    pub obstacles: &'a [Footprint],
}

impl Floor<'_> {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self
            .half_extent
            .to_array()
            .iter()
            .all(|v| v.is_finite() && *v > 0.)
        {
            return Err("invalid floor half extent");
        }
        for b in self.obstacles {
            if !(0..2).all(|i| {
                b.half_extent[i] > 0. && b.center[i].abs() + b.half_extent[i] <= self.half_extent[i]
            }) {
                return Err("obstacle is empty or outside the floor");
            }
        }
        Ok(())
    }

    /// Test an axis-aligned square of half width `r`; touching an obstacle edge is allowed.
    pub fn open(&self, p: Vec2, r: f32) -> bool {
        r >= 0.
            && (0..2).all(|i| p[i].abs() + r <= self.half_extent[i])
            && !self
                .obstacles
                .iter()
                .any(|b| (0..2).all(|i| (p[i] - b.center[i]).abs() < b.half_extent[i] + r))
    }

    /// Axis-separated endpoint tests for a single short simulation step.
    /// Edges are tested at the endpoint on each axis; choose substeps for thin geometry.
    pub fn slide(&self, from: Vec2, dir: Vec2, step: f32, r: f32) -> (Vec2, bool) {
        let mut pos = from;
        let mut hit = false;
        for (i, component) in dir.to_array().into_iter().enumerate() {
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

const EMPTY: usize = usize::MAX;

pub struct CrowdNavigation {
    extent: Vec2,
    width: usize,
    height: usize,
    walkable: Vec<bool>,
    distance: Vec<u32>,
    frontier: VecDeque<usize>,
    target: usize,
    heads: Vec<usize>,
    links: Vec<usize>,
    positions: Vec<Vec2>,
    goal: Vec2,
}

impl CrowdNavigation {
    /// Prepare a one-metre navigation grid at a scene change. Rebuild after editing geometry.
    pub fn new(floor: Floor<'_>, clearance: f32, max_agents: usize) -> Result<Self, &'static str> {
        floor.validate()?;
        if !clearance.is_finite() || clearance < 0. {
            return Err("invalid agent clearance");
        }
        let [width, height] = floor
            .half_extent
            .to_array()
            .map(|v| (v * 2.).ceil() as usize);
        let count = width
            .checked_mul(height)
            .filter(|n| *n < u32::MAX as usize)
            .ok_or("navigation grid too large")?;
        let mut field = Self {
            extent: floor.half_extent,
            width,
            height,
            walkable: vec![false; count],
            distance: vec![u32::MAX; count],
            frontier: VecDeque::with_capacity(count),
            target: EMPTY,
            heads: vec![EMPTY; count],
            links: vec![EMPTY; max_agents],
            positions: Vec::with_capacity(max_agents),
            goal: Vec2::ZERO,
        };
        for i in 0..count {
            field.walkable[i] = floor.open(field.center(i), clearance);
        }
        Ok(field)
    }
    fn cell(&self, p: Vec2) -> usize {
        let [x, z] = (p + self.extent).to_array().map(|v| v as usize);
        z.min(self.height - 1) * self.width + x.min(self.width - 1)
    }
    fn center(&self, c: usize) -> Vec2 {
        Vec2::new(
            (c % self.width) as f32 + 0.5 - self.extent[0],
            (c / self.width) as f32 + 0.5 - self.extent[1],
        )
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
    /// Snapshot positions in caller order; query indices remain valid until the next update.
    /// A capacity failure preserves the previous complete snapshot and goal.
    pub fn update<T>(
        &mut self,
        goal: Vec2,
        agents: &[T],
        mut position: impl FnMut(&T) -> Vec2,
    ) -> Result<(), &'static str> {
        if agents.len() > self.links.len() {
            return Err("crowd exceeds prepared agent capacity");
        }
        self.goal = goal;
        let target = self.cell(goal);
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
        self.positions.clear();
        for (i, agent) in agents.iter().enumerate() {
            let pos = position(agent);
            let c = self.cell(pos);
            self.positions.push(pos);
            self.links[i] = self.heads[c];
            self.heads[c] = i;
        }
        Ok(())
    }

    /// Snapshot indices and squared distances of agents inside a circle.
    pub fn nearby(&self, p: Vec2, radius: f32, mut visit: impl FnMut(usize, f32)) {
        if !(radius >= 0. && radius.is_finite()) {
            return;
        }
        let min = self.cell(p - Vec2::ONE * radius);
        let max = self.cell(p + Vec2::ONE * radius);
        for z in min / self.width..=max / self.width {
            for x in min % self.width..=max % self.width {
                let mut j = self.heads[z * self.width + x];
                while j != EMPTY {
                    let d2 = p.distance_squared(self.positions[j]);
                    if d2 <= radius * radius {
                        visit(j, d2);
                    }
                    j = self.links[j];
                }
            }
        }
    }

    /// Steer one snapshot agent toward the shared goal, avoiding its neighbours.
    pub fn steer(
        &self,
        index: usize,
        separation_radius: f32,
        separation_strength: f32,
    ) -> Option<Vec2> {
        let p = *self.positions.get(index)?;
        let goal = self.goal;
        let cell = self.cell(p);
        let mut target = goal;
        if self.distance[cell] > 2 {
            // Follow the direct bearing in open halls; detour down the shared flow
            // field when the next step would enter a blocked or farther cell.
            let toward = (goal - p).normalize_or_zero();
            let ahead = self.cell(p + toward);
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
        let mut v = (target - p).normalize_or_zero();
        self.nearby(p, separation_radius, |j, d2| {
            if j != index && d2 > 1e-6 && d2 < separation_radius * separation_radius {
                let q = self.positions[j];
                let force = (separation_radius / d2.sqrt() - 1.) * separation_strength;
                v += (p - q) * force;
            }
        });
        Some(v.normalize_or_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_a_footprint_rebuilds_both_wall_and_flow_from_the_same_placement() {
        let from = Vec2::new(-1.5, 0.5);
        let destination = Vec2::new(3.5, 0.5);
        let approach = |center| {
            let bench = Footprint {
                center,
                half_extent: Vec2::new(0.55, 1.5),
            };
            let floor = Floor {
                half_extent: Vec2::new(5., 5.),
                obstacles: std::slice::from_ref(&bench),
            };
            let mut field = CrowdNavigation::new(floor, 0.2, 1).unwrap();
            field.update(destination, &[from], |p| *p).unwrap();
            (
                floor.slide(from, Vec2::new(1., 0.), 1., 0.2),
                field.steer(0, 0., 0.).unwrap(),
            )
        };
        let (blocked, detour) = approach(Vec2::new(-0.5, 0.5));
        let (clear, direct) = approach(Vec2::new(-0.5, 3.));
        assert_eq!(blocked, (from, true));
        assert_eq!(clear, (Vec2::new(-0.5, 0.5), false));
        assert_ne!(detour[1], 0.);
        assert_eq!(direct, Vec2::new(1., 0.));
    }

    #[test]
    fn capacity_failure_preserves_query_indices_and_shorter_update_retires_them() {
        let floor = Floor {
            half_extent: Vec2::new(8., 8.),
            obstacles: &[],
        };
        let mut nav = CrowdNavigation::new(floor, 0., 2).unwrap();
        let snapshot = [Vec2::new(-1., 0.), Vec2::new(1., 0.)];
        nav.update(Vec2::new(3., 0.), &snapshot, |p| *p).unwrap();
        assert!(
            nav.update(Vec2::new(0., 3.), &[Vec2::ZERO; 3], |p| *p)
                .is_err()
        );
        let mut hits = Vec::new();
        nav.nearby(Vec2::ZERO, 1., |i, d| hits.push((i, d)));
        assert_eq!(hits, [(0, 1.), (1, 1.)]);
        assert_eq!(nav.steer(0, 0., 0.), Some(Vec2::new(1., 0.)));
        nav.update(Vec2::new(0., 3.), &snapshot[..1], |p| *p)
            .unwrap();
        assert!(nav.steer(1, 0., 0.).is_none());
    }
}
