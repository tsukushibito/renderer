use crate::Arena;
use sashiko::{
    math::{Aabb3, Pose3, Vec2, Vec3},
    physics::{Bodies, BodyId, BodyKind, ColliderId, PhysicsWorld},
};

pub(crate) struct Hall {
    world: PhysicsWorld,
    cargo: Option<(BodyId, ColliderId)>,
    extent: Vec2,
}
fn xyz(p: Vec2, y: f32) -> Vec3 {
    Vec3::new(p.x(), y, p.y())
}
impl Hall {
    pub fn new(arena: &Arena) -> Self {
        let mut world = PhysicsWorld::new(Vec3::new(0., -9.81, 0.), 1. / crate::TICK_HZ as f32);
        let [x, z] = arena.half_extent.to_array();
        world.add_box(
            Pose3::from_translation(Vec3::new(0., -0.5, 0.)),
            Vec3::new(x + 1., 0.5, z + 1.),
            BodyKind::Fixed,
        );
        for side in [-1., 1.] {
            world.add_box(
                Pose3::from_translation(Vec3::new(side * (x + 0.5), 1., 0.)),
                Vec3::new(0.5, 1., z + 1.),
                BodyKind::Fixed,
            );
            world.add_box(
                Pose3::from_translation(Vec3::new(0., 1., side * (z + 0.5))),
                Vec3::new(x + 1., 1., 0.5),
                BodyKind::Fixed,
            );
        }
        for b in &arena.obstacles {
            world.add_box(
                Pose3::from_translation(xyz(b.center, 0.5)),
                xyz(b.half_extent, 0.5),
                BodyKind::Fixed,
            );
        }
        let cargo = arena.cargo.map(|b| {
            world.add_box(
                Pose3::from_translation(xyz(b.center, 0.5)),
                xyz(b.half_extent, 0.5),
                BodyKind::Dynamic {
                    lock_rotation: true,
                },
            )
        });
        world.synchronize();
        Self {
            world,
            cargo,
            extent: arena.half_extent,
        }
    }
    pub fn cargo(&self) -> Option<Vec3> {
        self.cargo
            .map(|(body, _)| self.world.pose(body).translation)
    }
    pub fn open(&self, p: Vec2, r: f32) -> bool {
        (0..2).all(|i| p[i].abs() + r <= self.extent[i])
            && !self.world.overlaps_box(
                Aabb3::from_center_half_extents(
                    xyz(p, 0.5),
                    Vec3::new(r.max(0.00001), 0.1, r.max(0.00001)),
                ),
                Bodies::All,
            )
    }
    pub fn blocked_shot(&self, p: Vec2, delta: Vec2) -> bool {
        self.world
            .ray(xyz(p, 0.5), xyz(delta, 0.), Bodies::All)
            .is_some()
    }
    pub fn advance(&mut self) {
        self.world.step();
    }
    pub fn slide(&mut self, mut p: Vec2, dir: Vec2, step: f32, r: f32, push: bool) -> (Vec2, bool) {
        let travel = dir * step;
        let half = Vec3::new(r, 0.1, r);
        if !self.world.box_path_may_hit(
            Aabb3::from_center_half_extents(xyz(p, 0.5), half),
            xyz(travel, 0.),
            Bodies::All,
        ) {
            return (p + travel, false);
        }
        let mut blocked = false;
        for axis in 0..2 {
            let mut delta = Vec2::ZERO;
            delta[axis] = travel[axis];
            if delta[axis] == 0. {
                continue;
            }
            if let Some(hit) = self.world.sweep_box(
                Aabb3::from_center_half_extents(xyz(p, 0.5), half),
                xyz(delta, 0.),
                Bodies::All,
            ) {
                blocked = true;
                if push
                    && let Some((body, id)) = self.cargo
                    && hit.collider == id
                {
                    self.world.impulse(body, xyz(delta * 4., 0.));
                }
                let skin = 0.0001 / delta[axis].abs();
                p += delta * (hit.fraction - skin).max(0.);
            } else {
                p += delta;
            }
        }
        (p, blocked)
    }
}
