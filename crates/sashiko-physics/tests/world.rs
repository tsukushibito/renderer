use sashiko_math::{Aabb3, Pose3, Rotation3, Vec3};
use sashiko_physics::{Bodies, BodyKind, PhysicsWorld};

#[test]
fn synchronize_moves_the_query_without_advancing_time() {
    let mut world = PhysicsWorld::new(Vec3::ZERO, 1. / 60.);
    let (wall, id) = world.add_box(
        Pose3::from_translation(Vec3::X * 3.),
        Vec3::new(0.01, 1., 1.),
        BodyKind::Fixed,
    );
    world.synchronize();
    let cast = |world: &PhysicsWorld| {
        world
            .sweep_box(
                Aabb3::from_center_half_extents(Vec3::ZERO, Vec3::ONE * 0.1),
                Vec3::X * 10.,
                Bodies::Fixed,
            )
            .unwrap()
    };
    let hit = cast(&world);
    assert_eq!(hit.collider, id);
    assert!((hit.fraction - 0.289).abs() < 0.001);
    world.set_translation(wall, Vec3::X * 6.);
    world.synchronize();
    assert!((cast(&world).fraction - 0.589).abs() < 0.001);
    assert!(world.overlaps_box(
        Aabb3::from_center_half_extents(Vec3::X * 6., Vec3::ONE),
        Bodies::Fixed
    ));
}
#[test]
fn dynamic_box_lands_and_rests_on_fixed_floor() {
    let mut world = PhysicsWorld::new(Vec3::Y * (-9.81), 1. / 60.);
    world.add_box(
        Pose3::from_translation(-Vec3::Y),
        Vec3::new(4., 1., 4.),
        BodyKind::Fixed,
    );
    let (body, _) = world.add_box(
        Pose3::from_translation(Vec3::Y * 3.),
        Vec3::ONE * 0.5,
        BodyKind::Dynamic {
            lock_rotation: true,
        },
    );
    for _ in 0..240 {
        world.step();
    }
    let center = world.pose(body).translation;
    assert!((center.y() - 0.5).abs() < 0.02);
    for _ in 0..60 {
        world.step();
    }
    assert!((world.pose(body).translation - center).length() < 0.001);
}

#[test]
fn rotated_body_uses_local_extents_and_world_axis_queries() {
    let mut world = PhysicsWorld::new(Vec3::ZERO, 1. / 60.);
    let center = Vec3::new(3., 4., 5.);
    let rotation = Rotation3::from_axis_angle(Vec3::Z, std::f32::consts::FRAC_PI_2).unwrap();
    let (body, _) = world.add_box(
        Pose3::new(center, rotation),
        Vec3::new(2., 0.1, 0.1),
        BodyKind::Fixed,
    );
    world.synchronize();
    let pose = world.pose(body);
    assert!((pose.transform_point(Vec3::X) - (center + Vec3::Y)).length() < 1e-5);
    let probe = |offset| Aabb3::from_center_half_extents(center + offset, Vec3::ONE * 0.05);
    assert!(world.overlaps_box(probe(Vec3::Y), Bodies::All));
    assert!(!world.overlaps_box(probe(Vec3::X), Bodies::All));
    assert!(world.box_path_may_hit(probe(Vec3::X), -Vec3::X, Bodies::All));
    assert!(
        world
            .sweep_box(probe(Vec3::X), -Vec3::X, Bodies::All)
            .is_some()
    );
}
