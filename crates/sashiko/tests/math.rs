use sashiko::math::{Rotation3, Vec2, Vec3};
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

fn close(actual: Vec3, expected: Vec3) {
    assert!(
        (actual - expected).length() < 1e-5,
        "{actual:?} vs {expected:?}"
    );
}

#[test]
fn directions_and_recorded_arrays_round_trip() {
    let movement: Vec2 = serde_json::from_str("[3,4]").unwrap();
    assert_eq!(movement.normalize_or_zero(), Vec2::new(0.6, 0.8));
    assert_eq!(movement.distance_squared(Vec2::ZERO), 25.);
    assert_eq!(serde_json::to_string(&movement).unwrap(), "[3.0,4.0]");
    assert_eq!(Vec3::ZERO.normalize_or_zero(), Vec3::ZERO);
    assert_eq!(Vec2::new(f32::INFINITY, 0.).normalize_or_zero(), Vec2::ZERO);
    assert!(serde_json::from_str::<Rotation3>("[0,0,0,2]").is_err());
    assert!(Rotation3::try_from([f32::NAN, 0., 0., 1.]).is_err());
}

#[test]
fn heading_on_a_slope_uses_the_ground_normal_and_projected_forward() {
    let up = Vec3::new(0., 2., 2.);
    let rotation = Rotation3::from_forward_up(Vec3::X + up, up).unwrap();
    close(rotation.rotate_vector(Vec3::Y), up.normalize_or_zero());
    close(rotation.rotate_vector(Vec3::Z), Vec3::X);
    assert!(Rotation3::from_forward_up(Vec3::ZERO, up).is_err());
    assert!(Rotation3::from_forward_up(up * -2., up).is_err());
    assert!(Rotation3::from_axis_angle(Vec3::ZERO, 0.).is_err());
}

#[test]
fn rotations_compose_and_interpolate_across_the_shortest_arc() {
    let start = Rotation3::IDENTITY;
    // The negative quaternion encodes the same quarter turn about +Y.
    let quarter = Rotation3::from_axis_angle(Vec3::Y, FRAC_PI_2).unwrap();
    let xyzw: [f32; 4] = quarter.into();
    let end = Rotation3::try_from(xyzw.map(|v| -v)).unwrap();
    assert_eq!(start.slerp(end, 0.).unwrap(), start);
    assert_eq!(start.slerp(end, 1.).unwrap(), end);
    close(end.rotate_vector(Vec3::Z), Vec3::X);
    close(
        start.slerp(end, 0.5).unwrap().rotate_vector(Vec3::Z),
        Vec3::new(FRAC_PI_4.sin(), 0., FRAC_PI_4.cos()),
    );
    assert!(start.slerp(end, f32::NAN).is_err());
    let pitch = Rotation3::from_axis_angle(Vec3::X, FRAC_PI_2).unwrap();
    close(quarter.compose(pitch).rotate_vector(Vec3::Z), -Vec3::Y);
}

#[cfg(feature = "rendering")]
#[test]
fn local_and_render_transforms_agree_on_mirrored_scaled_points() {
    use sashiko::{math::LocalTransform, view::render_transform};
    let local = LocalTransform::new(
        Vec3::new(4., 5., 6.),
        Rotation3::from_axis_angle(Vec3::Y, FRAC_PI_2).unwrap(),
        Vec3::new(2., 3., -4.),
    );
    close(local.transform_vector(Vec3::Z), Vec3::new(-4., 0., 0.));
    let point = local.transform_point(Vec3::Z);
    close(point, Vec3::new(0., 5., 6.));
    close(
        render_transform(local)
            .unwrap()
            .transform_point(Vec3::Z.to_array())
            .into(),
        point,
    );
    let singular = LocalTransform {
        scale: Vec3::ZERO,
        ..local
    };
    assert_eq!(singular.transform_point(Vec3::ONE), local.translation);
    assert!(render_transform(singular).is_err());
}
