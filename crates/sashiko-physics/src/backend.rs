//! A fixed-step 3D world. Query displacements and hit fractions describe one segment.
use rapier3d::{parry::query::ShapeCastOptions, prelude as r};
use sashiko_math::{Aabb3, Pose3, Rotation3, Vec3};
use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorldId(u64);

impl WorldId {
    fn allocate() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("physics world IDs exhausted"),
        )
    }
}

/// A body operation received an ID from another world or an earlier session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongWorld;

impl fmt::Display for WrongWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("body belongs to another physics world")
    }
}
impl std::error::Error for WrongWorld {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyId {
    world: WorldId,
    handle: r::RigidBodyHandle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColliderId {
    world: WorldId,
    handle: r::ColliderHandle,
}
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
pub struct PhysicsWorld {
    id: WorldId,
    backend: r::PhysicsWorld,
}
fn vector(v: Vec3) -> r::Vector {
    v.to_array().into()
}

impl PhysicsWorld {
    pub fn new(gravity: Vec3, timestep: f32) -> Self {
        assert!(timestep.is_finite() && timestep > 0.);
        let mut world = r::PhysicsWorld::new();
        world.gravity = vector(gravity);
        world.integration_parameters.dt = timestep;
        Self {
            id: WorldId::allocate(),
            backend: world,
        }
    }
    /// Place a box at a world pose. Its half-extents are measured along its local axes.
    pub fn add_box(&mut self, pose: Pose3, half: Vec3, kind: BodyKind) -> (BodyId, ColliderId) {
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
        let (body, collider) = self.backend.insert(
            body.pose(r::Pose::from_parts(
                vector(pose.translation),
                r::Rotation::from_array(pose.rotation.into()),
            )),
            r::ColliderBuilder::cuboid(half.x(), half.y(), half.z()),
        );
        (
            BodyId {
                world: self.id,
                handle: body,
            },
            ColliderId {
                world: self.id,
                handle: collider,
            },
        )
    }
    fn handle(&self, body: BodyId) -> Result<r::RigidBodyHandle, WrongWorld> {
        if body.world == self.id {
            Ok(body.handle)
        } else {
            Err(WrongWorld)
        }
    }

    pub fn set_translation(&mut self, body: BodyId, value: Vec3) -> Result<(), WrongWorld> {
        let handle = self.handle(body)?;
        self.backend.bodies[handle].set_translation(vector(value), true);
        Ok(())
    }
    /// Current rigid-body pose in world coordinates.
    pub fn pose(&self, body: BodyId) -> Result<Pose3, WrongWorld> {
        let b = &self.backend.bodies[self.handle(body)?];
        Ok(Pose3::new(
            b.translation().to_array().into(),
            Rotation3::try_from(b.rotation().to_array()).expect("unit body rotation"),
        ))
    }
    pub fn impulse(&mut self, body: BodyId, impulse: Vec3) -> Result<(), WrongWorld> {
        let handle = self.handle(body)?;
        self.backend.bodies[handle].apply_impulse(vector(impulse), true);
        Ok(())
    }
    pub fn synchronize(&mut self) {
        self.backend.detect_collisions(&(), &());
    }
    pub fn step(&mut self) {
        self.backend.step();
    }
    pub fn ray(&self, origin: Vec3, delta: Vec3, bodies: Bodies) -> Option<Hit> {
        let ray = r::Ray::new(vector(origin), vector(delta));
        let (id, hit) = self
            .backend
            .cast_ray_and_get_normal(&ray, 1., true, bodies.filter())?;
        Some(Hit {
            collider: ColliderId {
                world: self.id,
                handle: id,
            },
            fraction: hit.time_of_impact,
            normal: hit.normal.to_array().into(),
            point: origin + delta * hit.time_of_impact,
        })
    }
    /// Sweep a world-axis-aligned box along a displacement in world coordinates.
    pub fn sweep_box(&self, bounds: Aabb3, delta: Vec3, bodies: Bodies) -> Option<Hit> {
        let options = ShapeCastOptions {
            max_time_of_impact: 1.,
            stop_at_penetration: false,
            ..Default::default()
        };
        let (id, hit) = self.backend.cast_shape(
            &r::Pose::from_translation(vector(bounds.center())),
            vector(delta),
            &r::Cuboid::new(vector(bounds.half_extents())),
            options,
            bodies.filter(),
        )?;
        Some(Hit {
            collider: ColliderId {
                world: self.id,
                handle: id,
            },
            fraction: hit.time_of_impact,
            normal: hit.normal1.to_array().into(),
            point: hit.witness1.to_array().into(),
        })
    }
    /// Conservative broad-phase test over the entire translation, including both endpoints.
    pub fn box_path_may_hit(&self, bounds: Aabb3, delta: Vec3, bodies: Bodies) -> bool {
        let a = vector(bounds.center());
        let b = a + vector(delta);
        let half = vector(bounds.half_extents());
        let swept = r::Aabb::new(a.min(b) - half, a.max(b) + half);
        self.backend
            .intersect_aabb_conservative(swept, bodies.filter())
            .next()
            .is_some()
    }
    /// Test a world-axis-aligned box against the selected colliders.
    pub fn overlaps_box(&self, bounds: Aabb3, bodies: Bodies) -> bool {
        self.backend
            .intersect_shape(
                r::Pose::from_translation(vector(bounds.center())),
                &r::Cuboid::new(vector(bounds.half_extents())),
                bodies.filter(),
            )
            .next()
            .is_some()
    }
}
