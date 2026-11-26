//! A fixed-step 3D world. Query displacements and hit fractions describe one segment.
use rapier3d::{parry::query::ShapeCastOptions, prelude as r};
use sashiko_math::{Rotation3, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyId(r::RigidBodyHandle);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColliderId(r::ColliderHandle);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bodies {
    All,
    Fixed,
    Dynamic,
}
impl Bodies {
    fn filter(self) -> r::QueryFilter<'static> {
        match self {
            Self::All => r::QueryFilter::default(),
            Self::Fixed => r::QueryFilter::only_fixed(),
            Self::Dynamic => r::QueryFilter::only_dynamic(),
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub collider: ColliderId,
    pub fraction: f32,
    pub normal: Vec3,
    pub point: Vec3,
}

/// How a box participates in the simulation.
#[derive(Clone, Copy, Debug)]
pub enum BodyKind {
    Fixed,
    Dynamic { lock_rotation: bool },
}

/// Body and collider IDs are local to the world that issued them.
pub struct PhysicsWorld(r::PhysicsWorld);
fn vector(v: Vec3) -> r::Vector {
    v.to_array().into()
}
fn pose(v: Vec3) -> r::Pose {
    r::Pose::translation(v.x(), v.y(), v.z())
}

impl PhysicsWorld {
    pub fn new(gravity: Vec3, timestep: f32) -> Self {
        assert!(timestep.is_finite() && timestep > 0.);
        let mut world = r::PhysicsWorld::new();
        world.gravity = vector(gravity);
        world.integration_parameters.dt = timestep;
        Self(world)
    }
    pub fn add_box(&mut self, center: Vec3, half: Vec3, kind: BodyKind) -> (BodyId, ColliderId) {
        assert!(half.to_array().into_iter().all(|v| v.is_finite() && v > 0.));
        let body = match kind {
            BodyKind::Fixed => r::RigidBodyBuilder::fixed(),
            BodyKind::Dynamic {
                lock_rotation: true,
            } => r::RigidBodyBuilder::dynamic().lock_rotations(),
            BodyKind::Dynamic {
                lock_rotation: false,
            } => r::RigidBodyBuilder::dynamic(),
        };
        let (body, collider) = self.0.insert(
            body.translation(vector(center)),
            r::ColliderBuilder::cuboid(half.x(), half.y(), half.z()),
        );
        (BodyId(body), ColliderId(collider))
    }
    pub fn set_translation(&mut self, body: BodyId, value: Vec3) {
        self.0.bodies[body.0].set_translation(vector(value), true);
    }
    pub fn pose(&self, body: BodyId) -> (Vec3, Rotation3) {
        let b = &self.0.bodies[body.0];
        (
            b.translation().to_array().into(),
            Rotation3::try_from(b.rotation().to_array()).expect("unit body rotation"),
        )
    }
    pub fn impulse(&mut self, body: BodyId, impulse: Vec3) {
        self.0.bodies[body.0].apply_impulse(vector(impulse), true);
    }
    pub fn synchronize(&mut self) {
        self.0.detect_collisions(&(), &());
    }
    pub fn step(&mut self) {
        self.0.step();
    }
    pub fn ray(&self, origin: Vec3, delta: Vec3, bodies: Bodies) -> Option<Hit> {
        let ray = r::Ray::new(vector(origin), vector(delta));
        let (id, hit) = self
            .0
            .cast_ray_and_get_normal(&ray, 1., true, bodies.filter())?;
        Some(Hit {
            collider: ColliderId(id),
            fraction: hit.time_of_impact,
            normal: hit.normal.to_array().into(),
            point: origin + delta * hit.time_of_impact,
        })
    }
    pub fn sweep_box(&self, center: Vec3, half: Vec3, delta: Vec3, bodies: Bodies) -> Option<Hit> {
        let options = ShapeCastOptions {
            max_time_of_impact: 1.,
            stop_at_penetration: false,
            ..Default::default()
        };
        let (id, hit) = self.0.cast_shape(
            &pose(center),
            vector(delta),
            &r::Cuboid::new(vector(half)),
            options,
            bodies.filter(),
        )?;
        Some(Hit {
            collider: ColliderId(id),
            fraction: hit.time_of_impact,
            normal: hit.normal1.to_array().into(),
            point: hit.witness1.to_array().into(),
        })
    }
    /// Conservative broad-phase test over the entire translation, including both endpoints.
    pub fn box_path_may_hit(&self, center: Vec3, half: Vec3, delta: Vec3, bodies: Bodies) -> bool {
        let a = vector(center);
        let b = a + vector(delta);
        let bounds = r::Aabb::new(a.min(b) - vector(half), a.max(b) + vector(half));
        self.0
            .intersect_aabb_conservative(bounds, bodies.filter())
            .next()
            .is_some()
    }
    pub fn overlaps_box(&self, center: Vec3, half: Vec3, bodies: Bodies) -> bool {
        self.0
            .intersect_shape(pose(center), &r::Cuboid::new(vector(half)), bodies.filter())
            .next()
            .is_some()
    }
}
