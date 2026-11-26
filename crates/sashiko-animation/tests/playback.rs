use glam::DVec3;
use sashiko_animation::{
    AnimationError, Clip, LocalPose, Marker, Playback, PoseWorkspace, Skeleton,
};
use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

fn rate() -> NonZeroU32 {
    NonZeroU32::new(1).unwrap()
}
fn local(translation: DVec3) -> LocalPose {
    LocalPose::new(translation.as_vec3().to_array(), [0., 0., 0., 1.], [1.; 3])
}

fn walking(rig: Arc<Skeleton>, looping: bool) -> Clip {
    Clip::new(
        rig,
        vec![local(DVec3::ZERO), local(DVec3::X)],
        rate(),
        looping,
        vec![
            Marker { frame: 1, id: 11 },
            Marker { frame: 0, id: 10 },
            Marker { frame: 1, id: 12 },
        ],
        vec![[0.; 3], [1., 0., 0.], [2., 0., 0.]],
    )
    .unwrap()
}

#[test]
fn retry_across_loops_preserves_time_and_marker_order() {
    let rig = Skeleton::new(vec![None]).unwrap();
    let clip = walking(rig.clone(), true);
    let mut player = Playback::new(&clip, rate());
    let mut short = [99; 5];
    assert_eq!(
        player.advance_to(4, &mut short).unwrap_err(),
        AnimationError::EventCapacity { required: 6 }
    );
    assert_eq!(short, [99; 5]);
    let mut events = [0; 6];
    let result = player.advance_to(4, &mut events).unwrap();
    assert_eq!(result.translation, [4., 0., 0.]);
    assert_eq!(events, [11, 12, 10, 11, 12, 10]);
    let mut workspace = PoseWorkspace::all(&rig);
    for _ in 0..2 {
        player.evaluate_pose(&mut workspace).unwrap();
        assert_eq!(workspace.joint(0).unwrap()[3], [0., 0., 0., 1.]);
    }
    assert_eq!(player.advance_to(4, &mut []).unwrap().event_count, 0);
    assert!(player.advance_to(3, &mut []).is_err());
}

#[test]
fn single_shot_holds_with_fractional_clock_and_no_repeated_events() {
    let rig = Skeleton::new(vec![None]).unwrap();
    let clip = walking(rig.clone(), false);
    let mut player = Playback::new(&clip, NonZeroU32::new(2).unwrap());
    let first = player.advance_to(1, &mut []).unwrap();
    assert_eq!(first.translation, [0.5, 0., 0.]);
    let mut events = [0; 2];
    let last = player.advance_to(u64::MAX, &mut events).unwrap();
    assert_eq!(last.translation, [1.5, 0., 0.]);
    assert_eq!(events, [11, 12]);
    let mut out = PoseWorkspace::all(&rig);
    player.evaluate_pose(&mut out).unwrap();
    assert_eq!(out.joint(0).unwrap()[3], [1., 0., 0., 1.]);
}

#[test]
fn turning_parent_and_partial_pose_agree_through_transition() {
    let rig = Skeleton::new(vec![Some(2), Some(2), None]).unwrap();
    let start = [local(DVec3::X), local(DVec3::Y), local(DVec3::ZERO)];
    let mut end = start;
    end[2] = LocalPose::new([0.; 3], [0., 0., 1., 0.], [1.; 3]);
    let make = |poses: [LocalPose; 3], id| {
        Clip::new(
            rig.clone(),
            poses.to_vec(),
            rate(),
            true,
            vec![Marker { frame: 0, id }],
            vec![[0.; 3], [id as f64, 0., 0.]],
        )
        .unwrap()
    };
    let source = make(start, 1);
    let target = make(end, 2);
    let mut player = Playback::new(&source, rate());
    player.advance_to(1, &mut [0]).unwrap();
    player
        .crossfade(&target, NonZeroU64::new(2).unwrap())
        .unwrap();
    let mut partial = PoseWorkspace::subset(&rig, &[0]).unwrap();
    let mut full = PoseWorkspace::all(&rig);
    let mut displacement = DVec3::ZERO;
    for (tick, expected) in [
        (1, DVec3::X),
        (2, DVec3::Y),
        (3, DVec3::NEG_X),
        (4, DVec3::NEG_X),
    ] {
        let mut events = [0];
        let result = player.advance_to(tick, &mut events).unwrap();
        if tick > 1 {
            assert_eq!(events, [2]);
        }
        displacement += DVec3::from_array(result.translation);
        player.evaluate_pose(&mut partial).unwrap();
        player.evaluate_pose(&mut full).unwrap();
        assert!(
            glam::Vec4::from_array(partial.joint(0).unwrap()[3])
                .truncate()
                .as_dvec3()
                .abs_diff_eq(expected, 1e-6)
        );
        assert_eq!(partial.joint(0), full.joint(0));
        assert!(partial.joint(1).is_none());
    }
    assert_eq!(displacement, DVec3::X * 5.);
    let mut grouped = Playback::new(&source, rate());
    grouped.advance_to(1, &mut [0]).unwrap();
    grouped
        .crossfade(&target, NonZeroU64::new(2).unwrap())
        .unwrap();
    assert_eq!(
        grouped.advance_to(4, &mut [0; 3]).unwrap().translation,
        displacement.to_array()
    );
    let wrong = Skeleton::new(vec![None, None, None]).unwrap();
    assert!(
        player
            .evaluate_pose(&mut PoseWorkspace::all(&wrong))
            .is_err()
    );
}

#[test]
fn synchronized_root_intervals_cross_the_seam_from_a_negative_phase() {
    let rig = Skeleton::new(vec![None]).unwrap();
    let looping = walking(rig.clone(), true);
    assert_eq!(
        looping.timeline().phase_displacement(-0.25, 1.25).unwrap(),
        [3., 0., 0.]
    );
    let once = walking(rig, false);
    assert_eq!(
        once.timeline().phase_displacement(-0.25, 1.25).unwrap(),
        [2., 0., 0.]
    );
}
